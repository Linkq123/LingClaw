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
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tokio::{fs::OpenOptions, io::AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use crate::{
    Session,
    agent::{TaskIntent, WorkingState},
    config::Config,
    context::{USAGE_ROLE_MEMORY, UsageUpdate, apply_usage_update, build_usage_labels},
    providers,
    tools::{ToolRankingContext, ToolRankingSource},
};

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
    /// Reusable lessons extracted from repeated successful or failed work.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lessons: Vec<MemoryLesson>,
    /// Unresolved goals or blockers that may still matter in future runs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_loops: Vec<OpenLoop>,
    /// Command or tool usage patterns worth remembering for this workspace.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command_patterns: Vec<CommandPattern>,
    /// Stable project-specific signals such as build systems or key entrypoints.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub project_signals: Vec<ProjectSignal>,
    /// Updated epoch seconds (set on write).
    #[serde(default)]
    pub updated_at: u64,
}

impl StructuredMemory {
    fn is_empty(&self) -> bool {
        self.user_context.is_none()
            && self.facts.is_empty()
            && self.lessons.is_empty()
            && self.open_loops.is_empty()
            && self.command_patterns.is_empty()
            && self.project_signals.is_empty()
    }

    fn entry_count(&self) -> usize {
        self.facts.len()
            + self.lessons.len()
            + self.open_loops.len()
            + self.command_patterns.len()
            + self.project_signals.len()
    }
}

/// A single remembered fact/decision.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct MemoryFact {
    /// Short label, e.g. "preferred_language", "project_stack".
    pub key: String,
    /// The remembered content.
    pub value: String,
    /// When this fact was recorded (epoch seconds).
    #[serde(default)]
    pub recorded_at: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MemoryConfidence {
    Low,
    #[default]
    Medium,
    High,
}

impl MemoryConfidence {
    fn label(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    fn from_raw(value: Option<&str>) -> Self {
        match value.unwrap_or("").trim().to_ascii_lowercase().as_str() {
            "low" => Self::Low,
            "high" => Self::High,
            _ => Self::Medium,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct MemoryLesson {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub when_to_apply: String,
    #[serde(default)]
    pub recommendation: String,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub confidence: MemoryConfidence,
    #[serde(default)]
    pub last_seen_at: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OpenLoopStatus {
    #[default]
    Open,
    InProgress,
    Resolved,
}

impl OpenLoopStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::InProgress => "in_progress",
            Self::Resolved => "resolved",
        }
    }

    fn from_raw(value: Option<&str>) -> Self {
        match value.unwrap_or("").trim().to_ascii_lowercase().as_str() {
            "in_progress" | "in-progress" | "active" => Self::InProgress,
            "resolved" | "closed" | "done" => Self::Resolved,
            _ => Self::Open,
        }
    }

    fn rank(self) -> usize {
        match self {
            Self::Open => 2,
            Self::InProgress => 1,
            Self::Resolved => 0,
        }
    }

    fn is_resolved(self) -> bool {
        matches!(self, Self::Resolved)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct OpenLoop {
    #[serde(default)]
    pub goal: String,
    #[serde(default)]
    pub blocker: String,
    #[serde(default)]
    pub next_step: String,
    #[serde(default)]
    pub status: OpenLoopStatus,
    #[serde(default)]
    pub updated_at: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct CommandPattern {
    #[serde(default)]
    pub signature: String,
    #[serde(default)]
    pub purpose: String,
    #[serde(default)]
    pub outcome: String,
    #[serde(default)]
    pub confidence: MemoryConfidence,
    #[serde(default)]
    pub last_seen_at: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ProjectSignal {
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub recorded_at: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RetrievedTaskMemory {
    pub lessons: Vec<MemoryLesson>,
    pub open_loops: Vec<OpenLoop>,
    pub command_patterns: Vec<CommandPattern>,
    pub project_signals: Vec<ProjectSignal>,
    pub facts: Vec<MemoryFact>,
}

impl RetrievedTaskMemory {
    pub(crate) fn is_empty(&self) -> bool {
        self.lessons.is_empty()
            && self.open_loops.is_empty()
            && self.command_patterns.is_empty()
            && self.project_signals.is_empty()
            && self.facts.is_empty()
    }
}

const MEMORY_FILE_NAME: &str = "structured_memory.json";
const MEMORY_AUDIT_FILE_NAME: &str = "structured_memory.audit.jsonl";
/// Max audit file size before rotation (trim oldest entries).
const MEMORY_AUDIT_MAX_BYTES: u64 = 256_000;
const MEMORY_INJECTION_CHAR_BUDGET: usize = 2_000;
const MEMORY_INJECTION_MAX_FACTS_WITHOUT_QUERY: usize = 8;
const MEMORY_INJECTION_MAX_RELEVANT_FACTS: usize = 8;
const MEMORY_INJECTION_MAX_FALLBACK_FACTS: usize = 3;
const MEMORY_INJECTION_MAX_LESSONS_WITHOUT_QUERY: usize = 3;
const MEMORY_INJECTION_MAX_RELEVANT_LESSONS: usize = 3;
const MEMORY_INJECTION_MAX_FALLBACK_LESSONS: usize = 1;
const MEMORY_INJECTION_MAX_OPEN_LOOPS_WITHOUT_QUERY: usize = 3;
const MEMORY_INJECTION_MAX_RELEVANT_OPEN_LOOPS: usize = 2;
const MEMORY_INJECTION_MAX_FALLBACK_OPEN_LOOPS: usize = 1;
const MEMORY_INJECTION_MAX_COMMAND_PATTERNS_WITHOUT_QUERY: usize = 3;
const MEMORY_INJECTION_MAX_RELEVANT_COMMAND_PATTERNS: usize = 2;
const MEMORY_INJECTION_MAX_FALLBACK_COMMAND_PATTERNS: usize = 1;
const MEMORY_INJECTION_MAX_PROJECT_SIGNALS_WITHOUT_QUERY: usize = 4;
const MEMORY_INJECTION_MAX_RELEVANT_PROJECT_SIGNALS: usize = 4;
const MEMORY_INJECTION_MAX_FALLBACK_PROJECT_SIGNALS: usize = 1;
const MAX_MEMORY_FACT_VALUE_CHARS: usize = 240;
const MAX_MEMORY_USER_CONTEXT_CHARS: usize = 320;
const MAX_MEMORY_LESSON_TITLE_CHARS: usize = 96;
const MAX_MEMORY_LESSON_WHEN_CHARS: usize = 160;
const MAX_MEMORY_LESSON_RECOMMENDATION_CHARS: usize = 220;
const MAX_MEMORY_SCOPE_CHARS: usize = 72;
const MAX_OPEN_LOOP_GOAL_CHARS: usize = 140;
const MAX_OPEN_LOOP_BLOCKER_CHARS: usize = 220;
const MAX_OPEN_LOOP_NEXT_STEP_CHARS: usize = 180;
const MAX_COMMAND_SIGNATURE_CHARS: usize = 140;
const MAX_COMMAND_PURPOSE_CHARS: usize = 180;
const MAX_COMMAND_OUTCOME_CHARS: usize = 160;
const MAX_PROJECT_SIGNAL_VALUE_CHARS: usize = 180;
const MAX_MEMORY_LESSONS: usize = 24;
const MAX_MEMORY_OPEN_LOOPS: usize = 16;
const MAX_MEMORY_COMMAND_PATTERNS: usize = 24;
const MAX_MEMORY_PROJECT_SIGNALS: usize = 24;
const TASK_MEMORY_PROMPT_CHAR_BUDGET: usize = 1_200;
const TASK_MEMORY_QUERY_CHAR_BUDGET: usize = 600;
const TOOL_HINT_PROMPT_CHAR_BUDGET: usize = 700;

#[derive(Clone, Copy)]
struct TaskMemoryCaps {
    lessons: usize,
    open_loops: usize,
    command_patterns: usize,
    project_signals: usize,
    facts: usize,
}

#[derive(Clone)]
struct StructuredMemoryCacheEntry {
    workspace: PathBuf,
    file_mtime: Option<SystemTime>,
    memory: StructuredMemory,
}

type StructuredMemoryCacheLock = OnceLock<Mutex<Option<StructuredMemoryCacheEntry>>>;
static STRUCTURED_MEMORY_CACHE: StructuredMemoryCacheLock = OnceLock::new();

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
    #[serde(default)]
    entries_before: usize,
    #[serde(default)]
    entries_after: usize,
    had_user_context_before: bool,
    had_user_context_after: bool,
    changed: bool,
    error: Option<String>,
}

struct MemoryAuditBaseline {
    excerpt_chars: usize,
    facts_before: usize,
    entries_before: usize,
    had_user_context_before: bool,
}

#[derive(Clone, Debug)]
struct MemoryProcessStats {
    excerpt_chars: usize,
    facts_before: usize,
    facts_after: usize,
    entries_before: usize,
    entries_after: usize,
    had_user_context_before: bool,
    had_user_context_after: bool,
    changed: bool,
    usage: Option<UsageUpdate>,
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
        entries_before: existing.entry_count(),
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
            "- {} | {} | model {} | excerpt {} chars | facts {} -> {} | entries {} -> {} | {} ms",
            age,
            record.status,
            record.model,
            record.excerpt_chars,
            record.facts_before,
            record.facts_after,
            record.entries_before,
            record.entries_after,
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
    let file_mtime = std::fs::metadata(&path)
        .ok()
        .and_then(|meta| meta.modified().ok());
    let cache = STRUCTURED_MEMORY_CACHE.get_or_init(|| Mutex::new(None));

    {
        let guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref entry) = *guard
            && entry.workspace == workspace
            && entry.file_mtime == file_mtime
        {
            return entry.memory.clone();
        }
    }

    let memory = match std::fs::read_to_string(&path) {
        Ok(data) => match serde_json::from_str(&data) {
            Ok(memory) => memory,
            Err(error) => {
                eprintln!(
                    "Failed to parse structured memory at {}: {error}",
                    path.display()
                );
                StructuredMemory::default()
            }
        },
        Err(_) => StructuredMemory::default(),
    };

    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(StructuredMemoryCacheEntry {
        workspace: workspace.to_path_buf(),
        file_mtime,
        memory: memory.clone(),
    });
    memory
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

    std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;

    let file_mtime = std::fs::metadata(&path)
        .ok()
        .and_then(|meta| meta.modified().ok());
    let cache = STRUCTURED_MEMORY_CACHE.get_or_init(|| Mutex::new(None));
    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(StructuredMemoryCacheEntry {
        workspace: workspace.to_path_buf(),
        file_mtime,
        memory: mem.clone(),
    });
    Ok(())
}

// ── Prompt injection ────────────────────────────────────────────────────────

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
    if mem.is_empty() {
        return None;
    }

    let mut lines = Vec::new();
    lines.push("## Structured Memory (auto-maintained)".to_string());

    if let Some(ref ctx) = mem.user_context
        && !ctx.trim().is_empty()
    {
        lines.push(format!("**User context:** {}", ctx.trim()));
    }

    let selected_open_loops = select_open_loops_for_injection(mem, current_query);
    if !selected_open_loops.is_empty() {
        lines.push("**Open loops:**".to_string());
        for item in &selected_open_loops {
            lines.push(format!(
                "- **{}** [{}]: blocker: {}; next: {}",
                item.goal,
                item.status.label(),
                item.blocker,
                item.next_step
            ));
        }
    }

    let selected_lessons = select_lessons_for_injection(mem, current_query);
    if !selected_lessons.is_empty() {
        lines.push("**Lessons:**".to_string());
        for lesson in &selected_lessons {
            let scope = if lesson.scope.is_empty() {
                "general"
            } else {
                lesson.scope.as_str()
            };
            lines.push(format!(
                "- **{}** [{} | {}]: when {}; {}",
                lesson.title,
                lesson.confidence.label(),
                scope,
                lesson.when_to_apply,
                lesson.recommendation
            ));
        }
    }

    let selected_project_signals = select_project_signals_for_injection(mem, current_query);
    if !selected_project_signals.is_empty() {
        lines.push("**Project signals:**".to_string());
        for signal in &selected_project_signals {
            lines.push(format!("- **{}**: {}", signal.key, signal.value));
        }
    }

    let selected_command_patterns = select_command_patterns_for_injection(mem, current_query);
    if !selected_command_patterns.is_empty() {
        lines.push("**Command patterns:**".to_string());
        for pattern in &selected_command_patterns {
            lines.push(format!(
                "- `{}` [{}]: {} -> {}",
                pattern.signature,
                pattern.confidence.label(),
                pattern.purpose,
                pattern.outcome
            ));
        }
    }

    let selected_facts = select_facts_for_injection(mem, current_query);
    if !selected_facts.is_empty() {
        lines.push("**Remembered facts:**".to_string());
        for fact in &selected_facts {
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

fn task_memory_caps(intent: TaskIntent) -> TaskMemoryCaps {
    match intent {
        TaskIntent::Inform => TaskMemoryCaps {
            lessons: 1,
            open_loops: 1,
            command_patterns: 1,
            project_signals: 2,
            facts: 2,
        },
        TaskIntent::Change => TaskMemoryCaps {
            lessons: 2,
            open_loops: 2,
            command_patterns: 2,
            project_signals: 2,
            facts: 1,
        },
        TaskIntent::Investigate => TaskMemoryCaps {
            lessons: 2,
            open_loops: 2,
            command_patterns: 1,
            project_signals: 2,
            facts: 2,
        },
        TaskIntent::Execute => TaskMemoryCaps {
            lessons: 1,
            open_loops: 2,
            command_patterns: 2,
            project_signals: 2,
            facts: 1,
        },
    }
}

fn build_task_memory_query(
    current_query: Option<&str>,
    state: Option<&WorkingState>,
) -> Option<String> {
    let mut parts = Vec::new();

    if let Some(query) = current_query
        .and_then(|query| sanitize_memory_text_value(query, TASK_MEMORY_QUERY_CHAR_BUDGET))
    {
        parts.push(query);
    }

    if let Some(goal) = state
        .and_then(|state| state.primary_goal.as_deref())
        .and_then(|goal| sanitize_memory_text_value(goal, 200))
        && !parts.iter().any(|part| part == &goal)
    {
        parts.push(goal);
    }

    if let Some(state) = state {
        for question in state.open_questions.iter().rev().take(2) {
            if let Some(question) = sanitize_memory_text_value(question, 160) {
                parts.push(question);
            }
        }
        for uncertainty in state
            .uncertainties
            .iter()
            .rev()
            .filter(|item| item.blocking)
            .take(2)
        {
            let topic = sanitize_memory_text_value(&uncertainty.topic, 80).unwrap_or_default();
            let reason = sanitize_memory_text_value(&uncertainty.reason, 120).unwrap_or_default();
            let combined = format!("{topic} {reason}").trim().to_string();
            if !combined.is_empty() {
                parts.push(combined);
            }
        }
        for action in state.next_actions.iter().rev().take(2) {
            if let Some(action) = sanitize_memory_text_value(action, 160) {
                parts.push(action);
            }
        }
        for step in state.completed_steps.iter().rev().take(2) {
            if let Some(step) = sanitize_memory_text_value(step, 160) {
                parts.push(step);
            }
        }
        for evidence in state.evidence.iter().rev().take(2) {
            let combined = if evidence.source_ref.trim().is_empty() {
                evidence.claim.clone()
            } else {
                format!("{} {}", evidence.claim, evidence.source_ref)
            };
            if let Some(evidence) = sanitize_memory_text_value(&combined, 180) {
                parts.push(evidence);
            }
        }
    }

    let combined = parts.join(" ");
    sanitize_memory_text_value(&combined, TASK_MEMORY_QUERY_CHAR_BUDGET)
}

pub(crate) fn retrieve_task_memory(
    mem: &StructuredMemory,
    current_query: Option<&str>,
    state: Option<&WorkingState>,
) -> RetrievedTaskMemory {
    if mem.is_empty() {
        return RetrievedTaskMemory::default();
    }

    let intent = state
        .map(|state| state.intent)
        .unwrap_or_else(|| TaskIntent::classify(current_query));
    let caps = task_memory_caps(intent);
    let retrieval_query = build_task_memory_query(current_query, state);
    let query = retrieval_query.as_deref();

    let open_loops = {
        let unresolved: Vec<OpenLoop> = mem
            .open_loops
            .iter()
            .filter(|item| !item.status.is_resolved())
            .cloned()
            .collect();
        select_task_items(
            &unresolved,
            query,
            caps.open_loops,
            open_loop_relevance_score,
            |item| item.updated_at,
            open_loop_identity,
        )
    };

    RetrievedTaskMemory {
        lessons: select_task_items(
            &mem.lessons,
            query,
            caps.lessons,
            lesson_relevance_score,
            |lesson| lesson.last_seen_at,
            lesson_identity,
        ),
        open_loops,
        command_patterns: select_task_items(
            &mem.command_patterns,
            query,
            caps.command_patterns,
            command_pattern_relevance_score,
            |item| item.last_seen_at,
            command_pattern_identity,
        ),
        project_signals: select_task_items(
            &mem.project_signals,
            query,
            caps.project_signals,
            project_signal_relevance_score,
            |signal| signal.recorded_at,
            |signal| signal.key.clone(),
        ),
        facts: select_task_items(
            &mem.facts,
            query,
            caps.facts,
            fact_relevance_score,
            |fact| fact.recorded_at,
            |fact| fact.key.clone(),
        ),
    }
}

pub(crate) fn task_memory_cache_key(
    current_query: Option<&str>,
    state: Option<&WorkingState>,
) -> String {
    let intent = state
        .map(|state| state.intent)
        .unwrap_or_else(|| TaskIntent::classify(current_query));
    let intent_tag = match intent {
        TaskIntent::Inform => "inform",
        TaskIntent::Change => "change",
        TaskIntent::Investigate => "investigate",
        TaskIntent::Execute => "execute",
    };
    let retrieval_query = build_task_memory_query(current_query, state).unwrap_or_default();
    format!("{intent_tag}\n{retrieval_query}")
}

pub(crate) fn format_task_memory_for_prompt(
    selected: &RetrievedTaskMemory,
    intent: TaskIntent,
) -> Option<String> {
    if selected.is_empty() {
        return None;
    }

    let mut lines = vec!["## Relevant Past Experience".to_string()];
    lines.push(format!(
        "- Focus: {}",
        match intent {
            TaskIntent::Inform => "background context that may answer the question",
            TaskIntent::Change => "past lessons and project anchors that may guide the change",
            TaskIntent::Investigate =>
                "prior blockers and project signals that may explain the issue",
            TaskIntent::Execute => "known commands and project anchors that may help the execution",
        }
    ));

    if !selected.open_loops.is_empty() {
        lines.push("- Open loops to revisit:".to_string());
        for item in &selected.open_loops {
            lines.push(format!(
                "  - {} [{}]: next {}",
                item.goal,
                item.status.label(),
                item.next_step
            ));
        }
    }
    if !selected.lessons.is_empty() {
        lines.push("- Relevant lessons:".to_string());
        for lesson in &selected.lessons {
            lines.push(format!(
                "  - {} [{}]: {}",
                lesson.title,
                lesson.confidence.label(),
                lesson.recommendation
            ));
        }
    }
    if !selected.project_signals.is_empty() {
        lines.push("- Project signals:".to_string());
        for signal in &selected.project_signals {
            lines.push(format!("  - {}: {}", signal.key, signal.value));
        }
    }
    if !selected.command_patterns.is_empty() {
        lines.push("- Command patterns:".to_string());
        for pattern in &selected.command_patterns {
            lines.push(format!(
                "  - `{}` [{}]: {}",
                pattern.signature,
                pattern.confidence.label(),
                pattern.purpose
            ));
        }
    }
    if !selected.facts.is_empty() {
        lines.push("- Relevant facts:".to_string());
        for fact in &selected.facts {
            lines.push(format!("  - {}: {}", fact.key, fact.value));
        }
    }

    let rendered = lines.join("\n");
    if rendered.len() <= TASK_MEMORY_PROMPT_CHAR_BUDGET {
        return Some(rendered);
    }

    let marker = "\n*(relevant memory truncated)*";
    let keep = TASK_MEMORY_PROMPT_CHAR_BUDGET.saturating_sub(marker.len());
    Some(format!("{}{}", crate::truncate(&rendered, keep), marker))
}

pub(crate) fn task_memory_next_actions(
    selected: &RetrievedTaskMemory,
    intent: TaskIntent,
) -> Vec<String> {
    let mut actions = Vec::new();
    let mut seen = HashSet::new();

    let mut push_action = |value: String| {
        if let Some(value) = sanitize_memory_text_value(&value, 180) {
            let key = normalize_match_text(&value);
            if seen.insert(key) {
                actions.push(value);
            }
        }
    };

    for item in selected.open_loops.iter().take(2) {
        push_action(format!("Revisit '{}': {}", item.goal, item.next_step));
    }
    for lesson in selected.lessons.iter().take(2) {
        push_action(lesson.recommendation.clone());
    }
    if matches!(intent, TaskIntent::Change | TaskIntent::Execute) {
        for pattern in selected.command_patterns.iter().take(1) {
            push_action(format!(
                "Reuse `{}` when {}",
                pattern.signature, pattern.purpose
            ));
        }
    }

    actions.truncate(3);
    actions
}

pub(crate) fn format_task_tool_hints_for_prompt(
    selected: &RetrievedTaskMemory,
    intent: TaskIntent,
) -> Option<String> {
    let mut lines = vec!["## Tool Hints".to_string()];
    let mut seen = HashSet::new();

    let mut push_line = |value: String| {
        if let Some(value) = sanitize_memory_text_value(&value, 220) {
            let key = normalize_match_text(&value);
            if seen.insert(key) {
                lines.push(format!("- {value}"));
            }
        }
    };

    if let Some(command) = remembered_command_hint(selected) {
        push_line(format!(
            "Prefer `exec` when validating or reusing known commands like `{command}`."
        ));
    }

    if let Some(anchor) = remembered_file_anchor(selected) {
        push_line(format!(
            "Prefer `read_file` or `search_files` around `{anchor}` before guessing."
        ));
    }

    if let Some(loop_item) = selected.open_loops.first() {
        let hint = match intent {
            TaskIntent::Inform | TaskIntent::Investigate => format!(
                "Revisit '{}' with focused inspection before concluding.",
                loop_item.goal
            ),
            TaskIntent::Change | TaskIntent::Execute => format!(
                "Revisit '{}' and validate the result before wrapping up.",
                loop_item.goal
            ),
        };
        push_line(hint);
    }

    if let Some(url_anchor) = remembered_url_anchor(selected) {
        push_line(format!(
            "Prefer `http_fetch` if you need to verify external docs or URLs like `{url_anchor}`."
        ));
    }

    if lines.len() == 1 {
        return None;
    }

    let rendered = lines.join("\n");
    if rendered.len() <= TOOL_HINT_PROMPT_CHAR_BUDGET {
        return Some(rendered);
    }

    let marker = "\n*(tool hints truncated)*";
    let keep = TOOL_HINT_PROMPT_CHAR_BUDGET.saturating_sub(marker.len());
    Some(format!("{}{}", crate::truncate(&rendered, keep), marker))
}

pub(crate) fn task_tool_ranking_context(
    selected: &RetrievedTaskMemory,
    _intent: TaskIntent,
) -> ToolRankingContext {
    let mut ranking = ToolRankingContext::default();

    if remembered_command_hint(selected).is_some() {
        ranking.add_preference(
            "exec",
            "remembered command pattern",
            4,
            ToolRankingSource::Memory,
        );
    }
    if remembered_file_anchor(selected).is_some() {
        ranking.add_preference(
            "read_file",
            "remembered file anchor",
            4,
            ToolRankingSource::Memory,
        );
        ranking.add_preference(
            "search_files",
            "remembered file anchor",
            4,
            ToolRankingSource::Memory,
        );
    }
    if remembered_url_anchor(selected).is_some() {
        ranking.add_preference(
            "http_fetch",
            "remembered URL anchor",
            4,
            ToolRankingSource::Memory,
        );
    }
    if !selected.open_loops.is_empty() {
        ranking.add_preference("think", "open memory loop", 3, ToolRankingSource::Memory);
    }

    ranking
}

pub(crate) fn task_memory_resolution_anchors(selected: &RetrievedTaskMemory) -> Vec<String> {
    let mut anchors = Vec::new();
    let mut seen = HashSet::new();

    let mut push_anchor = |value: String| {
        if let Some(value) = sanitize_memory_text_value(&value, 120) {
            let key = normalize_match_text(&value);
            if seen.insert(key) {
                anchors.push(value);
            }
        }
    };

    for pattern in &selected.command_patterns {
        if let Some(anchor) = extract_shell_command_anchor(&pattern.signature) {
            push_anchor(anchor);
        }
    }

    for signal in &selected.project_signals {
        for candidate in [&signal.key, &signal.value] {
            if let Some(anchor) = extract_file_anchor(candidate)
                .or_else(|| extract_url_anchor(candidate))
                .or_else(|| extract_shell_command_anchor(candidate))
            {
                push_anchor(anchor);
            }
        }
    }

    for fact in &selected.facts {
        for candidate in [&fact.key, &fact.value] {
            if let Some(anchor) = extract_file_anchor(candidate)
                .or_else(|| extract_url_anchor(candidate))
                .or_else(|| extract_shell_command_anchor(candidate))
            {
                push_anchor(anchor);
            }
        }
    }

    for item in &selected.open_loops {
        for candidate in [&item.goal, &item.blocker, &item.next_step] {
            if let Some(anchor) = extract_file_anchor(candidate)
                .or_else(|| extract_url_anchor(candidate))
                .or_else(|| extract_shell_command_anchor(candidate))
            {
                push_anchor(anchor);
            }
        }
    }

    anchors.truncate(5);
    anchors
}

fn select_task_items<T, FScore, FRecency, FKey>(
    items: &[T],
    current_query: Option<&str>,
    limit: usize,
    score_fn: FScore,
    recency_fn: FRecency,
    key_fn: FKey,
) -> Vec<T>
where
    T: Clone,
    FScore: Fn(&T, &[String], &str) -> usize,
    FRecency: Fn(&T) -> u64,
    FKey: Fn(&T) -> String,
{
    let mut ranked = items.to_vec();
    if ranked.is_empty() {
        return ranked;
    }

    if let Some(query) = current_query
        .map(str::trim)
        .filter(|query| !query.is_empty())
    {
        let query_tokens = crate::tokenize_for_matching(query);
        let query_lower = query.to_lowercase();
        let mut scored: Vec<(usize, T)> = ranked
            .iter()
            .cloned()
            .map(|item| (score_fn(&item, &query_tokens, &query_lower), item))
            .collect();
        scored.sort_by(|(score_a, item_a), (score_b, item_b)| {
            score_b
                .cmp(score_a)
                .then(recency_fn(item_b).cmp(&recency_fn(item_a)))
                .then_with(|| key_fn(item_a).cmp(&key_fn(item_b)))
        });
        if scored.first().map(|(score, _)| *score).unwrap_or(0) == 0 {
            return Vec::new();
        }
        return scored
            .into_iter()
            .filter(|(score, _)| *score > 0)
            .take(limit)
            .map(|(_, item)| item)
            .collect();
    }

    ranked.sort_by(|a, b| {
        recency_fn(b)
            .cmp(&recency_fn(a))
            .then_with(|| key_fn(a).cmp(&key_fn(b)))
    });
    ranked.truncate(limit);
    ranked
}

fn remembered_command_hint(selected: &RetrievedTaskMemory) -> Option<String> {
    selected
        .command_patterns
        .iter()
        .map(|pattern| pattern.signature.as_str())
        .chain(
            selected
                .project_signals
                .iter()
                .map(|signal| signal.value.as_str()),
        )
        .chain(selected.facts.iter().map(|fact| fact.value.as_str()))
        .find_map(extract_shell_command_anchor)
}

fn remembered_file_anchor(selected: &RetrievedTaskMemory) -> Option<String> {
    selected
        .project_signals
        .iter()
        .flat_map(|signal| [signal.key.as_str(), signal.value.as_str()])
        .chain(
            selected
                .facts
                .iter()
                .flat_map(|fact| [fact.key.as_str(), fact.value.as_str()]),
        )
        .chain(selected.open_loops.iter().flat_map(|item| {
            [
                item.goal.as_str(),
                item.blocker.as_str(),
                item.next_step.as_str(),
            ]
        }))
        .find_map(extract_file_anchor)
}

fn remembered_url_anchor(selected: &RetrievedTaskMemory) -> Option<String> {
    selected
        .project_signals
        .iter()
        .map(|signal| signal.value.as_str())
        .chain(selected.facts.iter().map(|fact| fact.value.as_str()))
        .find_map(extract_url_anchor)
}

fn extract_shell_command_anchor(text: &str) -> Option<String> {
    let trimmed = normalize_memory_whitespace(text);
    let lower = trimmed.to_ascii_lowercase();
    let looks_like_command = [
        "cargo ",
        "npm ",
        "pnpm ",
        "yarn ",
        "git ",
        "python ",
        "uv ",
        "go ",
        "node ",
        "bash ",
        "powershell ",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix));
    if !looks_like_command {
        return None;
    }
    sanitize_memory_text_value(&trimmed, 100)
}

fn extract_file_anchor(text: &str) -> Option<String> {
    text.split_whitespace().find_map(|token| {
        let trimmed = token
            .trim_matches(|c: char| {
                matches!(c, '`' | '"' | '\'' | ',' | ';' | '(' | ')' | '[' | ']')
            })
            .trim_end_matches(':');
        let lower = trimmed.to_ascii_lowercase();
        let looks_like_path = trimmed.contains('/')
            || trimmed.contains('\\')
            || [
                ".rs", ".toml", ".md", ".json", ".yaml", ".yml", ".ts", ".tsx", ".js", ".jsx",
                ".py", ".sh", ".go", ".java",
            ]
            .iter()
            .any(|suffix| lower.ends_with(suffix));
        if looks_like_path {
            sanitize_memory_text_value(trimmed, 90)
        } else {
            None
        }
    })
}

fn extract_url_anchor(text: &str) -> Option<String> {
    text.split_whitespace().find_map(|token| {
        let trimmed = token
            .trim_matches(|c: char| {
                matches!(c, '`' | '"' | '\'' | ',' | ';' | '(' | ')' | '[' | ']')
            })
            .trim_end_matches('.');
        if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            sanitize_memory_text_value(trimmed, 100)
        } else {
            None
        }
    })
}

fn select_facts_for_injection(
    mem: &StructuredMemory,
    current_query: Option<&str>,
) -> Vec<MemoryFact> {
    select_ranked_items(
        &mem.facts,
        current_query,
        MEMORY_INJECTION_MAX_FACTS_WITHOUT_QUERY,
        MEMORY_INJECTION_MAX_RELEVANT_FACTS,
        MEMORY_INJECTION_MAX_FALLBACK_FACTS,
        fact_relevance_score,
        |fact| fact.recorded_at,
        |fact| fact.key.clone(),
    )
}

/// Score a memory fact's relevance to the query tokens.
/// Higher score = more relevant.
fn fact_relevance_score(fact: &MemoryFact, query_tokens: &[String], query_lower: &str) -> usize {
    composite_memory_relevance_score(&[&fact.key, &fact.value], query_tokens, query_lower)
}

fn select_lessons_for_injection(
    mem: &StructuredMemory,
    current_query: Option<&str>,
) -> Vec<MemoryLesson> {
    select_ranked_items(
        &mem.lessons,
        current_query,
        MEMORY_INJECTION_MAX_LESSONS_WITHOUT_QUERY,
        MEMORY_INJECTION_MAX_RELEVANT_LESSONS,
        MEMORY_INJECTION_MAX_FALLBACK_LESSONS,
        lesson_relevance_score,
        |lesson| lesson.last_seen_at,
        lesson_identity,
    )
}

fn lesson_relevance_score(
    lesson: &MemoryLesson,
    query_tokens: &[String],
    query_lower: &str,
) -> usize {
    composite_memory_relevance_score(
        &[
            &lesson.title,
            &lesson.when_to_apply,
            &lesson.recommendation,
            &lesson.scope,
        ],
        query_tokens,
        query_lower,
    )
}

fn select_open_loops_for_injection(
    mem: &StructuredMemory,
    current_query: Option<&str>,
) -> Vec<OpenLoop> {
    let unresolved: Vec<OpenLoop> = mem
        .open_loops
        .iter()
        .filter(|item| !item.status.is_resolved())
        .cloned()
        .collect();
    select_ranked_items(
        &unresolved,
        current_query,
        MEMORY_INJECTION_MAX_OPEN_LOOPS_WITHOUT_QUERY,
        MEMORY_INJECTION_MAX_RELEVANT_OPEN_LOOPS,
        MEMORY_INJECTION_MAX_FALLBACK_OPEN_LOOPS,
        open_loop_relevance_score,
        |item| item.updated_at,
        open_loop_identity,
    )
}

fn open_loop_relevance_score(item: &OpenLoop, query_tokens: &[String], query_lower: &str) -> usize {
    composite_memory_relevance_score(
        &[&item.goal, &item.blocker, &item.next_step],
        query_tokens,
        query_lower,
    )
}

fn select_command_patterns_for_injection(
    mem: &StructuredMemory,
    current_query: Option<&str>,
) -> Vec<CommandPattern> {
    select_ranked_items(
        &mem.command_patterns,
        current_query,
        MEMORY_INJECTION_MAX_COMMAND_PATTERNS_WITHOUT_QUERY,
        MEMORY_INJECTION_MAX_RELEVANT_COMMAND_PATTERNS,
        MEMORY_INJECTION_MAX_FALLBACK_COMMAND_PATTERNS,
        command_pattern_relevance_score,
        |item| item.last_seen_at,
        command_pattern_identity,
    )
}

fn command_pattern_relevance_score(
    item: &CommandPattern,
    query_tokens: &[String],
    query_lower: &str,
) -> usize {
    composite_memory_relevance_score(
        &[&item.signature, &item.purpose, &item.outcome],
        query_tokens,
        query_lower,
    )
}

fn select_project_signals_for_injection(
    mem: &StructuredMemory,
    current_query: Option<&str>,
) -> Vec<ProjectSignal> {
    select_ranked_items(
        &mem.project_signals,
        current_query,
        MEMORY_INJECTION_MAX_PROJECT_SIGNALS_WITHOUT_QUERY,
        MEMORY_INJECTION_MAX_RELEVANT_PROJECT_SIGNALS,
        MEMORY_INJECTION_MAX_FALLBACK_PROJECT_SIGNALS,
        project_signal_relevance_score,
        |signal| signal.recorded_at,
        |signal| signal.key.clone(),
    )
}

fn project_signal_relevance_score(
    signal: &ProjectSignal,
    query_tokens: &[String],
    query_lower: &str,
) -> usize {
    composite_memory_relevance_score(&[&signal.key, &signal.value], query_tokens, query_lower)
}

#[allow(clippy::too_many_arguments)]
fn select_ranked_items<T, FScore, FRecency, FKey>(
    items: &[T],
    current_query: Option<&str>,
    limit_without_query: usize,
    limit_relevant: usize,
    fallback_limit: usize,
    score_fn: FScore,
    recency_fn: FRecency,
    key_fn: FKey,
) -> Vec<T>
where
    T: Clone,
    FScore: Fn(&T, &[String], &str) -> usize,
    FRecency: Fn(&T) -> u64,
    FKey: Fn(&T) -> String,
{
    let mut ranked = items.to_vec();
    if ranked.is_empty() {
        return ranked;
    }

    if let Some(query) = current_query
        .map(str::trim)
        .filter(|query| !query.is_empty())
    {
        let query_tokens = crate::tokenize_for_matching(query);
        let query_lower = query.to_lowercase();
        let mut scored: Vec<(usize, T)> = ranked
            .iter()
            .cloned()
            .map(|item| (score_fn(&item, &query_tokens, &query_lower), item))
            .collect();
        scored.sort_by(|(score_a, item_a), (score_b, item_b)| {
            score_b
                .cmp(score_a)
                .then(recency_fn(item_b).cmp(&recency_fn(item_a)))
                .then_with(|| key_fn(item_a).cmp(&key_fn(item_b)))
        });

        let max_score = scored.first().map(|(score, _)| *score).unwrap_or(0);
        if max_score > 0 {
            let mut selected: Vec<T> = scored
                .iter()
                .filter(|(score, _)| *score > 0)
                .take(limit_relevant)
                .map(|(_, item)| item.clone())
                .collect();
            if selected.len() < limit_relevant && fallback_limit > 0 {
                let selected_keys: HashSet<String> = selected.iter().map(&key_fn).collect();
                let mut fallback: Vec<T> = scored
                    .into_iter()
                    .filter(|(score, item)| *score == 0 && !selected_keys.contains(&key_fn(item)))
                    .map(|(_, item)| item)
                    .collect();
                fallback.sort_by(|a, b| {
                    recency_fn(b)
                        .cmp(&recency_fn(a))
                        .then_with(|| key_fn(a).cmp(&key_fn(b)))
                });
                selected.extend(fallback.into_iter().take(fallback_limit));
            }
            return selected;
        }
    }

    ranked.sort_by(|a, b| {
        recency_fn(b)
            .cmp(&recency_fn(a))
            .then_with(|| key_fn(a).cmp(&key_fn(b)))
    });
    ranked.truncate(limit_without_query);
    ranked
}

fn composite_memory_relevance_score(
    text_parts: &[&str],
    query_tokens: &[String],
    query_lower: &str,
) -> usize {
    if query_tokens.is_empty() {
        return 0;
    }
    let normalized_parts: Vec<String> = text_parts.iter().map(|part| part.to_lowercase()).collect();
    let combined = normalized_parts.join(" ");
    let mut score = 0usize;

    if !query_lower.is_empty() && combined.contains(query_lower) {
        score += 8;
    }

    for token in query_tokens {
        if token.is_empty() {
            continue;
        }
        for part in &normalized_parts {
            if part == token {
                score += 4;
            } else if part.contains(token) {
                score += 2;
            }
        }
    }

    score
}

fn normalize_memory_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_match_text(text: &str) -> String {
    normalize_memory_whitespace(text).to_ascii_lowercase()
}

fn truncate_memory_chars(text: String, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text;
    }
    text.chars().take(max_chars).collect()
}

fn sanitize_memory_text_value(value: &str, max_chars: usize) -> Option<String> {
    let sanitized = truncate_memory_chars(normalize_memory_whitespace(value.trim()), max_chars);
    if sanitized.is_empty() {
        None
    } else {
        Some(sanitized)
    }
}

fn normalize_memory_fact_key(key: &str) -> String {
    let mut out = String::new();
    let mut previous_was_separator = false;
    for ch in normalize_memory_whitespace(key).chars() {
        if ch.is_alphanumeric() {
            out.extend(ch.to_lowercase());
            previous_was_separator = false;
        } else if matches!(ch, ' ' | '_' | '-' | '/' | '.' | ':')
            && !out.is_empty()
            && !previous_was_separator
        {
            out.push('_');
            previous_was_separator = true;
        }
    }
    out.trim_matches('_').to_string()
}

fn sanitize_memory_fact_value(value: &str) -> String {
    sanitize_memory_text_value(value, MAX_MEMORY_FACT_VALUE_CHARS).unwrap_or_default()
}

fn sanitize_user_context_value(value: &str) -> Option<String> {
    sanitize_memory_text_value(value, MAX_MEMORY_USER_CONTEXT_CHARS)
}

fn sanitize_memory_lesson_title(value: &str) -> Option<String> {
    sanitize_memory_text_value(value, MAX_MEMORY_LESSON_TITLE_CHARS)
}

fn sanitize_memory_lesson_when(value: &str) -> Option<String> {
    sanitize_memory_text_value(value, MAX_MEMORY_LESSON_WHEN_CHARS)
}

fn sanitize_memory_lesson_recommendation(value: &str) -> Option<String> {
    sanitize_memory_text_value(value, MAX_MEMORY_LESSON_RECOMMENDATION_CHARS)
}

fn sanitize_memory_scope(value: &str) -> String {
    sanitize_memory_text_value(value, MAX_MEMORY_SCOPE_CHARS).unwrap_or_default()
}

fn sanitize_open_loop_goal(value: &str) -> Option<String> {
    sanitize_memory_text_value(value, MAX_OPEN_LOOP_GOAL_CHARS)
}

fn sanitize_open_loop_blocker(value: &str) -> Option<String> {
    sanitize_memory_text_value(value, MAX_OPEN_LOOP_BLOCKER_CHARS)
}

fn sanitize_open_loop_next_step(value: &str) -> Option<String> {
    sanitize_memory_text_value(value, MAX_OPEN_LOOP_NEXT_STEP_CHARS)
}

fn sanitize_command_signature(value: &str) -> Option<String> {
    sanitize_memory_text_value(value, MAX_COMMAND_SIGNATURE_CHARS)
}

fn sanitize_command_purpose(value: &str) -> Option<String> {
    sanitize_memory_text_value(value, MAX_COMMAND_PURPOSE_CHARS)
}

fn sanitize_command_outcome(value: &str) -> Option<String> {
    sanitize_memory_text_value(value, MAX_COMMAND_OUTCOME_CHARS)
}

fn sanitize_project_signal_value(value: &str) -> String {
    sanitize_memory_text_value(value, MAX_PROJECT_SIGNAL_VALUE_CHARS).unwrap_or_default()
}

fn lesson_identity(lesson: &MemoryLesson) -> String {
    format!(
        "{}|{}",
        normalize_match_text(&lesson.title),
        normalize_match_text(&lesson.when_to_apply)
    )
}

fn open_loop_identity(item: &OpenLoop) -> String {
    normalize_match_text(&item.goal)
}

fn command_pattern_identity(item: &CommandPattern) -> String {
    normalize_match_text(&item.signature)
}

fn dedupe_structured_memory(memory: &mut StructuredMemory) {
    dedupe_memory_facts(&mut memory.facts);
    dedupe_memory_lessons(&mut memory.lessons);
    dedupe_open_loops(&mut memory.open_loops);
    dedupe_command_patterns(&mut memory.command_patterns);
    dedupe_project_signals(&mut memory.project_signals);
}

fn dedupe_memory_facts(facts: &mut Vec<MemoryFact>) {
    let mut deduped: Vec<MemoryFact> = Vec::new();
    for mut fact in std::mem::take(facts) {
        fact.key = normalize_memory_fact_key(&fact.key);
        fact.value = sanitize_memory_fact_value(&fact.value);
        if fact.key.is_empty() || fact.value.is_empty() {
            continue;
        }
        match deduped.iter_mut().find(|existing| existing.key == fact.key) {
            Some(existing) if existing.value == fact.value => {
                existing.recorded_at = existing.recorded_at.max(fact.recorded_at);
            }
            Some(existing) if fact.recorded_at >= existing.recorded_at => {
                *existing = fact;
            }
            Some(_) => {}
            None => {
                deduped.push(fact);
            }
        }
    }
    *facts = deduped;
}

fn dedupe_memory_lessons(lessons: &mut Vec<MemoryLesson>) {
    let mut deduped: Vec<MemoryLesson> = Vec::new();
    for mut lesson in std::mem::take(lessons) {
        lesson.title = sanitize_memory_lesson_title(&lesson.title).unwrap_or_default();
        lesson.when_to_apply =
            sanitize_memory_lesson_when(&lesson.when_to_apply).unwrap_or_default();
        lesson.recommendation =
            sanitize_memory_lesson_recommendation(&lesson.recommendation).unwrap_or_default();
        lesson.scope = sanitize_memory_scope(&lesson.scope);
        if lesson.title.is_empty()
            || lesson.when_to_apply.is_empty()
            || lesson.recommendation.is_empty()
        {
            continue;
        }
        let lesson_key = lesson_identity(&lesson);
        match deduped
            .iter_mut()
            .find(|existing| lesson_identity(existing) == lesson_key)
        {
            Some(existing)
                if existing.recommendation == lesson.recommendation
                    && existing.scope == lesson.scope
                    && existing.confidence == lesson.confidence =>
            {
                existing.last_seen_at = existing.last_seen_at.max(lesson.last_seen_at);
            }
            Some(existing) if lesson.last_seen_at >= existing.last_seen_at => {
                *existing = lesson;
            }
            Some(_) => {}
            None => deduped.push(lesson),
        }
    }
    *lessons = deduped;
}

fn dedupe_open_loops(open_loops: &mut Vec<OpenLoop>) {
    let mut deduped: Vec<OpenLoop> = Vec::new();
    for mut item in std::mem::take(open_loops) {
        item.goal = sanitize_open_loop_goal(&item.goal).unwrap_or_default();
        item.blocker = sanitize_open_loop_blocker(&item.blocker).unwrap_or_default();
        item.next_step = sanitize_open_loop_next_step(&item.next_step).unwrap_or_default();
        if item.goal.is_empty() || item.blocker.is_empty() || item.next_step.is_empty() {
            continue;
        }
        let loop_key = open_loop_identity(&item);
        match deduped
            .iter_mut()
            .find(|existing| open_loop_identity(existing) == loop_key)
        {
            Some(existing)
                if existing.blocker == item.blocker
                    && existing.next_step == item.next_step
                    && existing.status == item.status =>
            {
                existing.updated_at = existing.updated_at.max(item.updated_at);
            }
            Some(existing) if item.updated_at >= existing.updated_at => {
                *existing = item;
            }
            Some(_) => {}
            None => deduped.push(item),
        }
    }
    *open_loops = deduped;
}

fn dedupe_command_patterns(command_patterns: &mut Vec<CommandPattern>) {
    let mut deduped: Vec<CommandPattern> = Vec::new();
    for mut item in std::mem::take(command_patterns) {
        item.signature = sanitize_command_signature(&item.signature).unwrap_or_default();
        item.purpose = sanitize_command_purpose(&item.purpose).unwrap_or_default();
        item.outcome = sanitize_command_outcome(&item.outcome).unwrap_or_default();
        if item.signature.is_empty() || item.purpose.is_empty() || item.outcome.is_empty() {
            continue;
        }
        let pattern_key = command_pattern_identity(&item);
        match deduped
            .iter_mut()
            .find(|existing| command_pattern_identity(existing) == pattern_key)
        {
            Some(existing)
                if existing.purpose == item.purpose
                    && existing.outcome == item.outcome
                    && existing.confidence == item.confidence =>
            {
                existing.last_seen_at = existing.last_seen_at.max(item.last_seen_at);
            }
            Some(existing) if item.last_seen_at >= existing.last_seen_at => {
                *existing = item;
            }
            Some(_) => {}
            None => deduped.push(item),
        }
    }
    *command_patterns = deduped;
}

fn dedupe_project_signals(project_signals: &mut Vec<ProjectSignal>) {
    let mut deduped: Vec<ProjectSignal> = Vec::new();
    for mut item in std::mem::take(project_signals) {
        item.key = normalize_memory_fact_key(&item.key);
        item.value = sanitize_project_signal_value(&item.value);
        if item.key.is_empty() || item.value.is_empty() {
            continue;
        }
        match deduped.iter_mut().find(|existing| existing.key == item.key) {
            Some(existing) if existing.value == item.value => {
                existing.recorded_at = existing.recorded_at.max(item.recorded_at);
            }
            Some(existing) if item.recorded_at >= existing.recorded_at => {
                *existing = item;
            }
            Some(_) => {}
            None => deduped.push(item),
        }
    }
    *project_signals = deduped;
}

// ── Async update queue ──────────────────────────────────────────────────────

/// Payload sent to the background memory updater.
#[derive(Clone)]
struct MemoryUpdateRequest {
    session_id: String,
    workspace: PathBuf,
    model: String,
    /// Immutable runtime configuration captured by the Agent run that
    /// produced this memory update. Model/provider resolution must use this
    /// snapshot rather than the queue's subsequently hot-reloaded config.
    config: Arc<Config>,
    /// Only user messages + final assistant response (no tool noise).
    conversation_excerpt: Vec<crate::ChatMessage>,
}

/// Max pending update requests. Beyond this, new requests replace the latest.
const MEMORY_QUEUE_CAPACITY: usize = 16;

/// Debounced async memory update queue.
/// Receives update requests from the OnFinish hook and processes them
/// in the background with debounce to avoid excessive LLM calls.
#[derive(Clone)]
pub(crate) struct MemoryUpdateQueue {
    tx: mpsc::Sender<MemoryUpdateRequest>,
    status: SharedMemoryQueueStatus,
    config: Arc<Mutex<Arc<Config>>>,
    cancel: CancellationToken,
}

impl MemoryUpdateQueue {
    /// Spawn the background updater task. Returns the queue handle.
    pub(crate) fn spawn(
        config: Config,
        sessions: Arc<AsyncMutex<HashMap<String, Session>>>,
    ) -> Self {
        let (tx, rx) = mpsc::channel(MEMORY_QUEUE_CAPACITY);
        let status = Arc::new(Mutex::new(MemoryQueueStatusSnapshot {
            state: "idle".to_string(),
            ..Default::default()
        }));
        let config = Arc::new(Mutex::new(Arc::new(config)));
        let cancel = CancellationToken::new();
        tokio::spawn(memory_updater_loop(
            rx,
            status.clone(),
            cancel.clone(),
            sessions,
        ));
        Self {
            tx,
            status,
            config,
            cancel,
        }
    }

    pub(crate) fn status_snapshot(&self) -> MemoryQueueStatusSnapshot {
        self.status
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    pub(crate) fn replace_config(&self, new: Config) {
        match self.config.lock() {
            Ok(mut guard) => {
                *guard = Arc::new(new);
            }
            Err(poisoned) => {
                eprintln!("Warning: memory queue config lock poisoned during replace; recovering");
                let mut guard = poisoned.into_inner();
                *guard = Arc::new(new);
            }
        }
    }

    pub(crate) fn shutdown(&self) {
        self.cancel.cancel();
    }

    /// Enqueue a memory update request (non-blocking).
    pub(crate) fn enqueue(
        &self,
        session_id: String,
        workspace: PathBuf,
        model: String,
        config: Arc<Config>,
        conversation_excerpt: Vec<crate::ChatMessage>,
    ) {
        if self.cancel.is_cancelled() {
            return;
        }
        let req = MemoryUpdateRequest {
            session_id,
            workspace,
            model: model.clone(),
            config,
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
    status: SharedMemoryQueueStatus,
    cancel: CancellationToken,
    sessions: Arc<AsyncMutex<HashMap<String, Session>>>,
) {
    let mut pending: Option<MemoryUpdateRequest> = None;

    loop {
        if let Some(req) = pending.take() {
            // Debounce: wait for more requests or timeout
            let final_req = tokio::select! {
                _ = cancel.cancelled() => return,
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

            let config_snapshot = Arc::clone(&final_req.config);
            let memory_timeout = config_snapshot.tool_timeout.max(Duration::from_secs(30));
            let http = Client::builder()
                .timeout(memory_timeout)
                .build()
                .unwrap_or_else(|_| Client::new());

            match tokio::select! {
                _ = cancel.cancelled() => return,
                result = tokio::time::timeout(
                    memory_timeout,
                    process_memory_update(&final_req, &config_snapshot, &http),
                ) => result,
            } {
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
                            entries_before: audit_baseline.entries_before,
                            entries_after: audit_baseline.entries_before,
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
                            entries_before: audit_baseline.entries_before,
                            entries_after: audit_baseline.entries_before,
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
                    if let Some(usage) = stats.usage.as_ref() {
                        let mut sessions = sessions.lock().await;
                        if let Some(session) = sessions.get_mut(&final_req.session_id) {
                            apply_usage_update(session, usage);
                        }
                    }
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
                            entries_before: stats.entries_before,
                            entries_after: stats.entries_after,
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
            match tokio::select! {
                _ = cancel.cancelled() => return,
                next = rx.recv() => next,
            } {
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
fn parse_memory_lesson_update(value: &serde_json::Value, now: u64) -> Option<MemoryLesson> {
    Some(MemoryLesson {
        title: sanitize_memory_lesson_title(
            value.get("title").and_then(|v| v.as_str()).unwrap_or(""),
        )?,
        when_to_apply: sanitize_memory_lesson_when(
            value
                .get("when_to_apply")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
        )?,
        recommendation: sanitize_memory_lesson_recommendation(
            value
                .get("recommendation")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
        )?,
        scope: sanitize_memory_scope(value.get("scope").and_then(|v| v.as_str()).unwrap_or("")),
        confidence: MemoryConfidence::from_raw(value.get("confidence").and_then(|v| v.as_str())),
        last_seen_at: now,
    })
}

fn parse_open_loop_update(value: &serde_json::Value, now: u64) -> Option<OpenLoop> {
    Some(OpenLoop {
        goal: sanitize_open_loop_goal(value.get("goal").and_then(|v| v.as_str()).unwrap_or(""))?,
        blocker: sanitize_open_loop_blocker(
            value.get("blocker").and_then(|v| v.as_str()).unwrap_or(""),
        )?,
        next_step: sanitize_open_loop_next_step(
            value
                .get("next_step")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
        )?,
        status: OpenLoopStatus::from_raw(value.get("status").and_then(|v| v.as_str())),
        updated_at: now,
    })
}

fn parse_command_pattern_update(value: &serde_json::Value, now: u64) -> Option<CommandPattern> {
    Some(CommandPattern {
        signature: sanitize_command_signature(
            value
                .get("signature")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
        )?,
        purpose: sanitize_command_purpose(
            value.get("purpose").and_then(|v| v.as_str()).unwrap_or(""),
        )?,
        outcome: sanitize_command_outcome(
            value.get("outcome").and_then(|v| v.as_str()).unwrap_or(""),
        )?,
        confidence: MemoryConfidence::from_raw(value.get("confidence").and_then(|v| v.as_str())),
        last_seen_at: now,
    })
}

fn parse_project_signal_update(value: &serde_json::Value, now: u64) -> Option<ProjectSignal> {
    let key = normalize_memory_fact_key(value.get("key").and_then(|v| v.as_str()).unwrap_or(""));
    let signal_value =
        sanitize_project_signal_value(value.get("value").and_then(|v| v.as_str()).unwrap_or(""));
    if key.is_empty() || signal_value.is_empty() {
        return None;
    }
    Some(ProjectSignal {
        key,
        value: signal_value,
        recorded_at: now,
    })
}

fn upsert_memory_lesson(lessons: &mut Vec<MemoryLesson>, lesson: MemoryLesson) {
    let lesson_key = lesson_identity(&lesson);
    if let Some(existing) = lessons
        .iter_mut()
        .find(|item| lesson_identity(item) == lesson_key)
    {
        if existing.recommendation != lesson.recommendation
            || existing.scope != lesson.scope
            || existing.confidence != lesson.confidence
        {
            *existing = lesson;
        } else {
            existing.last_seen_at = existing.last_seen_at.max(lesson.last_seen_at);
        }
    } else {
        lessons.push(lesson);
    }
}

fn upsert_open_loop(open_loops: &mut Vec<OpenLoop>, open_loop: OpenLoop) {
    let loop_key = open_loop_identity(&open_loop);
    if let Some(existing) = open_loops
        .iter_mut()
        .find(|item| open_loop_identity(item) == loop_key)
    {
        if existing.blocker != open_loop.blocker
            || existing.next_step != open_loop.next_step
            || existing.status != open_loop.status
        {
            *existing = open_loop;
        } else {
            existing.updated_at = existing.updated_at.max(open_loop.updated_at);
        }
    } else {
        open_loops.push(open_loop);
    }
}

fn upsert_command_pattern(command_patterns: &mut Vec<CommandPattern>, pattern: CommandPattern) {
    let pattern_key = command_pattern_identity(&pattern);
    if let Some(existing) = command_patterns
        .iter_mut()
        .find(|item| command_pattern_identity(item) == pattern_key)
    {
        if existing.purpose != pattern.purpose
            || existing.outcome != pattern.outcome
            || existing.confidence != pattern.confidence
        {
            *existing = pattern;
        } else {
            existing.last_seen_at = existing.last_seen_at.max(pattern.last_seen_at);
        }
    } else {
        command_patterns.push(pattern);
    }
}

fn upsert_project_signal(project_signals: &mut Vec<ProjectSignal>, signal: ProjectSignal) {
    if let Some(existing) = project_signals
        .iter_mut()
        .find(|item| item.key == signal.key)
    {
        if existing.value != signal.value {
            *existing = signal;
        } else {
            existing.recorded_at = existing.recorded_at.max(signal.recorded_at);
        }
    } else {
        project_signals.push(signal);
    }
}

pub(crate) fn merge_llm_response_into_memory(
    memory: &mut StructuredMemory,
    raw: &serde_json::Value,
    now: u64,
) {
    // Normalize any pre-existing records before incremental deletes/upserts so
    // we do not merge on top of stale duplicate keys.
    dedupe_structured_memory(memory);
    // Only touch user_context when the key is actually present in the response.
    // null → clear, string → update, absent → preserve existing.
    if raw.get("user_context").is_some() {
        memory.user_context = raw["user_context"]
            .as_str()
            .and_then(sanitize_user_context_value);
    }

    let used_incremental = [
        "update_facts",
        "delete_facts",
        "update_lessons",
        "delete_lessons",
        "update_open_loops",
        "delete_open_loops",
        "update_command_patterns",
        "delete_command_patterns",
        "update_project_signals",
        "delete_project_signals",
    ]
    .iter()
    .any(|key| raw.get(*key).is_some());

    if used_incremental {
        if let Some(delete_arr) = raw.get("delete_facts").and_then(|v| v.as_array()) {
            let delete_keys: HashSet<String> = delete_arr
                .iter()
                .filter_map(|v| v.as_str())
                .map(normalize_memory_fact_key)
                .filter(|key| !key.is_empty())
                .collect();
            memory.facts.retain(|f| !delete_keys.contains(&f.key));
        }
        if let Some(delete_arr) = raw.get("delete_lessons").and_then(|v| v.as_array()) {
            let delete_keys: HashSet<String> = delete_arr
                .iter()
                .filter_map(|v| v.as_str())
                .map(normalize_match_text)
                .filter(|key| !key.is_empty())
                .collect();
            memory.lessons.retain(|item| {
                let title_key = normalize_match_text(&item.title);
                let lesson_key = lesson_identity(item);
                !delete_keys.contains(&title_key) && !delete_keys.contains(&lesson_key)
            });
        }
        if let Some(delete_arr) = raw.get("delete_open_loops").and_then(|v| v.as_array()) {
            let delete_keys: HashSet<String> = delete_arr
                .iter()
                .filter_map(|v| v.as_str())
                .map(normalize_match_text)
                .filter(|key| !key.is_empty())
                .collect();
            memory
                .open_loops
                .retain(|item| !delete_keys.contains(&open_loop_identity(item)));
        }
        if let Some(delete_arr) = raw
            .get("delete_command_patterns")
            .and_then(|v| v.as_array())
        {
            let delete_keys: HashSet<String> = delete_arr
                .iter()
                .filter_map(|v| v.as_str())
                .map(normalize_match_text)
                .filter(|key| !key.is_empty())
                .collect();
            memory
                .command_patterns
                .retain(|item| !delete_keys.contains(&command_pattern_identity(item)));
        }
        if let Some(delete_arr) = raw.get("delete_project_signals").and_then(|v| v.as_array()) {
            let delete_keys: HashSet<String> = delete_arr
                .iter()
                .filter_map(|v| v.as_str())
                .map(normalize_memory_fact_key)
                .filter(|key| !key.is_empty())
                .collect();
            memory
                .project_signals
                .retain(|item| !delete_keys.contains(&item.key));
        }

        if let Some(update_arr) = raw.get("update_facts").and_then(|v| v.as_array()) {
            for fv in update_arr {
                let key = normalize_memory_fact_key(
                    fv.get("key").and_then(|v| v.as_str()).unwrap_or("").trim(),
                );
                let value = sanitize_memory_fact_value(
                    fv.get("value")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim(),
                );
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
        if let Some(update_arr) = raw.get("update_lessons").and_then(|v| v.as_array()) {
            for value in update_arr {
                if let Some(lesson) = parse_memory_lesson_update(value, now) {
                    upsert_memory_lesson(&mut memory.lessons, lesson);
                }
            }
        }
        if let Some(update_arr) = raw.get("update_open_loops").and_then(|v| v.as_array()) {
            for value in update_arr {
                if let Some(open_loop) = parse_open_loop_update(value, now) {
                    upsert_open_loop(&mut memory.open_loops, open_loop);
                }
            }
        }
        if let Some(update_arr) = raw
            .get("update_command_patterns")
            .and_then(|v| v.as_array())
        {
            for value in update_arr {
                if let Some(pattern) = parse_command_pattern_update(value, now) {
                    upsert_command_pattern(&mut memory.command_patterns, pattern);
                }
            }
        }
        if let Some(update_arr) = raw.get("update_project_signals").and_then(|v| v.as_array()) {
            for value in update_arr {
                if let Some(signal) = parse_project_signal_update(value, now) {
                    upsert_project_signal(&mut memory.project_signals, signal);
                }
            }
        }
    } else {
        if let Some(facts_arr) = raw.get("facts").and_then(|v| v.as_array()) {
            let mut new_facts = Vec::new();
            for fv in facts_arr {
                let key = normalize_memory_fact_key(
                    fv.get("key").and_then(|v| v.as_str()).unwrap_or("").trim(),
                );
                let value = sanitize_memory_fact_value(
                    fv.get("value")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim(),
                );
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
        if let Some(lessons_arr) = raw.get("lessons").and_then(|v| v.as_array()) {
            let mut new_lessons = Vec::new();
            for value in lessons_arr {
                if let Some(mut lesson) = parse_memory_lesson_update(value, now) {
                    lesson.last_seen_at = memory
                        .lessons
                        .iter()
                        .find(|item| {
                            lesson_identity(item) == lesson_identity(&lesson)
                                && item.recommendation == lesson.recommendation
                                && item.scope == lesson.scope
                                && item.confidence == lesson.confidence
                        })
                        .map(|item| item.last_seen_at)
                        .unwrap_or(now);
                    new_lessons.push(lesson);
                }
            }
            memory.lessons = new_lessons;
        }
        if let Some(open_loops_arr) = raw.get("open_loops").and_then(|v| v.as_array()) {
            let mut new_open_loops = Vec::new();
            for value in open_loops_arr {
                if let Some(mut open_loop) = parse_open_loop_update(value, now) {
                    open_loop.updated_at = memory
                        .open_loops
                        .iter()
                        .find(|item| {
                            open_loop_identity(item) == open_loop_identity(&open_loop)
                                && item.blocker == open_loop.blocker
                                && item.next_step == open_loop.next_step
                                && item.status == open_loop.status
                        })
                        .map(|item| item.updated_at)
                        .unwrap_or(now);
                    new_open_loops.push(open_loop);
                }
            }
            memory.open_loops = new_open_loops;
        }
        if let Some(patterns_arr) = raw.get("command_patterns").and_then(|v| v.as_array()) {
            let mut new_patterns = Vec::new();
            for value in patterns_arr {
                if let Some(mut pattern) = parse_command_pattern_update(value, now) {
                    pattern.last_seen_at = memory
                        .command_patterns
                        .iter()
                        .find(|item| {
                            command_pattern_identity(item) == command_pattern_identity(&pattern)
                                && item.purpose == pattern.purpose
                                && item.outcome == pattern.outcome
                                && item.confidence == pattern.confidence
                        })
                        .map(|item| item.last_seen_at)
                        .unwrap_or(now);
                    new_patterns.push(pattern);
                }
            }
            memory.command_patterns = new_patterns;
        }
        if let Some(signals_arr) = raw.get("project_signals").and_then(|v| v.as_array()) {
            let mut new_signals = Vec::new();
            for value in signals_arr {
                if let Some(mut signal) = parse_project_signal_update(value, now) {
                    signal.recorded_at = memory
                        .project_signals
                        .iter()
                        .find(|item| item.key == signal.key && item.value == signal.value)
                        .map(|item| item.recorded_at)
                        .unwrap_or(now);
                    new_signals.push(signal);
                }
            }
            memory.project_signals = new_signals;
        }
    }

    dedupe_structured_memory(memory);
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
    let entries_before = existing.entry_count();
    let had_user_context_before = existing.user_context.is_some();

    // Build conversation excerpt text
    let excerpt = build_conversation_excerpt(&req.conversation_excerpt);
    let excerpt_chars = excerpt.chars().count();
    if excerpt.trim().is_empty() {
        return Ok(MemoryProcessStats {
            excerpt_chars,
            facts_before,
            facts_after: facts_before,
            entries_before,
            entries_after: entries_before,
            had_user_context_before,
            had_user_context_after: had_user_context_before,
            changed: false,
            usage: None,
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
1. Extract any new user preferences, key decisions, project context, reusable lessons, unresolved loops, helpful command patterns, or important facts from the conversation.
2. Return ONLY the changes needed — do not repeat unchanged facts.
3. Update user_context if the user reveals preferences, background, or working style. Omit user_context from your response if it hasn't changed. Set to null to clear it.
4. Return ONLY valid JSON matching this schema (no markdown fences, no explanation):

{{"user_context": "string or null (omit if unchanged)", "update_facts": [{{"key": "short_label", "value": "content"}}], "delete_facts": ["key_to_remove"], "update_lessons": [{{"title": "short title", "when_to_apply": "situation", "recommendation": "what to do", "scope": "repo|tool|workflow|general", "confidence": "low|medium|high"}}], "delete_lessons": ["lesson title"], "update_open_loops": [{{"goal": "unfinished goal", "blocker": "what is stuck", "next_step": "best next step", "status": "open|in_progress|resolved"}}], "delete_open_loops": ["goal"], "update_command_patterns": [{{"signature": "command or tool pattern", "purpose": "why it helps", "outcome": "what happened", "confidence": "low|medium|high"}}], "delete_command_patterns": ["signature"], "update_project_signals": [{{"key": "short_label", "value": "project signal"}}], "delete_project_signals": ["key"]}}

- `update_facts`: Stable facts or decisions that do not fit a richer type.
- `update_lessons`: Reusable guidance inferred from what worked or failed. Store lessons that could help future runs, not one-off narration.
- `update_open_loops`: Unresolved tasks, blockers, or follow-up items that may matter later. Mark `status` as `resolved` only when the conversation clearly closes the loop.
- `update_command_patterns`: Commands or tool usage patterns that proved useful, risky, or diagnostic in this workspace.
- `update_project_signals`: Stable project anchors such as build systems, entrypoints, test commands, or important file locations.
- Only delete when you are certain the old entry is outdated or contradicted.
- If there is nothing meaningful to extract, return: {{"update_facts": [], "delete_facts": []}}
Keep all entries concise. Do not store ephemeral task chatter; prefer stable, reusable knowledge.
Keep durable knowledge only; skip ephemeral task details."#
    );

    let messages = vec![
        crate::ChatMessage {
            role: "system".into(),
            content: Some(system_prompt),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        crate::ChatMessage {
            role: "user".into(),
            content: Some(format!("Conversation to analyze:\n\n{excerpt}")),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
    ];

    let resolved = config.resolve_model(&req.model);
    let response = providers::call_llm_simple_with_usage(
        http,
        &resolved,
        &messages,
        &req.workspace,
        config.s3.as_ref(),
        "off",
        config.max_llm_retries,
    )
    .await
    .map_err(|e| format!("LLM call failed: {e}"))?;

    let provider_name = config.resolve_provider_name(&req.model);
    let input_tokens = response.input_tokens.unwrap_or_else(|| {
        crate::estimate_tokens_for_provider(resolved.provider, &messages) as u64
    });
    let output_tokens = response.output_tokens.unwrap_or_else(|| {
        crate::message_token_len_for_provider(
            resolved.provider,
            &crate::ChatMessage {
                role: "assistant".into(),
                content: Some(response.content.clone()),
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: None,
                tool_call_id: None,
                timestamp: None,
            },
        ) as u64
    });
    let usage = UsageUpdate {
        input_tokens,
        output_tokens,
        input_source: if response.input_tokens.is_some() {
            "provider".to_string()
        } else {
            "estimated".to_string()
        },
        output_source: if response.output_tokens.is_some() {
            "provider".to_string()
        } else {
            "estimated".to_string()
        },
        labels: build_usage_labels(
            input_tokens,
            output_tokens,
            Some(&provider_name),
            Some(USAGE_ROLE_MEMORY),
        ),
    };

    let response = response.content.trim().to_string();
    if response.is_empty() {
        return Ok(MemoryProcessStats {
            excerpt_chars,
            facts_before,
            facts_after: facts_before,
            entries_before,
            entries_after: entries_before,
            had_user_context_before,
            had_user_context_after: had_user_context_before,
            changed: false,
            usage: Some(usage),
        });
    }

    // Strip markdown fences if present
    let json_str = crate::strip_json_fences(&response);

    // Parse as raw Value first so we can distinguish "field absent" from
    // "field explicitly null" — prevents silent data loss when the LLM
    // returns incomplete JSON.
    let raw: serde_json::Value =
        serde_json::from_str(json_str).map_err(|e| format!("parse LLM response: {e}"))?;

    let now = now_epoch_secs();

    let mut merged = existing;
    let before_json = serde_json::to_string(&merged).unwrap_or_default();

    merge_llm_response_into_memory(&mut merged, &raw, now);

    // Cap total per-category memory to prevent unbounded growth.
    const MAX_FACTS: usize = 50;
    if merged.facts.len() > MAX_FACTS {
        merged
            .facts
            .sort_by(|a, b| b.recorded_at.cmp(&a.recorded_at));
        merged.facts.truncate(MAX_FACTS);
    }
    if merged.lessons.len() > MAX_MEMORY_LESSONS {
        merged.lessons.sort_by(|a, b| {
            b.last_seen_at
                .cmp(&a.last_seen_at)
                .then(a.title.cmp(&b.title))
        });
        merged.lessons.truncate(MAX_MEMORY_LESSONS);
    }
    if merged.open_loops.len() > MAX_MEMORY_OPEN_LOOPS {
        merged.open_loops.sort_by(|a, b| {
            b.status
                .rank()
                .cmp(&a.status.rank())
                .then(b.updated_at.cmp(&a.updated_at))
                .then(a.goal.cmp(&b.goal))
        });
        merged.open_loops.truncate(MAX_MEMORY_OPEN_LOOPS);
    }
    if merged.command_patterns.len() > MAX_MEMORY_COMMAND_PATTERNS {
        merged.command_patterns.sort_by(|a, b| {
            b.last_seen_at
                .cmp(&a.last_seen_at)
                .then(a.signature.cmp(&b.signature))
        });
        merged
            .command_patterns
            .truncate(MAX_MEMORY_COMMAND_PATTERNS);
    }
    if merged.project_signals.len() > MAX_MEMORY_PROJECT_SIGNALS {
        merged
            .project_signals
            .sort_by(|a, b| b.recorded_at.cmp(&a.recorded_at).then(a.key.cmp(&b.key)));
        merged.project_signals.truncate(MAX_MEMORY_PROJECT_SIGNALS);
    }

    let facts_after = merged.facts.len();
    let entries_after = merged.entry_count();
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
        entries_before,
        entries_after,
        had_user_context_before,
        had_user_context_after,
        changed,
        usage: Some(usage),
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

// ── Memory status ───────────────────────────────────────────────────────────

/// Build a human-readable status summary of structured memory.
pub(crate) fn memory_status(workspace: &Path) -> String {
    let mem = load_structured_memory(workspace);
    if mem.is_empty() {
        return "Structured memory: empty (will populate after first conversation)".to_string();
    }

    let mut lines = Vec::new();
    let mut counts = Vec::new();
    if !mem.facts.is_empty() {
        counts.push(format!("{} facts", mem.facts.len()));
    }
    if !mem.lessons.is_empty() {
        counts.push(format!("{} lessons", mem.lessons.len()));
    }
    if !mem.open_loops.is_empty() {
        counts.push(format!("{} open loops", mem.open_loops.len()));
    }
    if !mem.command_patterns.is_empty() {
        counts.push(format!("{} command patterns", mem.command_patterns.len()));
    }
    if !mem.project_signals.is_empty() {
        counts.push(format!("{} project signals", mem.project_signals.len()));
    }
    lines.push(format!("**Structured Memory** ({})", counts.join(", ")));

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

    if !mem.lessons.is_empty() {
        lines.push("Lessons:".to_string());
        for (i, lesson) in mem.lessons.iter().enumerate() {
            lines.push(format!(
                "  {}. **{}** [{}]: when {}; {}",
                i + 1,
                lesson.title,
                lesson.confidence.label(),
                truncate_inline(&lesson.when_to_apply, 60),
                truncate_inline(&lesson.recommendation, 80)
            ));
        }
    }

    if !mem.open_loops.is_empty() {
        lines.push("Open loops:".to_string());
        for (i, item) in mem.open_loops.iter().enumerate() {
            lines.push(format!(
                "  {}. **{}** [{}]: next {}",
                i + 1,
                item.goal,
                item.status.label(),
                truncate_inline(&item.next_step, 80)
            ));
        }
    }

    if !mem.command_patterns.is_empty() {
        lines.push("Command patterns:".to_string());
        for (i, item) in mem.command_patterns.iter().enumerate() {
            lines.push(format!(
                "  {}. `{}` [{}]: {}",
                i + 1,
                truncate_inline(&item.signature, 60),
                item.confidence.label(),
                truncate_inline(&item.outcome, 80)
            ));
        }
    }

    if !mem.project_signals.is_empty() {
        lines.push("Project signals:".to_string());
        for (i, item) in mem.project_signals.iter().enumerate() {
            lines.push(format!(
                "  {}. **{}**: {}",
                i + 1,
                item.key,
                truncate_inline(&item.value, 80)
            ));
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
