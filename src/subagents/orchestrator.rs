// ══════════════════════════════════════════════════════════════════════════════
//  Sub-Agent Orchestrator
//
//  Coordinates multi-agent workflows defined as a DAG (directed acyclic graph).
//  Tasks without mutual dependencies execute in parallel; dependent tasks
//  wait for upstream results, which are injected via {{results.<id>}} placeholders.
//
//  Example serial:   coder → reviewer → coder
//  Example parallel: (explore + researcher) → coder  (both run first, coder waits)
//  Example mixed:    explore → coder → reviewer → coder  (serial chain)
//                    researcher ──────↗                   (parallel with explore→coder)
// ══════════════════════════════════════════════════════════════════════════════

use std::collections::{HashMap, HashSet};
use std::path::Path;

use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::executor::run_subagent;
use crate::hooks::HookRegistry;
use crate::{Config, LiveTx, live_send, truncate};

/// Maximum tasks in a single orchestration plan.
const MAX_ORCHESTRATION_TASKS: usize = 20;

/// Total character budget for the aggregated orchestration result.
const MAX_TOTAL_RESULT_CHARS: usize = 50_000;

/// Maximum characters for a single task's result in the aggregated output.
const MAX_PER_TASK_RESULT_CHARS: usize = 15_000;

/// Task IDs must remain compatible with `{{results.<id>}}` placeholders.
pub(crate) fn is_valid_task_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

// ── Types ────────────────────────────────────────────────────────────────────

/// Individual task in an orchestration plan.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct OrchestrationTask {
    /// Unique identifier within this orchestration plan.
    pub id: String,
    /// Name of the sub-agent to execute this task.
    pub agent: String,
    /// Task prompt. May contain `{{results.<task_id>}}` placeholders that are
    /// resolved with outputs from completed upstream dependencies.
    pub prompt: String,
    /// IDs of tasks that must complete before this one starts.
    #[serde(default)]
    pub depends_on: Vec<String>,
}

/// Validated orchestration plan (guaranteed acyclic, valid agents, unique IDs).
#[derive(Clone, Debug)]
pub(crate) struct OrchestrationPlan {
    pub tasks: Vec<OrchestrationTask>,
}

/// Result of a single orchestrated task.
#[derive(Clone, Debug)]
pub(crate) struct TaskResult {
    pub id: String,
    pub agent: String,
    pub status: TaskStatus,
    pub result: String,
    pub cycles: usize,
    pub tool_calls: usize,
    pub duration_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub provider_usage: HashMap<String, [u64; 2]>,
}

/// Status of an individual task in the orchestration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TaskStatus {
    Completed,
    Failed,
    Skipped,
}

/// Outcome of a full orchestration run.
pub(crate) struct OrchestrationOutcome {
    pub task_results: Vec<TaskResult>,
    pub aborted: bool,
}

impl OrchestrationOutcome {
    pub(crate) fn has_non_completed_tasks(&self) -> bool {
        self.task_results
            .iter()
            .any(|result| result.status != TaskStatus::Completed)
    }

    /// Total input tokens consumed across all sub-agent calls in this run.
    pub(crate) fn total_input_tokens(&self) -> u64 {
        self.task_results
            .iter()
            .map(|r| r.input_tokens)
            .fold(0u64, u64::saturating_add)
    }

    /// Total output tokens consumed across all sub-agent calls in this run.
    pub(crate) fn total_output_tokens(&self) -> u64 {
        self.task_results
            .iter()
            .map(|r| r.output_tokens)
            .fold(0u64, u64::saturating_add)
    }

    pub(crate) fn provider_usage(&self) -> HashMap<String, [u64; 2]> {
        let mut totals: HashMap<String, [u64; 2]> = HashMap::new();
        for result in &self.task_results {
            for (label, [input_tokens, output_tokens]) in &result.provider_usage {
                let entry = totals.entry(label.clone()).or_insert([0, 0]);
                entry[0] = entry[0].saturating_add(*input_tokens);
                entry[1] = entry[1].saturating_add(*output_tokens);
            }
        }
        totals
    }
}

/// Drop guard that sends an `orchestrate_task_failed` event if a task future is
/// dropped after `orchestrate_task_started` but before a terminal task event.
/// Uses `try_send` because `Drop` cannot await.
struct OrchestrateTaskEventGuard<'a> {
    live_tx: &'a LiveTx,
    orchestrate_id: String,
    task_id: String,
    agent: String,
    finished: bool,
}

impl<'a> OrchestrateTaskEventGuard<'a> {
    fn new(live_tx: &'a LiveTx, orchestrate_id: &str, task_id: &str, agent: &str) -> Self {
        Self {
            live_tx,
            orchestrate_id: orchestrate_id.to_string(),
            task_id: task_id.to_string(),
            agent: agent.to_string(),
            finished: false,
        }
    }

    fn mark_finished(&mut self) {
        self.finished = true;
    }
}

impl Drop for OrchestrateTaskEventGuard<'_> {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.live_tx.try_send(json!({
                "type": "orchestrate_task_failed",
                "orchestrate_id": self.orchestrate_id,
                "id": self.task_id,
                "agent": self.agent,
                "error": "task aborted (timeout or cancellation)",
            }));
        }
    }
}

/// Generate a short random identifier that disambiguates concurrent
/// orchestration runs in the frontend. 8 bytes of entropy = 16 hex chars.
fn generate_orchestrate_id() -> String {
    let mut bytes = [0u8; 8];
    if getrandom::getrandom(&mut bytes).is_err() {
        // Fallback: timestamp-based id. Collision risk is negligible because
        // we only need uniqueness within an active session.
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        return format!("orch-{ts:x}");
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ── Validation ───────────────────────────────────────────────────────────────

/// Validate and build an orchestration plan.
///
/// Checks: unique task IDs, valid agent names, valid dependency references,
/// no self-dependencies, and no cycles in the dependency graph.
pub(crate) fn validate_plan(
    tasks: Vec<OrchestrationTask>,
    workspace: &Path,
) -> Result<OrchestrationPlan, String> {
    if tasks.is_empty() {
        return Err("orchestrate error: at least one task is required".into());
    }
    if tasks.len() > MAX_ORCHESTRATION_TASKS {
        return Err(format!(
            "orchestrate error: too many tasks ({}, max {MAX_ORCHESTRATION_TASKS})",
            tasks.len(),
        ));
    }

    // Unique IDs
    let mut ids = HashSet::new();
    for task in &tasks {
        if task.id.is_empty() {
            return Err("orchestrate error: task id cannot be empty".into());
        }
        if !is_valid_task_id(&task.id) {
            return Err(format!(
                "orchestrate error: task id '{}' must use only ASCII letters, digits, '_' or '-'",
                task.id
            ));
        }
        if !ids.insert(&task.id) {
            return Err(format!(
                "orchestrate error: duplicate task id '{}'",
                task.id
            ));
        }
    }

    // Valid agent names
    let agents = crate::subagents::discovery::discover_all_agents(workspace);
    for task in &tasks {
        if !agents.iter().any(|a| a.name == task.agent) {
            let names: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
            return Err(format!(
                "orchestrate error: agent '{}' not found for task '{}'. Available: {}",
                task.agent,
                task.id,
                if names.is_empty() {
                    "(none)".to_string()
                } else {
                    names.join(", ")
                }
            ));
        }
    }

    // Valid dependency references (no self-deps, all targets exist)
    for task in &tasks {
        for dep in &task.depends_on {
            if dep == &task.id {
                return Err(format!(
                    "orchestrate error: task '{}' depends on itself",
                    task.id
                ));
            }
            if !ids.contains(dep) {
                return Err(format!(
                    "orchestrate error: task '{}' depends on unknown task '{dep}'",
                    task.id,
                ));
            }
        }
    }

    // Cycle detection (DFS)
    if has_cycle(&tasks) {
        return Err("orchestrate error: dependency cycle detected in task graph".into());
    }

    Ok(OrchestrationPlan { tasks })
}

/// DFS-based cycle detection on the dependency graph.
pub(crate) fn has_cycle(tasks: &[OrchestrationTask]) -> bool {
    let id_to_index: HashMap<&str, usize> = tasks
        .iter()
        .enumerate()
        .map(|(i, t)| (t.id.as_str(), i))
        .collect();

    let n = tasks.len();
    // 0 = unvisited, 1 = in current DFS path, 2 = fully explored
    let mut state = vec![0u8; n];

    fn dfs(
        node: usize,
        tasks: &[OrchestrationTask],
        id_to_index: &HashMap<&str, usize>,
        state: &mut [u8],
    ) -> bool {
        state[node] = 1;
        for dep in &tasks[node].depends_on {
            if let Some(&dep_idx) = id_to_index.get(dep.as_str()) {
                match state[dep_idx] {
                    1 => return true, // back-edge → cycle
                    0 => {
                        if dfs(dep_idx, tasks, id_to_index, state) {
                            return true;
                        }
                    }
                    _ => {} // already explored
                }
            }
        }
        state[node] = 2;
        false
    }

    for i in 0..n {
        if state[i] == 0 && dfs(i, tasks, &id_to_index, &mut state) {
            return true;
        }
    }
    false
}

// ── Execution Layers ─────────────────────────────────────────────────────────

/// Compute execution layers using Kahn's algorithm.
///
/// Each layer contains task indices whose dependencies are all satisfied by
/// previous layers. Tasks within the same layer execute in parallel.
pub(crate) fn compute_execution_layers(plan: &OrchestrationPlan) -> Vec<Vec<usize>> {
    let n = plan.tasks.len();
    let id_to_index: HashMap<&str, usize> = plan
        .tasks
        .iter()
        .enumerate()
        .map(|(i, t)| (t.id.as_str(), i))
        .collect();

    let mut in_degree = vec![0usize; n];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];

    for (i, task) in plan.tasks.iter().enumerate() {
        in_degree[i] = task.depends_on.len();
        for dep in &task.depends_on {
            if let Some(&dep_idx) = id_to_index.get(dep.as_str()) {
                dependents[dep_idx].push(i);
            }
        }
    }

    let mut layers = Vec::new();
    let mut ready: Vec<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();

    while !ready.is_empty() {
        layers.push(ready.clone());
        let mut next_ready = Vec::new();
        for &idx in &ready {
            for &dep_idx in &dependents[idx] {
                in_degree[dep_idx] -= 1;
                if in_degree[dep_idx] == 0 {
                    next_ready.push(dep_idx);
                }
            }
        }
        ready = next_ready;
    }

    layers
}

// ── Result Interpolation ─────────────────────────────────────────────────────

/// Replace `{{results.<task_id>}}` placeholders with completed task outputs.
///
/// Single-pass scanner: deterministic and does NOT re-expand placeholders that
/// appear inside substituted values (e.g. if a task result text happens to
/// contain `{{results.X}}`, it is emitted verbatim).
/// Unknown or malformed placeholders are preserved as-is.
pub(crate) fn interpolate_results(prompt: &str, completed: &HashMap<String, String>) -> String {
    const OPEN: &str = "{{results.";
    const CLOSE: &str = "}}";
    let mut out = String::with_capacity(prompt.len());
    let mut rest = prompt;
    while let Some(start) = rest.find(OPEN) {
        out.push_str(&rest[..start]);
        let after_open = &rest[start + OPEN.len()..];
        match after_open.find(CLOSE) {
            Some(end) => {
                let id = &after_open[..end];
                if is_valid_task_id(id)
                    && let Some(value) = completed.get(id)
                {
                    out.push_str(value);
                } else {
                    // Unresolved / malformed: preserve verbatim
                    out.push_str(OPEN);
                    out.push_str(id);
                    out.push_str(CLOSE);
                }
                rest = &after_open[end + CLOSE.len()..];
            }
            None => {
                // No closing "}}" — emit rest verbatim and stop scanning
                out.push_str(OPEN);
                out.push_str(after_open);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

// ── Main Execution ───────────────────────────────────────────────────────────

/// Execute a validated orchestration plan.
///
/// Tasks are executed layer-by-layer: within each layer, tasks with no mutual
/// dependencies run concurrently via `futures::future::join_all`. Results from
/// completed tasks are injected into downstream prompts via `{{results.<id>}}`
/// interpolation.
///
/// If a task fails, all transitive dependents are skipped. Independent branches
/// continue executing.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_orchestration(
    plan: &OrchestrationPlan,
    config: &Config,
    http: &Client,
    workspace: &Path,
    live_tx: &LiveTx,
    cancel: CancellationToken,
    hooks: &HookRegistry,
) -> OrchestrationOutcome {
    let orchestration_start = std::time::Instant::now();
    let orchestrate_id = generate_orchestrate_id();
    let layers = compute_execution_layers(plan);
    let mut completed_results: HashMap<String, String> = HashMap::new();
    let mut failed_ids: HashSet<String> = HashSet::new();
    let mut task_results: Vec<TaskResult> = Vec::new();
    let mut aborted = false;

    // ── orchestrate_started event ────────────────────────────────────────
    let task_summary: Vec<serde_json::Value> = plan
        .tasks
        .iter()
        .map(|t| {
            json!({
                "id": t.id,
                "agent": t.agent,
                "depends_on": t.depends_on,
                "prompt_preview": truncate(&t.prompt, 240),
            })
        })
        .collect();
    let _ = live_send(
        live_tx,
        json!({
            "type": "orchestrate_started",
            "orchestrate_id": orchestrate_id,
            "task_count": plan.tasks.len(),
            "layer_count": layers.len(),
            "tasks": task_summary,
        }),
    )
    .await;

    // ── Execute layer by layer ───────────────────────────────────────────
    for (layer_idx, layer) in layers.iter().enumerate() {
        if cancel.is_cancelled() {
            aborted = true;
            mark_remaining_skipped(
                plan,
                &mut task_results,
                &mut failed_ids,
                live_tx,
                "Orchestration cancelled",
                &orchestrate_id,
            )
            .await;
            break;
        }

        // Layer progress event
        let layer_task_ids: Vec<&str> = layer.iter().map(|&i| plan.tasks[i].id.as_str()).collect();
        let _ = live_send(
            live_tx,
            json!({
                "type": "orchestrate_layer",
                "orchestrate_id": orchestrate_id,
                "layer": layer_idx + 1,
                "total_layers": layers.len(),
                "tasks": layer_task_ids,
            }),
        )
        .await;

        // Partition layer into runnable vs skipped (failed dependency)
        let mut runnable: Vec<usize> = Vec::new();
        for &idx in layer {
            let task = &plan.tasks[idx];
            let failed_dep = task.depends_on.iter().find(|d| failed_ids.contains(*d));
            if let Some(dep) = failed_dep {
                let reason = format!("dependency '{dep}' failed");
                let _ = live_send(
                    live_tx,
                    json!({
                        "type": "orchestrate_task_skipped",
                        "orchestrate_id": orchestrate_id,
                        "id": task.id,
                        "agent": task.agent,
                        "reason": reason,
                    }),
                )
                .await;
                failed_ids.insert(task.id.clone());
                task_results.push(TaskResult {
                    id: task.id.clone(),
                    agent: task.agent.clone(),
                    status: TaskStatus::Skipped,
                    result: format!("Skipped: dependency '{dep}' failed"),
                    cycles: 0,
                    tool_calls: 0,
                    duration_ms: 0,
                    input_tokens: 0,
                    output_tokens: 0,
                    provider_usage: HashMap::new(),
                });
            } else {
                runnable.push(idx);
            }
        }

        if runnable.is_empty() {
            continue;
        }

        // Execute runnable tasks — single directly, multiple in parallel.
        let layer_task_results = if runnable.len() == 1 {
            let idx = runnable[0];
            let result = execute_single_task(
                &plan.tasks[idx],
                config,
                http,
                workspace,
                live_tx,
                cancel.child_token(),
                hooks,
                &completed_results,
                &orchestrate_id,
            )
            .await;
            vec![(idx, result)]
        } else {
            execute_parallel_tasks(
                &runnable,
                plan,
                config,
                http,
                workspace,
                live_tx,
                &cancel,
                hooks,
                &completed_results,
                &orchestrate_id,
            )
            .await
        };

        // Record layer results in plan order
        for (_, result) in layer_task_results {
            match result.status {
                TaskStatus::Completed => {
                    completed_results.insert(result.id.clone(), result.result.clone());
                }
                TaskStatus::Failed => {
                    failed_ids.insert(result.id.clone());
                }
                TaskStatus::Skipped => {
                    failed_ids.insert(result.id.clone());
                }
            }
            task_results.push(result);
        }

        if cancel.is_cancelled() {
            aborted = true;
            mark_remaining_skipped(
                plan,
                &mut task_results,
                &mut failed_ids,
                live_tx,
                "Orchestration cancelled",
                &orchestrate_id,
            )
            .await;
            break;
        }
    }

    // ── orchestrate_completed event ──────────────────────────────────────
    let completed_count = task_results
        .iter()
        .filter(|r| r.status == TaskStatus::Completed)
        .count();
    let failed_count = task_results
        .iter()
        .filter(|r| r.status == TaskStatus::Failed)
        .count();
    let skipped_count = task_results
        .iter()
        .filter(|r| r.status == TaskStatus::Skipped)
        .count();
    // Wall-clock duration of the orchestration. Summing task durations would
    // overcount parallel layers.
    let total_duration_ms = orchestration_start.elapsed().as_millis() as u64;
    let total_input_tokens: u64 = task_results
        .iter()
        .map(|r| r.input_tokens)
        .fold(0u64, u64::saturating_add);
    let total_output_tokens: u64 = task_results
        .iter()
        .map(|r| r.output_tokens)
        .fold(0u64, u64::saturating_add);

    let _ = live_send(
        live_tx,
        json!({
            "type": "orchestrate_completed",
            "orchestrate_id": orchestrate_id,
            "completed": completed_count,
            "failed": failed_count,
            "skipped": skipped_count,
            "total_tasks": plan.tasks.len(),
            "input_tokens": total_input_tokens,
            "output_tokens": total_output_tokens,
            "duration_ms": total_duration_ms,
            "aborted": aborted,
        }),
    )
    .await;

    OrchestrationOutcome {
        task_results,
        aborted,
    }
}

/// Mark all tasks not yet in `task_results` as skipped and add TaskResult entries.
async fn mark_remaining_skipped(
    plan: &OrchestrationPlan,
    task_results: &mut Vec<TaskResult>,
    failed_ids: &mut HashSet<String>,
    live_tx: &LiveTx,
    reason: &str,
    orchestrate_id: &str,
) {
    let processed: HashSet<String> = task_results.iter().map(|r| r.id.clone()).collect();
    for task in &plan.tasks {
        if !processed.contains(&task.id) {
            let _ = live_send(
                live_tx,
                json!({
                    "type": "orchestrate_task_skipped",
                    "orchestrate_id": orchestrate_id,
                    "id": task.id,
                    "agent": task.agent,
                    "reason": reason,
                }),
            )
            .await;
            failed_ids.insert(task.id.clone());
            task_results.push(TaskResult {
                id: task.id.clone(),
                agent: task.agent.clone(),
                status: TaskStatus::Skipped,
                result: format!("Skipped: {reason}"),
                cycles: 0,
                tool_calls: 0,
                duration_ms: 0,
                input_tokens: 0,
                output_tokens: 0,
                provider_usage: HashMap::new(),
            });
        }
    }
}

/// Execute a single orchestrated task.
#[allow(clippy::too_many_arguments)]
async fn execute_single_task(
    task: &OrchestrationTask,
    config: &Config,
    http: &Client,
    workspace: &Path,
    live_tx: &LiveTx,
    cancel: CancellationToken,
    hooks: &HookRegistry,
    completed_results: &HashMap<String, String>,
    orchestrate_id: &str,
) -> TaskResult {
    let start = std::time::Instant::now();

    // Look up agent spec
    let spec = match crate::subagents::discovery::find_agent(workspace, &task.agent) {
        Some(s) => s,
        None => {
            let _ = live_send(
                live_tx,
                json!({
                    "type": "orchestrate_task_failed",
                    "orchestrate_id": orchestrate_id,
                    "id": task.id,
                    "agent": task.agent,
                    "error": format!("agent '{}' not found", task.agent),
                }),
            )
            .await;
            return TaskResult {
                id: task.id.clone(),
                agent: task.agent.clone(),
                status: TaskStatus::Failed,
                result: format!("Agent '{}' not found", task.agent),
                cycles: 0,
                tool_calls: 0,
                duration_ms: start.elapsed().as_millis() as u64,
                input_tokens: 0,
                output_tokens: 0,
                provider_usage: HashMap::new(),
            };
        }
    };

    // Interpolate dependency results into prompt
    let resolved_prompt = interpolate_results(&task.prompt, completed_results);

    // Send task started event
    let _ = live_send(
        live_tx,
        json!({
            "type": "orchestrate_task_started",
            "orchestrate_id": orchestrate_id,
            "id": task.id,
            "agent": task.agent,
            "prompt": truncate(&resolved_prompt, 500),
        }),
    )
    .await;
    let mut guard = OrchestrateTaskEventGuard::new(live_tx, orchestrate_id, &task.id, &task.agent);

    // Compose a composite task_id so forwarded inner events (task_progress /
    // task_tool / tool_result with `subagent` tag) can still be disambiguated
    // in the frontend even when an orchestration runs the same agent twice.
    let composite_task_id = format!("{orchestrate_id}:{}", task.id);

    // Run sub-agent
    let outcome = run_subagent(
        &spec,
        &resolved_prompt,
        config,
        http,
        workspace,
        live_tx,
        cancel,
        hooks,
        &composite_task_id,
    )
    .await;

    let duration_ms = start.elapsed().as_millis() as u64;

    if outcome.aborted {
        let _ = live_send(
            live_tx,
            json!({
                "type": "orchestrate_task_failed",
                "orchestrate_id": orchestrate_id,
                "id": task.id,
                "agent": task.agent,
                "error": outcome.result,
                "cycles": outcome.cycles,
                "tool_calls": outcome.tool_calls,
                "input_tokens": outcome.total_input_tokens,
                "output_tokens": outcome.total_output_tokens,
                "duration_ms": duration_ms,
            }),
        )
        .await;
        guard.mark_finished();
        TaskResult {
            id: task.id.clone(),
            agent: task.agent.clone(),
            status: TaskStatus::Failed,
            result: outcome.result,
            cycles: outcome.cycles,
            tool_calls: outcome.tool_calls,
            duration_ms,
            input_tokens: outcome.total_input_tokens,
            output_tokens: outcome.total_output_tokens,
            provider_usage: outcome.provider_usage,
        }
    } else {
        let _ = live_send(
            live_tx,
            json!({
                "type": "orchestrate_task_completed",
                "orchestrate_id": orchestrate_id,
                "id": task.id,
                "agent": task.agent,
                "cycles": outcome.cycles,
                "tool_calls": outcome.tool_calls,
                "input_tokens": outcome.total_input_tokens,
                "output_tokens": outcome.total_output_tokens,
                "duration_ms": duration_ms,
                "result_excerpt": truncate(&outcome.result, 1_500),
            }),
        )
        .await;
        guard.mark_finished();
        TaskResult {
            id: task.id.clone(),
            agent: task.agent.clone(),
            status: TaskStatus::Completed,
            result: outcome.result,
            cycles: outcome.cycles,
            tool_calls: outcome.tool_calls,
            duration_ms,
            input_tokens: outcome.total_input_tokens,
            output_tokens: outcome.total_output_tokens,
            provider_usage: outcome.provider_usage,
        }
    }
}

/// Execute multiple tasks in parallel using `futures::future::join_all`.
///
/// Each task gets its own child cancellation token. All tasks borrow shared
/// references from the enclosing scope (config, http, workspace, etc.).
#[allow(clippy::too_many_arguments)]
async fn execute_parallel_tasks(
    runnable: &[usize],
    plan: &OrchestrationPlan,
    config: &Config,
    http: &Client,
    workspace: &Path,
    live_tx: &LiveTx,
    cancel: &CancellationToken,
    hooks: &HookRegistry,
    completed_results: &HashMap<String, String>,
    orchestrate_id: &str,
) -> Vec<(usize, TaskResult)> {
    let futures: Vec<_> = runnable
        .iter()
        .map(|&idx| {
            let task = &plan.tasks[idx];
            let child_cancel = cancel.child_token();
            async move {
                let result = execute_single_task(
                    task,
                    config,
                    http,
                    workspace,
                    live_tx,
                    child_cancel,
                    hooks,
                    completed_results,
                    orchestrate_id,
                )
                .await;
                (idx, result)
            }
        })
        .collect();

    futures::future::join_all(futures).await
}

// ── Result Formatting ────────────────────────────────────────────────────────

/// Format the orchestration outcome into a structured Markdown report.
///
/// The report is injected into the parent agent's observation phase, so it
/// uses a character budget to avoid context overflow.
pub(crate) fn format_orchestration_result(outcome: &OrchestrationOutcome) -> String {
    let total = outcome.task_results.len();
    let completed = outcome
        .task_results
        .iter()
        .filter(|r| r.status == TaskStatus::Completed)
        .count();
    let failed = outcome
        .task_results
        .iter()
        .filter(|r| r.status == TaskStatus::Failed)
        .count();
    let skipped = outcome
        .task_results
        .iter()
        .filter(|r| r.status == TaskStatus::Skipped)
        .count();

    let header = format!(
        "## Orchestration {status}\n\n\
         **{total} tasks**: {completed} completed, {failed} failed, {skipped} skipped",
        status = if outcome.aborted {
            "Aborted"
        } else {
            "Complete"
        },
    );

    let mut parts = vec![header];

    // Allocate per-task character budget proportionally
    let header_budget = 200 * total;
    let available = MAX_TOTAL_RESULT_CHARS
        .saturating_sub(header_budget)
        .saturating_sub(parts[0].len());
    let result_tasks = outcome
        .task_results
        .iter()
        .filter(|r| r.status != TaskStatus::Skipped)
        .count()
        .max(1);
    let per_task_budget = (available / result_tasks).min(MAX_PER_TASK_RESULT_CHARS);

    for (i, result) in outcome.task_results.iter().enumerate() {
        let status_icon = match result.status {
            TaskStatus::Completed => "✅",
            TaskStatus::Failed => "❌",
            TaskStatus::Skipped => "⏭️",
        };

        let meta = match result.status {
            TaskStatus::Completed | TaskStatus::Failed => {
                let secs = result.duration_ms as f64 / 1000.0;
                format!(
                    " ({} cycles, {} tools, {secs:.1}s)",
                    result.cycles, result.tool_calls,
                )
            }
            TaskStatus::Skipped => String::new(),
        };

        let truncated_result = truncate(&result.result, per_task_budget);
        parts.push(format!(
            "### [{}] {} ({}) — {status_icon}{meta}\n\n{truncated_result}",
            i + 1,
            result.id,
            result.agent,
        ));
    }

    let full = parts.join("\n\n");
    truncate(&full, MAX_TOTAL_RESULT_CHARS).to_string()
}
