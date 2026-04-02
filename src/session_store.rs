use serde_json::json;
#[cfg(test)]
use std::collections::HashSet;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use crate::{
    Config, Session, config_dir_path, context_input_budget_for_model, estimate_tokens_for_provider,
    format_token_count, format_usage_block, prompts,
};

use super::{AppState, ChatMessage};

pub(crate) fn sessions_dir() -> PathBuf {
    let dir = config_dir_path()
        .unwrap_or_else(|| PathBuf::from(".lingclaw"))
        .join("sessions");
    std::fs::create_dir_all(&dir).ok();
    dir
}

pub(crate) async fn save_session_to_disk(session: &Session) -> Result<(), String> {
    let path = sessions_dir().join(format!("{}.json", session.id));
    let tmp_path = sessions_dir().join(format!("{}.json.tmp", session.id));
    let mut session = session.clone();
    sanitize_session_messages(&mut session.messages);
    let data = serde_json::to_string_pretty(&session).map_err(|e| e.to_string())?;
    tokio::fs::write(&tmp_path, data)
        .await
        .map_err(|e| e.to_string())?;

    #[cfg(windows)]
    if tokio::fs::try_exists(&path)
        .await
        .map_err(|e| e.to_string())?
    {
        tokio::fs::remove_file(&path)
            .await
            .map_err(|e| e.to_string())?;
    }

    if let Err(e) = tokio::fs::rename(&tmp_path, &path).await {
        // Best-effort cleanup of the orphaned temp file.
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return Err(e.to_string());
    }
    Ok(())
}

pub(crate) fn sanitize_session_messages(messages: &mut Vec<ChatMessage>) {
    messages.retain(|message| !message.is_empty_assistant_message());
}

pub(crate) fn trim_incomplete_tool_calls(messages: &mut Vec<ChatMessage>) {
    let ast_idx = messages.iter().rposition(|m| {
        m.role == "assistant" && m.tool_calls.as_ref().is_some_and(|tc| !tc.is_empty())
    });
    let Some(idx) = ast_idx else {
        sanitize_session_messages(messages);
        return;
    };
    let expected = messages[idx]
        .tool_calls
        .as_ref()
        .map(|tc| tc.len())
        .unwrap_or(0);
    let actual = messages[idx + 1..]
        .iter()
        .filter(|m| m.role == "tool")
        .count();
    if actual < expected {
        let removed = messages.len() - idx;
        eprintln!(
            "trim_incomplete_tool_calls: removed {removed} trailing messages (expected {expected} tool results, found {actual})"
        );
        messages.truncate(idx);
    }

    sanitize_session_messages(messages);
}

pub(crate) fn normalize_session(session: &mut Session) {
    super::migrate_session(session);
    trim_incomplete_tool_calls(&mut session.messages);
}

pub(crate) fn load_session_snapshot_from_path(path: &Path) -> Option<Session> {
    let data = std::fs::read_to_string(path).ok()?;
    let mut session: Session = serde_json::from_str(&data).ok()?;
    normalize_session(&mut session);
    Some(session)
}

pub(crate) fn load_session_from_disk(id: &str) -> Option<Session> {
    if id.contains('/') || id.contains('\\') || id.contains("..") {
        return None;
    }
    let path = sessions_dir().join(format!("{id}.json"));
    let tmp_path = sessions_dir().join(format!("{id}.json.tmp"));
    // Load from primary, fall back to .tmp, or pick the newer of the two.
    // Crash scenarios: (a) primary missing, tmp exists → use tmp;
    // (b) both exist, tmp is newer → use tmp (crash after tmp write, before rename);
    // (c) both exist, primary is newer → use primary (normal case).
    let primary = load_session_snapshot_from_path(&path);
    let tmp_available = tmp_path.exists();
    let mut session = match (primary, tmp_available) {
        (Some(p), false) => p,
        (None, true) => {
            eprintln!(
                "Warning: recovering session '{id}' from .tmp file (primary missing after crash)"
            );
            load_session_snapshot_from_path(&tmp_path)?
        }
        (Some(p), true) => {
            // Both exist — pick the one with the later mtime.
            let primary_mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            let tmp_mtime = std::fs::metadata(&tmp_path).and_then(|m| m.modified()).ok();
            if tmp_mtime >= primary_mtime {
                eprintln!(
                    "Warning: recovering session '{id}' from newer .tmp file (crash during save)"
                );
                load_session_snapshot_from_path(&tmp_path).unwrap_or(p)
            } else {
                // tmp is stale leftover — clean it up.
                let _ = std::fs::remove_file(&tmp_path);
                p
            }
        }
        (None, false) => return None,
    };
    session.workspace = super::session_workspace_path(&session.id);
    std::fs::create_dir_all(&session.workspace).ok();
    prompts::ensure_session_workspace(&session.workspace);
    Some(session)
}

pub(crate) fn refresh_session_system_prompt(state: &AppState, session: &mut Session) {
    let model = session.effective_model(&state.config.model).to_string();
    let sys = super::build_system_prompt(
        &state.config,
        &session.workspace,
        &model,
        &session.disabled_system_skills,
    );
    if let Some(first) = session.messages.first_mut()
        && first.role == "system"
    {
        *first = sys;
    }
}

#[cfg(test)]
pub(crate) fn sanitized_non_system_message_count(session: &Session) -> usize {
    let mut normalized = session.clone();
    normalize_session(&mut normalized);
    normalized
        .messages
        .iter()
        .filter(|message| message.role != "system")
        .count()
}

#[cfg(test)]
pub(crate) fn list_saved_session_summaries_in_dir(dir: &Path) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Some(session) = load_session_snapshot_from_path(&path) {
                    let msg_count = sanitized_non_system_message_count(&session);
                    out.push(json!({
                        "id": session.id,
                        "name": session.name,
                        "messages": msg_count,
                        "created_at": session.created_at,
                        "updated_at": session.updated_at,
                        "corrupt": false,
                    }));
                } else if let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) {
                    out.push(json!({
                        "id": id,
                        "name": "[Corrupt Session]",
                        "messages": 0,
                        "created_at": 0,
                        "updated_at": 0,
                        "corrupt": true,
                    }));
                }
            }
        }
    }
    out.sort_by(|a, b| {
        let b_ts = b["updated_at"].as_u64().unwrap_or(0);
        let a_ts = a["updated_at"].as_u64().unwrap_or(0);
        b_ts.cmp(&a_ts)
    });
    out
}

#[cfg(test)]
pub(crate) fn recoverable_session_ids_from_summaries(
    summaries: &[serde_json::Value],
) -> Vec<String> {
    summaries
        .iter()
        .filter(|summary| {
            summary["corrupt"].as_bool() != Some(true)
                && summary["messages"].as_u64().unwrap_or(0) > 0
        })
        .filter_map(|summary| summary["id"].as_str().map(str::to_string))
        .collect()
}

#[cfg(test)]
pub(crate) fn list_saved_session_ids_in_dir(dir: &Path) -> HashSet<String> {
    let mut ids = HashSet::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                ids.insert(stem.to_string());
            }
        }
    }
    ids
}

pub(crate) fn build_history_payload(session: &Session) -> serde_json::Value {
    let mut msgs = Vec::new();
    for msg in &session.messages {
        match msg.role.as_str() {
            "system" => {}
            "user" => {
                if let Some(c) = &msg.content {
                    msgs.push(json!({"role":"user","content":c,"timestamp":msg.timestamp}));
                }
            }
            "assistant" => {
                if let Some(c) = &msg.content
                    && !c.is_empty()
                {
                    msgs.push(json!({"role":"assistant","content":c,"timestamp":msg.timestamp}));
                }
                if let Some(tcs) = &msg.tool_calls
                    && session.show_tools
                {
                    for tc in tcs {
                        msgs.push(json!({"role":"tool_call","name":tc.function.name,"arguments":tc.function.arguments,"id":tc.id}));
                    }
                }
            }
            "tool" => {
                if session.show_tools
                    && let Some(c) = &msg.content
                {
                    msgs.push(json!({"role":"tool_result","result":c,"id":msg.tool_call_id.as_deref().unwrap_or("")}));
                }
            }
            _ => {}
        }
    }
    json!({"type":"history","messages":msgs})
}

pub(crate) fn build_view_state_payload(session: &Session) -> serde_json::Value {
    json!({
        "type": "view_state",
        "show_tools": session.show_tools,
        "show_reasoning": session.show_reasoning,
        "show_react": session.show_react,
    })
}

#[cfg(test)]
pub(crate) fn resolve_session_target(
    target: &str,
    known_ids: &HashSet<String>,
) -> Result<String, String> {
    if known_ids.contains(target) {
        return Ok(target.to_string());
    }

    let mut matches: Vec<&String> = known_ids
        .iter()
        .filter(|id| id.starts_with(target))
        .collect();
    matches.sort_unstable();
    match matches.len() {
        0 => Err(format!("Session '{}' not found.", target)),
        1 => Ok(matches[0].to_string()),
        _ => Err(format!(
            "Session '{}' is ambiguous. Use a longer ID.",
            target
        )),
    }
}

#[cfg(test)]
pub(crate) fn build_active_session_lines(
    sessions: &HashMap<String, Session>,
    active_ids: &HashSet<String>,
    config: &Config,
) -> Vec<String> {
    let mut ids: Vec<&String> = active_ids.iter().collect();
    ids.sort_unstable();

    ids.into_iter()
        .filter_map(|id| {
            let session = sessions.get(id)?;
            let model = session.effective_model(&config.model).to_string();
            let ctx_limit = config.context_limit_for_model(&model);
            let input_budget = context_input_budget_for_model(config, &model);
            let resolved = config.resolve_model(&model);
            let estimated = estimate_tokens_for_provider(resolved.provider, &session.messages);
            let mt_str = resolved
                .max_tokens
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".into());
            Some(format!(
                "  {id}  {}\n    model: {model}  context_est: {estimated}/{input_budget} (limit {ctx_limit})  token_usage_source: in={} out={}  max_tokens: {mt_str}  [active]",
                session.name,
                session.input_token_source,
                session.output_token_source,
            ))
        })
        .collect()
}

pub(crate) fn build_session_status(session: &Session, config: &Config) -> String {
    let model_ref = session.effective_model(&config.model);
    let canonical_model = config
        .canonical_model_ref(model_ref)
        .unwrap_or_else(|_| model_ref.to_string());
    let resolved = config.resolve_model(&canonical_model);
    let ctx_limit = config.context_limit_for_model(&canonical_model);
    let input_budget = context_input_budget_for_model(config, &canonical_model);
    let estimated_tokens = estimate_tokens_for_provider(resolved.provider, &session.messages);
    let model_max_tokens = resolved
        .max_tokens
        .map(format_token_count)
        .unwrap_or_else(|| "-".into());

    format!(
        "agent: LingClaw\n\
         model: {canonical_model}\n\
         resolved_provider: {}\n\
         resolved_api_base: {}\n\
         resolved_model_id: {}\n\
         max_tokens: {model_max_tokens}\n\
         context_est: {}/{} (limit {})\n\
         token_usage_source: input={} output={}\n\
         think: {}\n\
         react: {}\n\
         tools: {}\n\
         reasoning: {}",
        resolved.provider.label(),
        resolved.api_base,
        resolved.model_id,
        format_token_count(estimated_tokens as u64),
        format_token_count(input_budget as u64),
        format_token_count(ctx_limit as u64),
        session.input_token_source,
        session.output_token_source,
        session.think_level,
        if session.show_react { "on" } else { "off" },
        if session.show_tools { "on" } else { "off" },
        if session.show_reasoning { "on" } else { "off" },
    )
}

pub(crate) fn build_session_usage(session: &Session) -> String {
    let (today_input_tokens, today_output_tokens) = super::current_daily_token_usage(session);
    let total_input_tokens = session.input_tokens;
    let total_output_tokens = session.output_tokens;
    let total_tokens = session.input_tokens.saturating_add(session.output_tokens);

    format!(
        "today_usage_est: # 当前会话今日 token 使用估算\n\tinput_tokens: {}\n\toutput_tokens: {}\n\n\
total_usage_est: # 当前会话累计 token 使用估算\n\ttotal_tokens: {}\n\ttotal_input_tokens: {}\n\ttotal_output_tokens: {}",
        format_token_count(today_input_tokens),
        format_token_count(today_output_tokens),
        format_token_count(total_tokens),
        format_token_count(total_input_tokens),
        format_token_count(total_output_tokens),
    )
}

pub(crate) fn build_global_today_usage(sessions: &HashMap<String, Session>) -> String {
    let (global_today_input_tokens, global_today_output_tokens) =
        super::accumulate_daily_token_usage(sessions.values());
    format_usage_block(
        "global_today_usage_est",
        "所有会话今日 token 使用估算",
        global_today_input_tokens,
        global_today_output_tokens,
    )
}

pub(crate) fn build_usage_report(session: &Session, global_today_usage: &str) -> String {
    format!("{}\n\n{}", build_session_usage(session), global_today_usage)
}
