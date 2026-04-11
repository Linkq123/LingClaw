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
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::{fs::OpenOptions, io::AsyncWriteExt};

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
const MEMORY_AUDIT_FILE_NAME: &str = "structured_memory.audit.jsonl";
/// Max audit file size before rotation (trim oldest entries).
const MEMORY_AUDIT_MAX_BYTES: u64 = 256_000;

#[derive(Clone, Debug, Default)]
pub(crate) struct MemoryQueueStatusSnapshot {
    pub state: String,
    pub enqueued: u64,
    pub replaced_during_debounce: u64,
    pub started: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub timed_out: u64,
    pub last_model: Option<String>,
    pub last_excerpt_chars: usize,
    pub last_duration_ms: u64,
    pub last_error: Option<String>,
    pub last_enqueued_at: u64,
    pub last_started_at: u64,
    pub last_finished_at: u64,
    pub last_success_at: u64,
    pub last_failure_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct MemoryAuditRecord {
    timestamp: u64,
    model: String,
    status: String,
    excerpt_chars: usize,
    duration_ms: u64,
    facts_before: usize,
    facts_after: usize,
    had_user_context_before: bool,
    had_user_context_after: bool,
    changed: bool,
    error: Option<String>,
}

struct MemoryAuditBaseline {
    excerpt_chars: usize,
    facts_before: usize,
    had_user_context_before: bool,
}

#[derive(Clone, Debug)]
struct MemoryProcessStats {
    excerpt_chars: usize,
    facts_before: usize,
    facts_after: usize,
    had_user_context_before: bool,
    had_user_context_after: bool,
    changed: bool,
}

type SharedMemoryQueueStatus = Arc<Mutex<MemoryQueueStatusSnapshot>>;

/// Storage path for a session's structured memory.
fn memory_path(workspace: &Path) -> PathBuf {
    workspace.join(MEMORY_FILE_NAME)
}

fn memory_audit_path(workspace: &Path) -> PathBuf {
    workspace.join(MEMORY_AUDIT_FILE_NAME)
}

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn format_relative_age(age_secs: u64) -> String {
    if age_secs < 60 {
        "just now".to_string()
    } else if age_secs < 3600 {
        format!("{}m ago", age_secs / 60)
    } else if age_secs < 86400 {
        format!("{}h ago", age_secs / 3600)
    } else {
        format!("{}d ago", age_secs / 86400)
    }
}

fn timestamp_label(ts: u64) -> Option<String> {
    if ts == 0 {
        return None;
    }
    Some(format_relative_age(now_epoch_secs().saturating_sub(ts)))
}

fn truncate_inline(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let cut = (0..=limit)
        .rev()
        .find(|&idx| text.is_char_boundary(idx))
        .unwrap_or(0);
    format!("{}…", &text[..cut])
}

fn with_queue_status<F>(status: &SharedMemoryQueueStatus, update: F)
where
    F: FnOnce(&mut MemoryQueueStatusSnapshot),
{
    if let Ok(mut guard) = status.lock() {
        update(&mut guard);
    }
}

fn build_audit_baseline(req: &MemoryUpdateRequest) -> MemoryAuditBaseline {
    let existing = load_structured_memory(&req.workspace);
    let excerpt = build_conversation_excerpt(&req.conversation_excerpt);
    MemoryAuditBaseline {
        excerpt_chars: excerpt.chars().count(),
        facts_before: existing.facts.len(),
        had_user_context_before: existing.user_context.is_some(),
    }
}

async fn append_memory_audit_record(workspace: &Path, record: &MemoryAuditRecord) {
    let serialized = match serde_json::to_string(record) {
        Ok(data) => data,
        Err(error) => {
            eprintln!("memory audit serialize error: {error}");
            return;
        }
    };

    let path = memory_audit_path(workspace);
    let tmp_path = workspace.join("structured_memory.audit.jsonl.tmp");

    // Recover .tmp left behind by a previous crash during rotation (Windows).
    if !tokio::fs::try_exists(&path).await.unwrap_or(true)
        && tokio::fs::try_exists(&tmp_path).await.unwrap_or(false)
    {
        let _ = tokio::fs::rename(&tmp_path, &path).await;
    }

    // Rotate if oversized: keep the most recent half of lines via tmp+rename.
    // On Windows, rename requires removing the destination first, leaving a
    // brief crash window; the recovery above handles that on the next call.
    if let Ok(meta) = tokio::fs::metadata(&path).await
        && meta.len() > MEMORY_AUDIT_MAX_BYTES
        && let Ok(data) = tokio::fs::read_to_string(&path).await
    {
        let lines: Vec<&str> = data.lines().collect();
        let keep_from = lines.len() / 2;
        let trimmed = lines[keep_from..].join("\n") + "\n";
        if tokio::fs::write(&tmp_path, &trimmed).await.is_ok() {
            #[cfg(windows)]
            let _ = tokio::fs::remove_file(&path).await;
            if tokio::fs::rename(&tmp_path, &path).await.is_err() {
                let _ = tokio::fs::write(&path, &trimmed).await;
                let _ = tokio::fs::remove_file(&tmp_path).await;
            }
        }
    }

    let mut file = match OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await
    {
        Ok(file) => file,
        Err(error) => {
            eprintln!("memory audit open error: {error}");
            return;
        }
    };

    if let Err(error) = file.write_all(format!("{serialized}\n").as_bytes()).await {
        eprintln!("memory audit write error: {error}");
    }
}

fn read_recent_memory_audit(workspace: &Path, limit: usize) -> Vec<MemoryAuditRecord> {
    if limit == 0 {
        return Vec::new();
    }

    let data = match std::fs::read_to_string(memory_audit_path(workspace)) {
        Ok(data) => data,
        Err(_) => return Vec::new(),
    };

    let mut records = Vec::new();
    for line in data.lines().rev() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(record) = serde_json::from_str::<MemoryAuditRecord>(line) {
            records.push(record);
            if records.len() == limit {
                break;
            }
        }
    }
    records.reverse();
    records
}

fn format_queue_status(snapshot: &MemoryQueueStatusSnapshot) -> String {
    let mut lines = Vec::new();
    lines.push("**Memory Updater**".to_string());
    lines.push(format!("State: {}", snapshot.state));
    lines.push(format!(
        "Attempts: enqueued {} | started {} | ok {} | failed {} | timed out {}",
        snapshot.enqueued,
        snapshot.started,
        snapshot.succeeded,
        snapshot.failed,
        snapshot.timed_out,
    ));
    if snapshot.replaced_during_debounce > 0 {
        lines.push(format!(
            "Debounce replacements: {}",
            snapshot.replaced_during_debounce
        ));
    }
    if let Some(model) = &snapshot.last_model {
        lines.push(format!("Last model: {model}"));
    }
    if snapshot.last_excerpt_chars > 0 {
        lines.push(format!(
            "Last excerpt: {} chars",
            snapshot.last_excerpt_chars
        ));
    }
    if snapshot.last_duration_ms > 0 {
        lines.push(format!("Last duration: {} ms", snapshot.last_duration_ms));
    }
    if let Some(label) = timestamp_label(snapshot.last_enqueued_at) {
        lines.push(format!("Last enqueued: {label}"));
    }
    if let Some(label) = timestamp_label(snapshot.last_started_at) {
        lines.push(format!("Last started: {label}"));
    }
    if let Some(label) = timestamp_label(snapshot.last_success_at) {
        lines.push(format!("Last success: {label}"));
    }
    if let Some(label) = timestamp_label(snapshot.last_failure_at) {
        lines.push(format!("Last failure: {label}"));
    }
    if let Some(error) = &snapshot.last_error {
        lines.push(format!("Last error: {}", truncate_inline(error, 160)));
    }
    lines.join("\n")
}

pub(crate) fn memory_runtime_status(queue: Option<&MemoryUpdateQueue>) -> String {
    match queue {
        Some(queue) => format_queue_status(&queue.status_snapshot()),
        None => "**Memory Updater**\nState: unavailable in this process".to_string(),
    }
}

pub(crate) fn memory_debug_status(workspace: &Path, queue: Option<&MemoryUpdateQueue>) -> String {
    let mut lines = vec![memory_runtime_status(queue)];
    let records = read_recent_memory_audit(workspace, 5);
    if records.is_empty() {
        lines.push("\nRecent audit entries: none".to_string());
        return lines.join("\n");
    }

    lines.push("\nRecent audit entries:".to_string());
    for record in records {
        let age = format_relative_age(now_epoch_secs().saturating_sub(record.timestamp));
        let mut line = format!(
            "- {} | {} | model {} | excerpt {} chars | facts {} -> {} | {} ms",
            age,
            record.status,
            record.model,
            record.excerpt_chars,
            record.facts_before,
            record.facts_after,
            record.duration_ms,
        );
        if let Some(error) = record.error {
            line.push_str(&format!(" | {}", truncate_inline(&error, 120)));
        }
        lines.push(line);
    }
    lines.join("\n")
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
///
/// When `current_query` is provided, facts are sorted by keyword relevance
/// to the current query (most relevant first), with recency as tiebreaker.
/// Without a query, facts are sorted by recency (newest first).
pub(crate) fn format_memory_for_injection(
    mem: &StructuredMemory,
    current_query: Option<&str>,
) -> Option<String> {
    if mem.user_context.is_none() && mem.facts.is_empty() {
        return None;
    }

    let mut lines = Vec::new();
    lines.push("## Structured Memory (auto-maintained)".to_string());

    if let Some(ref ctx) = mem.user_context
        && !ctx.trim().is_empty()
    {
        lines.push(format!("**User context:** {}", ctx.trim()));
    }

    if !mem.facts.is_empty() {
        let mut sorted_facts = mem.facts.clone();

        if let Some(query) = current_query {
            // Keyword-overlap scoring: tokenize query and score each fact.
            let query_tokens = crate::tokenize_for_matching(query);
            sorted_facts.sort_by(|a, b| {
                let score_a = fact_relevance_score(a, &query_tokens);
                let score_b = fact_relevance_score(b, &query_tokens);
                score_b
                    .cmp(&score_a)
                    .then(b.recorded_at.cmp(&a.recorded_at))
            });
        } else {
            sorted_facts.sort_by(|a, b| b.recorded_at.cmp(&a.recorded_at));
        }

        lines.push("**Remembered facts:**".to_string());
        for fact in &sorted_facts {
            lines.push(format!("- **{}**: {}", fact.key, fact.value));
        }
    }

    let result = lines.join("\n");
    if result.len() > MEMORY_INJECTION_CHAR_BUDGET {
        let truncated = crate::truncate(&result, MEMORY_INJECTION_CHAR_BUDGET);
        Some(format!("{truncated}\n*(memory truncated)*"))
    } else {
        Some(result)
    }
}

/// Score a memory fact's relevance to the query tokens.
/// Higher score = more relevant.
fn fact_relevance_score(fact: &MemoryFact, query_tokens: &[String]) -> usize {
    if query_tokens.is_empty() {
        return 0;
    }
    let fact_text = format!("{} {}", fact.key, fact.value).to_lowercase();
    query_tokens
        .iter()
        .filter(|token| fact_text.contains(token.as_str()))
        .count()
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

/// Max pending update requests. Beyond this, new requests replace the latest.
const MEMORY_QUEUE_CAPACITY: usize = 16;

/// Debounced async memory update queue.
/// Receives update requests from the OnFinish hook and processes them
/// in the background with debounce to avoid excessive LLM calls.
pub(crate) struct MemoryUpdateQueue {
    tx: mpsc::Sender<MemoryUpdateRequest>,
    status: SharedMemoryQueueStatus,
}

impl MemoryUpdateQueue {
    /// Spawn the background updater task. Returns the queue handle.
    pub(crate) fn spawn(config: Config) -> Self {
        let (tx, rx) = mpsc::channel(MEMORY_QUEUE_CAPACITY);
        let status = Arc::new(Mutex::new(MemoryQueueStatusSnapshot {
            state: "idle".to_string(),
            ..Default::default()
        }));
        tokio::spawn(memory_updater_loop(rx, config, status.clone()));
        Self { tx, status }
    }

    pub(crate) fn status_snapshot(&self) -> MemoryQueueStatusSnapshot {
        self.status
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
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
            model: model.clone(),
            conversation_excerpt,
        };
        if self.tx.try_send(req).is_err() {
            eprintln!("Warning: memory update queue is full, request dropped");
            return;
        }
        with_queue_status(&self.status, |snapshot| {
            snapshot.state = "pending".to_string();
            snapshot.enqueued += 1;
            snapshot.last_model = Some(model);
            snapshot.last_enqueued_at = now_epoch_secs();
        });
    }
}

/// Debounce duration: wait this long after the last request before processing.
const DEBOUNCE_DURATION: Duration = Duration::from_secs(3);

/// Background loop that processes memory update requests with debounce.
async fn memory_updater_loop(
    mut rx: mpsc::Receiver<MemoryUpdateRequest>,
    config: Config,
    status: SharedMemoryQueueStatus,
) {
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
                            with_queue_status(&status, |snapshot| {
                                snapshot.state = "pending".to_string();
                                snapshot.replaced_during_debounce += 1;
                                snapshot.last_model = Some(newer.model.clone());
                                snapshot.last_enqueued_at = now_epoch_secs();
                            });
                            pending = Some(newer);
                            continue;
                        }
                        None => return, // channel closed
                    }
                }
                _ = tokio::time::sleep(DEBOUNCE_DURATION) => req,
            };

            // Process the debounced request with a timeout guard
            let audit_baseline = build_audit_baseline(&final_req);
            let started_at = now_epoch_secs();
            let start = std::time::Instant::now();
            with_queue_status(&status, |snapshot| {
                snapshot.state = "running".to_string();
                snapshot.started += 1;
                snapshot.last_model = Some(final_req.model.clone());
                snapshot.last_excerpt_chars = audit_baseline.excerpt_chars;
                snapshot.last_started_at = started_at;
            });

            match tokio::time::timeout(
                memory_timeout,
                process_memory_update(&final_req, &config, &http),
            )
            .await
            {
                Ok(Err(error)) => {
                    let duration_ms = start.elapsed().as_millis() as u64;
                    let now = now_epoch_secs();
                    with_queue_status(&status, |snapshot| {
                        snapshot.state = "idle".to_string();
                        snapshot.failed += 1;
                        snapshot.last_duration_ms = duration_ms;
                        snapshot.last_error = Some(error.clone());
                        snapshot.last_failure_at = now;
                        snapshot.last_finished_at = now;
                    });
                    append_memory_audit_record(
                        &final_req.workspace,
                        &MemoryAuditRecord {
                            timestamp: now,
                            model: final_req.model.clone(),
                            status: "error".to_string(),
                            excerpt_chars: audit_baseline.excerpt_chars,
                            duration_ms,
                            facts_before: audit_baseline.facts_before,
                            facts_after: audit_baseline.facts_before,
                            had_user_context_before: audit_baseline.had_user_context_before,
                            had_user_context_after: audit_baseline.had_user_context_before,
                            changed: false,
                            error: Some(error.clone()),
                        },
                    )
                    .await;
                    eprintln!("memory update error: {error}");
                }
                Err(_) => {
                    let duration_ms = start.elapsed().as_millis() as u64;
                    let now = now_epoch_secs();
                    let error = "memory update timed out".to_string();
                    with_queue_status(&status, |snapshot| {
                        snapshot.state = "idle".to_string();
                        snapshot.timed_out += 1;
                        snapshot.last_duration_ms = duration_ms;
                        snapshot.last_error = Some(error.clone());
                        snapshot.last_failure_at = now;
                        snapshot.last_finished_at = now;
                    });
                    append_memory_audit_record(
                        &final_req.workspace,
                        &MemoryAuditRecord {
                            timestamp: now,
                            model: final_req.model.clone(),
                            status: "timeout".to_string(),
                            excerpt_chars: audit_baseline.excerpt_chars,
                            duration_ms,
                            facts_before: audit_baseline.facts_before,
                            facts_after: audit_baseline.facts_before,
                            had_user_context_before: audit_baseline.had_user_context_before,
                            had_user_context_after: audit_baseline.had_user_context_before,
                            changed: false,
                            error: Some(error.clone()),
                        },
                    )
                    .await;
                    eprintln!("{error}");
                }
                Ok(Ok(stats)) => {
                    let duration_ms = start.elapsed().as_millis() as u64;
                    let now = now_epoch_secs();
                    with_queue_status(&status, |snapshot| {
                        snapshot.state = "idle".to_string();
                        snapshot.succeeded += 1;
                        snapshot.last_duration_ms = duration_ms;
                        snapshot.last_error = None;
                        snapshot.last_success_at = now;
                        snapshot.last_finished_at = now;
                        snapshot.last_excerpt_chars = stats.excerpt_chars;
                    });
                    append_memory_audit_record(
                        &final_req.workspace,
                        &MemoryAuditRecord {
                            timestamp: now,
                            model: final_req.model.clone(),
                            status: "success".to_string(),
                            excerpt_chars: stats.excerpt_chars,
                            duration_ms,
                            facts_before: stats.facts_before,
                            facts_after: stats.facts_after,
                            had_user_context_before: stats.had_user_context_before,
                            had_user_context_after: stats.had_user_context_after,
                            changed: stats.changed,
                            error: None,
                        },
                    )
                    .await;
                }
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

/// Merge a parsed LLM extraction response into the existing memory.
///
/// Supports two formats:
/// - **Incremental** (`update_facts` + `delete_facts`): only touches mentioned facts.
/// - **Legacy full-replacement** (`facts`): replaces all facts (backward compat).
///
/// `user_context` is only updated when the key is explicitly present in `raw`.
pub(crate) fn merge_llm_response_into_memory(
    memory: &mut StructuredMemory,
    raw: &serde_json::Value,
    now: u64,
) {
    // Only touch user_context when the key is actually present in the response.
    // null → clear, string → update, absent → preserve existing.
    if raw.get("user_context").is_some() {
        memory.user_context = raw["user_context"].as_str().map(|s| s.to_string());
    }

    let used_incremental = raw.get("update_facts").is_some() || raw.get("delete_facts").is_some();

    if used_incremental {
        // Apply deletions first
        if let Some(delete_arr) = raw.get("delete_facts").and_then(|v| v.as_array()) {
            let delete_keys: Vec<&str> = delete_arr.iter().filter_map(|v| v.as_str()).collect();
            memory
                .facts
                .retain(|f| !delete_keys.contains(&f.key.as_str()));
        }

        // Apply updates/inserts
        if let Some(update_arr) = raw.get("update_facts").and_then(|v| v.as_array()) {
            for fv in update_arr {
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
                if let Some(existing_fact) = memory.facts.iter_mut().find(|f| f.key == key) {
                    if existing_fact.value != value {
                        existing_fact.value = value;
                        existing_fact.recorded_at = now;
                    }
                } else {
                    memory.facts.push(MemoryFact {
                        key,
                        value,
                        recorded_at: now,
                    });
                }
            }
        }
    } else if let Some(facts_val) = raw.get("facts") {
        // Legacy full-replacement path
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
                let recorded_at = memory
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
            memory.facts = new_facts;
        }
    }
}

/// Core memory update: call LLM to extract memory from conversation,
/// merge with existing memory, and persist.
async fn process_memory_update(
    req: &MemoryUpdateRequest,
    config: &Config,
    http: &Client,
) -> Result<MemoryProcessStats, String> {
    let existing = load_structured_memory(&req.workspace);
    let facts_before = existing.facts.len();
    let had_user_context_before = existing.user_context.is_some();

    // Build conversation excerpt text
    let excerpt = build_conversation_excerpt(&req.conversation_excerpt);
    let excerpt_chars = excerpt.chars().count();
    if excerpt.trim().is_empty() {
        return Ok(MemoryProcessStats {
            excerpt_chars,
            facts_before,
            facts_after: facts_before,
            had_user_context_before,
            had_user_context_after: had_user_context_before,
            changed: false,
        });
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
2. Return ONLY the changes needed — do not repeat unchanged facts.
3. Update user_context if the user reveals preferences, background, or working style. Omit user_context from your response if it hasn't changed. Set to null to clear it.
4. Return ONLY valid JSON matching this schema (no markdown fences, no explanation):

{{"user_context": "string or null (omit if unchanged)", "update_facts": [{{"key": "short_label", "value": "content"}}], "delete_facts": ["key_to_remove"]}}

- `update_facts`: New facts to add, or existing facts to update (matched by key). Only include facts that are new or whose value changed.
- `delete_facts`: Keys of facts that are clearly outdated or contradicted by the conversation. Only delete when you are certain.
- If there is nothing meaningful to extract, return: {{"update_facts": [], "delete_facts": []}}
Keep facts concise. Do not store ephemeral task details — only persistent knowledge."#
    );

    let messages = vec![
        crate::ChatMessage {
            role: "system".into(),
            content: Some(system_prompt),
            images: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        crate::ChatMessage {
            role: "user".into(),
            content: Some(format!("Conversation to analyze:\n\n{excerpt}")),
            images: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
    ];

    let resolved = config.resolve_model(&req.model);
    let response = providers::call_llm_simple(
        http,
        &resolved,
        &messages,
        &req.workspace,
        config.s3.as_ref(),
        config.max_llm_retries,
    )
    .await
    .map_err(|e| format!("LLM call failed: {e}"))?;

    let response = response.trim();
    if response.is_empty() {
        return Ok(MemoryProcessStats {
            excerpt_chars,
            facts_before,
            facts_after: facts_before,
            had_user_context_before,
            had_user_context_after: had_user_context_before,
            changed: false,
        });
    }

    // Strip markdown fences if present
    let json_str = strip_json_fences(response);

    // Parse as raw Value first so we can distinguish "field absent" from
    // "field explicitly null" — prevents silent data loss when the LLM
    // returns incomplete JSON.
    let raw: serde_json::Value =
        serde_json::from_str(json_str).map_err(|e| format!("parse LLM response: {e}"))?;

    let now = now_epoch_secs();

    let mut merged = existing;
    let before_json = serde_json::to_string(&merged).unwrap_or_default();

    merge_llm_response_into_memory(&mut merged, &raw, now);

    // Cap total facts to prevent unbounded growth
    const MAX_FACTS: usize = 50;
    if merged.facts.len() > MAX_FACTS {
        // Keep the most recently recorded facts
        merged
            .facts
            .sort_by(|a, b| b.recorded_at.cmp(&a.recorded_at));
        merged.facts.truncate(MAX_FACTS);
    }

    let facts_after = merged.facts.len();
    let had_user_context_after = merged.user_context.is_some();
    // Only update timestamp and persist when actual content changed.
    let after_json = serde_json::to_string(&merged).unwrap_or_default();
    let changed = before_json != after_json;
    if changed {
        merged.updated_at = now;
        save_structured_memory(&req.workspace, &merged)?;
    }

    Ok(MemoryProcessStats {
        excerpt_chars,
        facts_before,
        facts_after,
        had_user_context_before,
        had_user_context_after,
        changed,
    })
}

/// Max chars for a single tool result summary in the conversation excerpt.
const TOOL_RESULT_EXCERPT_LIMIT: usize = 200;

/// Maximum number of recent messages to keep for memory extraction.
/// Only the tail of the conversation is relevant — older context is already captured.
const MEMORY_EXCERPT_MAX_MESSAGES: usize = 40;

/// Pre-filter messages for memory extraction. Returns a lightweight clone
/// containing only the recent non-system messages needed for memory extraction,
/// avoiding a full clone of the entire session history.
pub(crate) fn prefilter_for_memory(messages: &[crate::ChatMessage]) -> Vec<crate::ChatMessage> {
    let start = if messages.len() > MEMORY_EXCERPT_MAX_MESSAGES {
        let tentative = messages.len() - MEMORY_EXCERPT_MAX_MESSAGES;
        // Scan backward from (and including) tentative to find the nearest
        // "user" message, ensuring we start at a complete turn boundary
        // rather than mid-turn (e.g. orphaned tool results without their
        // triggering question).
        // If no user message exists at or before tentative, fall back to tentative.
        messages[..=tentative]
            .iter()
            .rposition(|m| m.role == "user")
            .unwrap_or(tentative)
    } else {
        0
    };
    messages[start..]
        .iter()
        .filter(|m| m.role != "system")
        .cloned()
        .collect()
}

/// Build conversation excerpt from messages, including user, assistant, and
/// brief tool result summaries for key findings. Filters out auto-generated
/// compression summaries and excessive tool noise.
pub(crate) fn build_conversation_excerpt(messages: &[crate::ChatMessage]) -> String {
    let mut lines = Vec::new();
    for msg in messages {
        match msg.role.as_str() {
            "user" => {
                if let Some(content) = msg.content.as_deref()
                    && !content.is_empty()
                {
                    lines.push(format!("User: {content}"));
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
                // Include tool call names so memory captures what the agent did.
                if let Some(tool_calls) = &msg.tool_calls {
                    for tc in tool_calls {
                        lines.push(format!("[tool: {}]", tc.function.name));
                    }
                }
            }
            "tool" => {
                // Include brief tool result summaries when the result indicates
                // a notable finding (not just raw data dumps).
                if let Some(content) = msg.content.as_deref()
                    && !content.is_empty()
                {
                    let first_line = content.lines().next().unwrap_or("");
                    let summary = if content.len() <= TOOL_RESULT_EXCERPT_LIMIT {
                        content.to_string()
                    } else {
                        truncate_inline(first_line, TOOL_RESULT_EXCERPT_LIMIT)
                    };
                    // Only include non-trivial results.
                    if !summary.trim().is_empty() {
                        lines.push(format!("[tool result: {summary}]"));
                    }
                }
            }
            _ => {} // skip system
        }
    }
    lines.join("\n\n")
}

/// Strip ```json ... ``` fences from LLM output.
fn strip_json_fences(s: &str) -> &str {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("```json")
        && let Some(inner) = rest.strip_suffix("```")
    {
        return inner.trim();
    }
    if let Some(rest) = s.strip_prefix("```")
        && let Some(inner) = rest.strip_suffix("```")
    {
        return inner.trim();
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
        lines.push(format!(
            "Last updated: {}",
            format_relative_age(now_epoch_secs().saturating_sub(mem.updated_at))
        ));
    }

    lines.join("\n")
}

// ══════════════════════════════════════════════════════════════════════════════
//  Tests
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
#[path = "tests/memory_tests.rs"]
mod tests;
