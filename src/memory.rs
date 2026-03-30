// ══════════════════════════════════════════════════════════════════════════════
//  Structured Async Memory
//
//  Machine-readable memory layer that coexists with the human-editable
//  MEMORY.md and daily memory/{YYYY-MM-DD}.md files. Updated asynchronously
//  via the OnFinish hook — never blocks the main agent loop.
//
//  Inspired by DeerFlow's structured memory system but adapted to LingClaw's
//  single-session, file-based architecture.
// ══════════════════════════════════════════════════════════════════════════════

use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::{config::Config, providers};

// ── Schema ──────────────────────────────────────────────────────────────────

/// Top-level structured memory for a session.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub(crate) struct StructuredMemory {
    /// Free-form user context: preferences, background, language, etc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_context: Option<String>,
    /// Key facts and decisions the agent should remember across rounds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<MemoryFact>,
    /// Updated epoch seconds (set on write).
    #[serde(default)]
    pub updated_at: u64,
}

/// A single remembered fact/decision.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct MemoryFact {
    /// Short label, e.g. "preferred_language", "project_stack".
    pub key: String,
    /// The remembered content.
    pub value: String,
    /// When this fact was recorded (epoch seconds).
    #[serde(default)]
    pub recorded_at: u64,
}

const MEMORY_FILE_NAME: &str = "structured_memory.json";

/// Storage path for a session's structured memory.
fn memory_path(workspace: &Path) -> PathBuf {
    workspace.join(MEMORY_FILE_NAME)
}

// ── Storage ─────────────────────────────────────────────────────────────────

/// Load structured memory from disk. Returns default if missing/corrupt.
pub(crate) fn load_structured_memory(workspace: &Path) -> StructuredMemory {
    let path = memory_path(workspace);
    match std::fs::read_to_string(&path) {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => StructuredMemory::default(),
    }
}

/// Persist structured memory to disk atomically (temp + rename).
pub(crate) fn save_structured_memory(
    workspace: &Path,
    mem: &StructuredMemory,
) -> Result<(), String> {
    let path = memory_path(workspace);
    let tmp = workspace.join("structured_memory.json.tmp");
    let data = serde_json::to_string_pretty(mem).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, &data).map_err(|e| format!("write tmp: {e}"))?;

    #[cfg(windows)]
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("remove old: {e}"))?;
    }

    std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))
}

// ── Prompt injection ────────────────────────────────────────────────────────

/// Max characters for the structured memory block injected into the system prompt.
const MEMORY_INJECTION_CHAR_BUDGET: usize = 2_000;

/// Format structured memory for injection into the system prompt.
/// Returns `None` if the memory is empty.
pub(crate) fn format_memory_for_injection(mem: &StructuredMemory) -> Option<String> {
    if mem.user_context.is_none() && mem.facts.is_empty() {
        return None;
    }

    let mut lines = Vec::new();
    lines.push("## Structured Memory (auto-maintained)".to_string());

    if let Some(ref ctx) = mem.user_context {
        if !ctx.trim().is_empty() {
            lines.push(format!("**User context:** {}", ctx.trim()));
        }
    }

    if !mem.facts.is_empty() {
        lines.push("**Remembered facts:**".to_string());
        for fact in &mem.facts {
            lines.push(format!("- **{}**: {}", fact.key, fact.value));
        }
    }

    let result = lines.join("\n");
    if result.len() > MEMORY_INJECTION_CHAR_BUDGET {
        // Truncate at a safe boundary
        let truncated = crate::truncate(&result, MEMORY_INJECTION_CHAR_BUDGET);
        Some(format!("{truncated}\n*(memory truncated)*"))
    } else {
        Some(result)
    }
}

// ── Async update queue ──────────────────────────────────────────────────────

/// Payload sent to the background memory updater.
#[derive(Clone)]
struct MemoryUpdateRequest {
    workspace: PathBuf,
    model: String,
    /// Only user messages + final assistant response (no tool noise).
    conversation_excerpt: Vec<crate::ChatMessage>,
}

/// Debounced async memory update queue.
/// Receives update requests from the OnFinish hook and processes them
/// in the background with debounce to avoid excessive LLM calls.
pub(crate) struct MemoryUpdateQueue {
    tx: mpsc::UnboundedSender<MemoryUpdateRequest>,
}

impl MemoryUpdateQueue {
    /// Spawn the background updater task. Returns the queue handle.
    pub(crate) fn spawn(config: Config) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(memory_updater_loop(rx, config));
        Self { tx }
    }

    /// Enqueue a memory update request (non-blocking).
    pub(crate) fn enqueue(
        &self,
        workspace: PathBuf,
        model: String,
        conversation_excerpt: Vec<crate::ChatMessage>,
    ) {
        let req = MemoryUpdateRequest {
            workspace,
            model,
            conversation_excerpt,
        };
        let _ = self.tx.send(req);
    }
}

/// Debounce duration: wait this long after the last request before processing.
const DEBOUNCE_DURATION: Duration = Duration::from_secs(3);

/// Background loop that processes memory update requests with debounce.
async fn memory_updater_loop(mut rx: mpsc::UnboundedReceiver<MemoryUpdateRequest>, config: Config) {
    let memory_timeout = config.tool_timeout.max(Duration::from_secs(30));
    let http = Client::builder()
        .timeout(memory_timeout)
        .build()
        .unwrap_or_else(|_| Client::new());
    let mut pending: Option<MemoryUpdateRequest> = None;

    loop {
        if let Some(req) = pending.take() {
            // Debounce: wait for more requests or timeout
            let final_req = tokio::select! {
                next = rx.recv() => {
                    match next {
                        Some(newer) => {
                            // Replace with newer request, restart debounce
                            pending = Some(newer);
                            continue;
                        }
                        None => return, // channel closed
                    }
                }
                _ = tokio::time::sleep(DEBOUNCE_DURATION) => req,
            };

            // Process the debounced request with a timeout guard
            match tokio::time::timeout(
                memory_timeout,
                process_memory_update(&final_req, &config, &http),
            )
            .await
            {
                Ok(Err(e)) => eprintln!("memory update error: {e}"),
                Err(_) => eprintln!("memory update timed out"),
                Ok(Ok(())) => {}
            }
        } else {
            // Wait for next request
            match rx.recv().await {
                Some(req) => {
                    pending = Some(req);
                }
                None => return, // channel closed
            }
        }
    }
}

/// Core memory update: call LLM to extract memory from conversation,
/// merge with existing memory, and persist.
async fn process_memory_update(
    req: &MemoryUpdateRequest,
    config: &Config,
    http: &Client,
) -> Result<(), String> {
    let existing = load_structured_memory(&req.workspace);

    // Build conversation excerpt text
    let excerpt = build_conversation_excerpt(&req.conversation_excerpt);
    if excerpt.trim().is_empty() {
        return Ok(());
    }

    // Build existing memory context
    let existing_json =
        serde_json::to_string_pretty(&existing).unwrap_or_else(|_| "{}".to_string());

    let system_prompt = format!(
        r#"You are a memory extraction assistant. Your task is to analyze a conversation and update the user's structured memory.

Current memory state:
```json
{existing_json}
```

Instructions:
1. Extract any new user preferences, key decisions, project context, or important facts from the conversation.
2. Return the COMPLETE updated memory — include all facts that should be kept. Omit any facts that are clearly outdated or contradicted by the conversation.
3. Update user_context if the user reveals preferences, background, or working style. Set to null to clear it.
4. Return ONLY valid JSON matching this schema (no markdown fences, no explanation):

{{"user_context": "string or null", "facts": [{{"key": "short_label", "value": "content"}}]}}

IMPORTANT: The returned facts list REPLACES the existing facts entirely. Include ALL facts that should persist, not just new ones.
If there is nothing meaningful to extract, return the existing memory unchanged.
Keep facts concise. Do not store ephemeral task details — only persistent knowledge."#
    );

    let messages = vec![
        crate::ChatMessage {
            role: "system".into(),
            content: Some(system_prompt),
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        crate::ChatMessage {
            role: "user".into(),
            content: Some(format!("Conversation to analyze:\n\n{excerpt}")),
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
    ];

    let resolved = config.resolve_model(&req.model);
    let response = providers::call_llm_simple(http, &resolved, &messages)
        .await
        .map_err(|e| format!("LLM call failed: {e}"))?;

    let response = response.trim();
    if response.is_empty() {
        return Ok(());
    }

    // Strip markdown fences if present
    let json_str = strip_json_fences(response);

    // Parse as raw Value first so we can distinguish "field absent" from
    // "field explicitly null" — prevents silent data loss when the LLM
    // returns incomplete JSON.
    let raw: serde_json::Value =
        serde_json::from_str(json_str).map_err(|e| format!("parse LLM response: {e}"))?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut merged = existing;

    // Only touch user_context when the key is actually present in the response.
    // null → clear, string → update, absent → preserve existing.
    if raw.get("user_context").is_some() {
        merged.user_context = raw["user_context"].as_str().map(|s| s.to_string());
    }

    // Only replace facts when the key is actually present in the response.
    // Absent → preserve existing facts unchanged.
    if let Some(facts_val) = raw.get("facts") {
        if let Some(facts_arr) = facts_val.as_array() {
            let mut new_facts = Vec::new();
            for fv in facts_arr {
                let key = fv
                    .get("key")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let value = fv
                    .get("value")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if key.is_empty() || value.is_empty() {
                    continue;
                }
                let recorded_at = merged
                    .facts
                    .iter()
                    .find(|f| f.key == key && f.value == value)
                    .map(|f| f.recorded_at)
                    .unwrap_or(now);
                new_facts.push(MemoryFact {
                    key,
                    value,
                    recorded_at,
                });
            }
            merged.facts = new_facts;
        }
    }

    merged.updated_at = now;

    // Cap total facts to prevent unbounded growth
    const MAX_FACTS: usize = 50;
    if merged.facts.len() > MAX_FACTS {
        // Keep the most recently recorded facts
        merged
            .facts
            .sort_by(|a, b| b.recorded_at.cmp(&a.recorded_at));
        merged.facts.truncate(MAX_FACTS);
    }

    save_structured_memory(&req.workspace, &merged)
}

/// Build conversation excerpt from messages, filtering to only user and
/// final assistant content (no tool calls, no tool results).
fn build_conversation_excerpt(messages: &[crate::ChatMessage]) -> String {
    let mut lines = Vec::new();
    for msg in messages {
        match msg.role.as_str() {
            "user" => {
                if let Some(content) = msg.content.as_deref() {
                    if !content.is_empty() {
                        lines.push(format!("User: {content}"));
                    }
                }
            }
            "assistant" => {
                if let Some(content) = msg.content.as_deref() {
                    // Skip auto-generated compression summaries — they are
                    // synthetic, not real user/assistant interaction.
                    if !content.is_empty()
                        && !content.starts_with("## Context Summary (auto-generated)")
                    {
                        lines.push(format!("Assistant: {content}"));
                    }
                }
                // Skip tool_calls — we don't want tool noise in memory
            }
            _ => {} // skip tool results, system
        }
    }
    lines.join("\n\n")
}

/// Strip ```json ... ``` fences from LLM output.
fn strip_json_fences(s: &str) -> &str {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("```json") {
        if let Some(inner) = rest.strip_suffix("```") {
            return inner.trim();
        }
    }
    if let Some(rest) = s.strip_prefix("```") {
        if let Some(inner) = rest.strip_suffix("```") {
            return inner.trim();
        }
    }
    s
}

// ── Memory status ───────────────────────────────────────────────────────────

/// Build a human-readable status summary of structured memory.
pub(crate) fn memory_status(workspace: &Path) -> String {
    let mem = load_structured_memory(workspace);
    if mem.user_context.is_none() && mem.facts.is_empty() {
        return "Structured memory: empty (will populate after first conversation)".to_string();
    }

    let mut lines = Vec::new();
    lines.push(format!("**Structured Memory** ({} facts)", mem.facts.len()));

    if let Some(ref ctx) = mem.user_context {
        let display = if ctx.len() > 100 {
            let end = (0..=100)
                .rev()
                .find(|&i| ctx.is_char_boundary(i))
                .unwrap_or(0);
            format!("{}…", &ctx[..end])
        } else {
            ctx.clone()
        };
        lines.push(format!("User context: {display}"));
    }

    if !mem.facts.is_empty() {
        lines.push("Facts:".to_string());
        for (i, fact) in mem.facts.iter().enumerate() {
            let display = if fact.value.len() > 80 {
                let end = (0..=80)
                    .rev()
                    .find(|&i| fact.value.is_char_boundary(i))
                    .unwrap_or(0);
                format!("{}…", &fact.value[..end])
            } else {
                fact.value.clone()
            };
            lines.push(format!("  {}. **{}**: {display}", i + 1, fact.key));
        }
    }

    if mem.updated_at > 0 {
        let age_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .saturating_sub(mem.updated_at);
        let age_label = if age_secs < 60 {
            "just now".to_string()
        } else if age_secs < 3600 {
            format!("{}m ago", age_secs / 60)
        } else if age_secs < 86400 {
            format!("{}h ago", age_secs / 3600)
        } else {
            format!("{}d ago", age_secs / 86400)
        };
        lines.push(format!("Last updated: {age_label}"));
    }

    lines.join("\n")
}

// ══════════════════════════════════════════════════════════════════════════════
//  Tests
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
#[path = "tests/memory_tests.rs"]
mod tests;
