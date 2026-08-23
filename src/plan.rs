use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub(crate) const MAX_PLAN_BYTES: usize = 64 * 1024;
pub(crate) const MAX_PLAN_STEPS: usize = 12;
pub(crate) const MAX_PLAN_QUESTIONS: usize = 5;
pub(crate) const MAX_PLAN_EVIDENCE: usize = 256;
pub(crate) const MAX_PLAN_COMPLETION_CHECKS: usize = 24;
const MAX_PLAN_COMPLETION_EXACT_CONTENT_BYTES: usize = 8 * 1024;
const MAX_PLAN_COMPLETION_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PLAN_COMPLETION_DIRECTORY_ENTRIES: usize = 100_000;
const MAX_INITIAL_PLACEHOLDER_GOAL_BYTES: usize = 4_000;
const INITIAL_IMAGE_PLACEHOLDER_GOAL: &str = "Prepare a plan using the attached image input.";
const MAX_EVIDENCE_HASH_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_EVIDENCE_HASH_TOTAL_BYTES: u64 = 32 * 1024 * 1024;
pub(crate) const TOOL_NAME_SUBMIT_PLAN: &str = "submit_plan";
pub(crate) const TOOL_NAME_UPDATE_PLAN: &str = "update_plan";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PlanStatus {
    Planning,
    NeedsInput,
    #[default]
    Ready,
    Executing,
    Completed,
    Failed,
    Stopped,
    Discarded,
}

impl PlanStatus {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Planning => "planning",
            Self::NeedsInput => "needs_input",
            Self::Ready => "ready",
            Self::Executing => "executing",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
            Self::Discarded => "discarded",
        }
    }

    pub(crate) fn is_active(self) -> bool {
        matches!(
            self,
            Self::Planning | Self::NeedsInput | Self::Ready | Self::Executing
        )
    }

    /// Whether the plan can still continue or be revised and therefore keeps
    /// ownership of the Session's current working directory.
    pub(crate) fn blocks_workspace_rebind(self) -> bool {
        matches!(
            self,
            Self::Planning
                | Self::NeedsInput
                | Self::Ready
                | Self::Executing
                | Self::Failed
                | Self::Stopped
        )
    }

    pub(crate) fn can_receive_feedback(self) -> bool {
        matches!(
            self,
            Self::NeedsInput | Self::Ready | Self::Failed | Self::Stopped
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PlanStepStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
    Blocked,
    Skipped,
}

impl PlanStepStatus {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Blocked => "blocked",
            Self::Skipped => "skipped",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlanStep {
    pub(crate) id: String,
    pub(crate) title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) affected_areas: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlanQuestionOption {
    pub(crate) id: String,
    pub(crate) label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) description: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlanQuestion {
    pub(crate) id: String,
    pub(crate) prompt: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) options: Vec<PlanQuestionOption>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PlanContractSection {
    Verification,
    AcceptanceCriteria,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlanContractClauseRef {
    pub(crate) section: PlanContractSection,
    /// Zero-based index into the immutable section on the approved artifact.
    pub(crate) index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PlanCompletionCheckKind {
    WorkspacePath,
    ApprovedEvidenceUnchanged,
    PlanProgress,
    ToolCallSuccess,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PlanExpectedPathType {
    File,
    Directory,
    Absent,
}

/// Immutable, server-verifiable evidence required before an approved revision
/// can be marked completed. The shape is deliberately flat so every supported
/// Provider receives the same JSON-schema subset; kind-specific validation
/// rejects unused or contradictory fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlanCompletionCheck {
    pub(crate) id: String,
    pub(crate) step_id: String,
    pub(crate) covers: Vec<PlanContractClauseRef>,
    pub(crate) kind: PlanCompletionCheckKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) expected_path_type: Option<PlanExpectedPathType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) exact_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) evidence_kind: Option<PlanEvidenceKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) required_step_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) arguments: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlanArtifact {
    #[serde(default = "plan_artifact_schema_version")]
    pub(crate) schema_version: u32,
    pub(crate) title: String,
    pub(crate) goal: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) steps: Vec<PlanStep>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) assumptions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) risks: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) verification: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) acceptance_criteria: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) completion_checks: Vec<PlanCompletionCheck>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) questions: Vec<PlanQuestion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) legacy_markdown: Option<String>,
}

impl Default for PlanArtifact {
    fn default() -> Self {
        Self {
            schema_version: plan_artifact_schema_version(),
            title: String::new(),
            goal: String::new(),
            summary: String::new(),
            steps: Vec::new(),
            assumptions: Vec::new(),
            risks: Vec::new(),
            verification: Vec::new(),
            acceptance_criteria: Vec::new(),
            completion_checks: Vec::new(),
            questions: Vec::new(),
            legacy_markdown: None,
        }
    }
}

fn plan_artifact_schema_version() -> u32 {
    1
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlanProgressStep {
    pub(crate) id: String,
    pub(crate) title: String,
    #[serde(default)]
    pub(crate) status: PlanStepStatus,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) note: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) deviation_reason: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PlanEvidenceKind {
    File,
    Directory,
    DirectoryTree,
    Git,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlanEvidence {
    pub(crate) path: String,
    pub(crate) kind: PlanEvidenceKind,
    pub(crate) fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) selector: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CapturedPlanEvidence {
    pub(crate) evidence: Vec<PlanEvidence>,
    pub(crate) truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct PendingPlan {
    pub(crate) id: String,
    pub(crate) original_user_message_index: usize,
    pub(crate) assistant_plan_message_index: usize,
    pub(crate) created_at: u64,
    #[serde(default = "default_revision")]
    pub(crate) revision: u32,
    #[serde(default)]
    pub(crate) status: PlanStatus,
    #[serde(default)]
    pub(crate) artifact: PlanArtifact,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) progress: Vec<PlanProgressStep>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) evidence: Vec<PlanEvidence>,
    #[serde(default)]
    pub(crate) evidence_truncated: bool,
    #[serde(default)]
    pub(crate) updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) approved_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) finished_at: Option<u64>,
    #[serde(default)]
    pub(crate) execution_attempt: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) stale_override_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) stale_override_confirmed_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pending_feedback: Option<String>,
    /// True only while the initial Plan-only run has not submitted its first
    /// artifact. This must be persisted: message pruning can collapse both
    /// message anchors to zero, so anchor equality is not a safe substitute.
    #[serde(default)]
    pub(crate) initial_submission_pending: bool,
}

impl Default for PendingPlan {
    fn default() -> Self {
        Self {
            id: String::new(),
            original_user_message_index: 0,
            assistant_plan_message_index: 0,
            created_at: 0,
            revision: 1,
            status: PlanStatus::Ready,
            artifact: PlanArtifact::default(),
            progress: Vec::new(),
            evidence: Vec::new(),
            evidence_truncated: false,
            updated_at: 0,
            approved_at: None,
            finished_at: None,
            execution_attempt: 0,
            stale_override_paths: Vec::new(),
            stale_override_confirmed_at: None,
            pending_feedback: None,
            initial_submission_pending: false,
        }
    }
}

fn default_revision() -> u32 {
    1
}

impl PendingPlan {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        id: String,
        original_user_message_index: usize,
        assistant_plan_message_index: usize,
        created_at: u64,
        revision: u32,
        status: PlanStatus,
        artifact: PlanArtifact,
        evidence: Vec<PlanEvidence>,
        evidence_truncated: bool,
    ) -> Self {
        let progress = artifact
            .steps
            .iter()
            .map(|step| PlanProgressStep {
                id: step.id.clone(),
                title: step.title.clone(),
                ..PlanProgressStep::default()
            })
            .collect();
        Self {
            id,
            original_user_message_index,
            assistant_plan_message_index,
            created_at,
            revision,
            status,
            artifact,
            progress,
            evidence,
            evidence_truncated,
            updated_at: created_at,
            approved_at: None,
            finished_at: None,
            execution_attempt: 0,
            stale_override_paths: Vec::new(),
            stale_override_confirmed_at: None,
            pending_feedback: None,
            initial_submission_pending: false,
        }
    }

    pub(crate) fn normalize_legacy(&mut self, messages: &[crate::ChatMessage]) {
        if self.revision == 0 {
            self.revision = 1;
        }
        if self.updated_at == 0 {
            self.updated_at = self.created_at;
        }
        if self.artifact.schema_version == 0 {
            self.artifact.schema_version = 1;
        }
        if self.artifact.title.trim().is_empty() {
            let markdown = messages
                .get(self.assistant_plan_message_index)
                .and_then(|message| message.content.clone())
                .unwrap_or_else(|| "Approved plan".to_string());
            self.artifact = legacy_artifact(&markdown);
        }
        if self.progress.is_empty() {
            self.progress = self
                .artifact
                .steps
                .iter()
                .map(|step| PlanProgressStep {
                    id: step.id.clone(),
                    title: step.title.clone(),
                    ..PlanProgressStep::default()
                })
                .collect();
        }
    }

    pub(crate) fn to_live_value(&self) -> Value {
        let unfinished_steps = self.unfinished_step_count();
        json!({
            "plan_id": self.id,
            "revision": self.revision,
            "status": self.status,
            "message_index": self.assistant_plan_message_index,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
            "approved_at": self.approved_at,
            "finished_at": self.finished_at,
            "execution_attempt": self.execution_attempt,
            "artifact": self.artifact,
            "progress": self.progress,
            "evidence_count": self.evidence.len(),
            "evidence_truncated": self.evidence_truncated,
            "stale_override_paths": self.stale_override_paths,
            "stale_override_confirmed_at": self.stale_override_confirmed_at,
            "pending_feedback": self.pending_feedback,
            "initial_submission_pending": self.initial_submission_pending,
            "initial_request_image_only": self.initial_submission_pending
                && self.artifact.goal == INITIAL_IMAGE_PLACEHOLDER_GOAL,
            "unfinished_steps": unfinished_steps,
            "run_finished_with_unreported_steps": self.status == PlanStatus::Completed && unfinished_steps > 0,
        })
    }

    pub(crate) fn unfinished_step_count(&self) -> usize {
        self.progress
            .iter()
            .filter(|step| {
                !matches!(
                    step.status,
                    PlanStepStatus::Completed | PlanStepStatus::Skipped
                )
            })
            .count()
    }

    pub(crate) fn approved_prompt_section(&self) -> String {
        let mut output = String::from("## Approved Execution Plan\n\n");
        output.push_str("Plan ID: `");
        output.push_str(&self.id);
        output.push_str("`\nRevision: ");
        output.push_str(&self.revision.to_string());
        output.push_str("\n\n");
        output.push_str(&canonical_markdown(&self.artifact));
        if !self.progress.is_empty() {
            output.push_str("\n\n### Current execution progress\n");
            for step in &self.progress {
                let is_adaptation = !self
                    .artifact
                    .steps
                    .iter()
                    .any(|artifact_step| artifact_step.id == step.id);
                output.push_str("\n- `");
                output.push_str(&step.id);
                output.push_str("` [");
                output.push_str(step.status.label());
                output.push_str("] ");
                output.push_str(&step.title);
                if is_adaptation {
                    output.push_str(" (runtime adaptation)");
                }
                if !step.note.is_empty() {
                    output.push_str(" — note: ");
                    output.push_str(&step.note);
                }
                if let Some(reason) = step.deviation_reason.as_deref() {
                    output.push_str(" — deviation reason: ");
                    output.push_str(reason);
                }
            }
        }
        output.push_str(
            "\n\nFollow this exact approved revision as an immutable execution contract. Its goal, original step constraints, verification, acceptance criteria, and server completion checks take precedence over broader assumptions such as following the current contents of a stale input. `allow_stale` permits execution in the changed environment; it does not approve a refresh, reinterpretation, or replacement of this revision. Use `update_plan` to report progress. You may append an adaptation step only to help satisfy this contract, must include a deviation reason, and must block the affected original step when new evidence conflicts with the contract. Do not change or replace the approved goal, constraints, verification, acceptance criteria, or completion checks.",
        );
        output
    }

    pub(crate) fn rebase_message_indices_after_prefix_prune(&mut self, removed: usize) {
        fn rebase(index: &mut usize, removed: usize) {
            if *index == 0 {
                return;
            }
            *index = if *index <= removed {
                0
            } else {
                index.saturating_sub(removed)
            };
        }

        rebase(&mut self.original_user_message_index, removed);
        rebase(&mut self.assistant_plan_message_index, removed);
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PlanSubmissionState {
    NeedsInput,
    Ready,
}

#[derive(Clone, Debug)]
pub(crate) struct PlanSubmission {
    pub(crate) state: PlanSubmissionState,
    pub(crate) artifact: PlanArtifact,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlanProgressUpdate {
    pub(crate) base_revision: u32,
    #[serde(default)]
    pub(crate) updates: Vec<PlanStepUpdate>,
    #[serde(default)]
    pub(crate) append_steps: Vec<PlanAppendedStep>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlanStepUpdate {
    pub(crate) id: String,
    pub(crate) status: PlanStepStatus,
    #[serde(default)]
    pub(crate) note: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlanAppendedStep {
    pub(crate) id: String,
    pub(crate) title: String,
    #[serde(default)]
    pub(crate) note: String,
    pub(crate) deviation_reason: String,
}

pub(crate) fn validate_submission_json(args: &str) -> Result<PlanSubmission, String> {
    if args.len() > MAX_PLAN_BYTES {
        return Err(format!("plan exceeds the {MAX_PLAN_BYTES}-byte limit"));
    }
    let value: Value =
        serde_json::from_str(args).map_err(|error| format!("invalid plan JSON: {error}"))?;
    let mut object = value
        .as_object()
        .cloned()
        .ok_or_else(|| "plan must be a JSON object".to_string())?;
    let state = object
        .remove("state")
        .ok_or_else(|| "plan state is required".to_string())
        .and_then(|value| {
            serde_json::from_value(value).map_err(|error| format!("invalid plan state: {error}"))
        })?;
    let artifact = serde_json::from_value(Value::Object(object))
        .map_err(|error| format!("invalid plan JSON: {error}"))?;
    let mut submission = PlanSubmission { state, artifact };
    normalize_artifact(&mut submission.artifact);
    validate_artifact(&submission.artifact, submission.state)?;
    validate_submitted_completion_contract(&submission.artifact, submission.state)?;
    Ok(submission)
}

pub(crate) fn validate_progress_json(args: &str) -> Result<PlanProgressUpdate, String> {
    if args.len() > MAX_PLAN_BYTES {
        return Err(format!(
            "plan update exceeds the {MAX_PLAN_BYTES}-byte limit"
        ));
    }
    let mut update: PlanProgressUpdate =
        serde_json::from_str(args).map_err(|error| format!("invalid plan update JSON: {error}"))?;
    if update.updates.is_empty() && update.append_steps.is_empty() {
        return Err("a plan update must change or append at least one step".to_string());
    }
    if update
        .updates
        .len()
        .saturating_add(update.append_steps.len())
        > MAX_PLAN_STEPS
    {
        return Err(format!(
            "a plan update may contain at most {MAX_PLAN_STEPS} step changes"
        ));
    }
    for step in &mut update.updates {
        step.id = step.id.trim().to_string();
        step.note = step.note.trim().to_string();
        validate_identifier("step id", &step.id)?;
        validate_text("step note", &step.note, 0, 2_000)?;
    }
    for step in &mut update.append_steps {
        step.id = step.id.trim().to_string();
        step.title = step.title.trim().to_string();
        step.note = step.note.trim().to_string();
        step.deviation_reason = step.deviation_reason.trim().to_string();
        validate_identifier("appended step id", &step.id)?;
        validate_text("appended step title", &step.title, 1, 240)?;
        validate_text("appended step note", &step.note, 0, 2_000)?;
        validate_text(
            "appended step deviation_reason",
            &step.deviation_reason,
            1,
            2_000,
        )?;
    }
    Ok(update)
}

pub(crate) fn apply_progress_update(
    plan: &mut PendingPlan,
    update: PlanProgressUpdate,
) -> Result<(), String> {
    if plan.status != PlanStatus::Executing {
        return Err("the plan is not currently executing".to_string());
    }
    if update.base_revision != plan.revision {
        return Err(format!(
            "stale_plan_revision: expected revision {}, received {}",
            plan.revision, update.base_revision
        ));
    }

    let mut next_progress = plan.progress.clone();
    let mut seen = HashSet::new();
    for change in update.updates {
        if !seen.insert(change.id.clone()) {
            return Err(format!("duplicate update for step '{}'", change.id));
        }
        let Some(step) = next_progress.iter_mut().find(|step| step.id == change.id) else {
            return Err(format!("unknown plan step '{}'", change.id));
        };
        step.status = change.status;
        step.note = change.note;
    }

    for appended in update.append_steps {
        if next_progress.len() >= MAX_PLAN_STEPS {
            return Err(format!("a plan may contain at most {MAX_PLAN_STEPS} steps"));
        }
        if next_progress.iter().any(|step| step.id == appended.id)
            || !seen.insert(appended.id.clone())
        {
            return Err(format!("duplicate plan step '{}'", appended.id));
        }
        next_progress.push(PlanProgressStep {
            id: appended.id,
            title: appended.title,
            status: PlanStepStatus::Pending,
            note: appended.note,
            deviation_reason: Some(appended.deviation_reason),
        });
    }
    plan.progress = next_progress;
    Ok(())
}

fn normalize_contract_path(path: &str) -> String {
    let mut normalized = path.trim().replace('\\', "/");
    while let Some(stripped) = normalized.strip_prefix("./") {
        normalized = stripped.to_string();
    }
    #[cfg(windows)]
    {
        normalized.make_ascii_lowercase();
    }
    normalized
}

fn validate_completion_checks(
    artifact: &PlanArtifact,
    require_complete_coverage: bool,
) -> Result<(), String> {
    if artifact.completion_checks.len() > MAX_PLAN_COMPLETION_CHECKS {
        return Err(format!(
            "completion_checks must contain at most {MAX_PLAN_COMPLETION_CHECKS} items"
        ));
    }

    let artifact_step_ids = artifact
        .steps
        .iter()
        .map(|step| step.id.as_str())
        .collect::<HashSet<_>>();
    let mut check_ids = HashSet::new();
    let mut covered = HashSet::new();
    let mut path_expectations = BTreeMap::<
        String,
        (
            PlanExpectedPathType,
            Option<String>,
            Option<u64>,
            Option<String>,
        ),
    >::new();

    for check in &artifact.completion_checks {
        validate_identifier("completion check id", &check.id)?;
        if !check_ids.insert(check.id.as_str()) {
            return Err(format!("duplicate completion check id '{}'", check.id));
        }
        validate_identifier("completion check step_id", &check.step_id)?;
        if !artifact_step_ids.contains(check.step_id.as_str()) {
            return Err(format!(
                "completion check '{}' must bind to an original approved step",
                check.id
            ));
        }
        if check.covers.is_empty() {
            return Err(format!(
                "completion check '{}' must cover at least one verification or acceptance criterion",
                check.id
            ));
        }
        if check.covers.len() > 24 {
            return Err(format!(
                "completion check '{}' covers too many contract clauses",
                check.id
            ));
        }
        let mut local_coverage = HashSet::new();
        for clause in &check.covers {
            let count = match clause.section {
                PlanContractSection::Verification => artifact.verification.len(),
                PlanContractSection::AcceptanceCriteria => artifact.acceptance_criteria.len(),
            };
            if clause.index >= count {
                return Err(format!(
                    "completion check '{}' references missing {:?} item {}",
                    check.id, clause.section, clause.index
                ));
            }
            if !local_coverage.insert((clause.section, clause.index)) {
                return Err(format!(
                    "completion check '{}' repeats a covered contract clause",
                    check.id
                ));
            }
            // `plan_progress` is Agent-reported state. It remains useful as an
            // additional completion gate, but it cannot prove an immutable
            // verification or acceptance clause by itself.
            if check.kind != PlanCompletionCheckKind::PlanProgress {
                covered.insert((clause.section, clause.index));
            }
        }

        match check.kind {
            PlanCompletionCheckKind::WorkspacePath => {
                let path = check
                    .path
                    .as_deref()
                    .ok_or_else(|| format!("workspace_path check '{}' requires path", check.id))?;
                validate_text("completion check path", path, 1, 4_096)?;
                let expected_type = check.expected_path_type.ok_or_else(|| {
                    format!(
                        "workspace_path check '{}' requires expected_path_type",
                        check.id
                    )
                })?;
                if check.evidence_kind.is_some()
                    || !check.required_step_ids.is_empty()
                    || check.tool_name.is_some()
                    || check.arguments.is_some()
                {
                    return Err(format!(
                        "workspace_path check '{}' contains fields for another check kind",
                        check.id
                    ));
                }
                if expected_type != PlanExpectedPathType::File
                    && (check.exact_content.is_some()
                        || check.size_bytes.is_some()
                        || check.sha256.is_some())
                {
                    return Err(format!(
                        "workspace_path check '{}' may use content, size, or sha256 only for a file",
                        check.id
                    ));
                }
                if let Some(content) = check.exact_content.as_deref() {
                    if content.len() > MAX_PLAN_COMPLETION_EXACT_CONTENT_BYTES {
                        return Err(format!(
                            "completion check exact_content exceeds the {MAX_PLAN_COMPLETION_EXACT_CONTENT_BYTES}-byte limit"
                        ));
                    }
                    if check
                        .size_bytes
                        .is_some_and(|size| size != content.len() as u64)
                    {
                        return Err(format!(
                            "workspace_path check '{}' has a size_bytes value that contradicts exact_content",
                            check.id
                        ));
                    }
                    if let Some(expected_hash) = check.sha256.as_deref() {
                        let actual_hash = format!("{:x}", Sha256::digest(content.as_bytes()));
                        if !actual_hash.eq_ignore_ascii_case(expected_hash) {
                            return Err(format!(
                                "workspace_path check '{}' has a sha256 value that contradicts exact_content",
                                check.id
                            ));
                        }
                    }
                }
                if let Some(hash) = check.sha256.as_deref()
                    && (hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
                {
                    return Err(format!(
                        "workspace_path check '{}' sha256 must contain 64 hexadecimal characters",
                        check.id
                    ));
                }

                let path_key = normalize_contract_path(path);
                let normalized_hash = check.sha256.as_ref().map(|hash| hash.to_ascii_lowercase());
                let expectation = path_expectations
                    .entry(path_key)
                    .or_insert_with(|| (expected_type, None, None, None));
                let contradicts = expectation.0 != expected_type
                    || expectation
                        .1
                        .as_ref()
                        .zip(check.exact_content.as_ref())
                        .is_some_and(|(left, right)| left != right)
                    || expectation
                        .2
                        .zip(check.size_bytes)
                        .is_some_and(|(left, right)| left != right)
                    || expectation
                        .3
                        .as_ref()
                        .zip(normalized_hash.as_ref())
                        .is_some_and(|(left, right)| left != right);
                if contradicts {
                    return Err(format!(
                        "completion checks contain contradictory expectations for path '{path}'"
                    ));
                }
                if expectation.1.is_none() {
                    expectation.1 = check.exact_content.clone();
                }
                if expectation.2.is_none() {
                    expectation.2 = check.size_bytes;
                }
                if expectation.3.is_none() {
                    expectation.3 = normalized_hash;
                }
                if let Some(content) = expectation.1.as_deref() {
                    let merged_hash = format!("{:x}", Sha256::digest(content.as_bytes()));
                    if expectation
                        .2
                        .is_some_and(|size| size != content.len() as u64)
                        || expectation
                            .3
                            .as_deref()
                            .is_some_and(|hash| !hash.eq_ignore_ascii_case(&merged_hash))
                    {
                        return Err(format!(
                            "completion checks contain contradictory expectations for path '{path}'"
                        ));
                    }
                }
            }
            PlanCompletionCheckKind::ApprovedEvidenceUnchanged => {
                let path = check.path.as_deref().ok_or_else(|| {
                    format!(
                        "approved_evidence_unchanged check '{}' requires path",
                        check.id
                    )
                })?;
                validate_text("completion check path", path, 1, 4_096)?;
                let evidence_kind = check.evidence_kind.ok_or_else(|| {
                    format!(
                        "approved_evidence_unchanged check '{}' requires evidence_kind",
                        check.id
                    )
                })?;
                if evidence_kind == PlanEvidenceKind::Git {
                    return Err(format!(
                        "approved_evidence_unchanged check '{}' does not support Git evidence",
                        check.id
                    ));
                }
                if check.expected_path_type.is_some()
                    || check.exact_content.is_some()
                    || check.size_bytes.is_some()
                    || check.sha256.is_some()
                    || !check.required_step_ids.is_empty()
                    || check.tool_name.is_some()
                    || check.arguments.is_some()
                {
                    return Err(format!(
                        "approved_evidence_unchanged check '{}' contains fields for another check kind",
                        check.id
                    ));
                }
            }
            PlanCompletionCheckKind::PlanProgress => {
                if check.required_step_ids.is_empty() {
                    return Err(format!(
                        "plan_progress check '{}' requires required_step_ids",
                        check.id
                    ));
                }
                let mut required_ids = HashSet::new();
                for step_id in &check.required_step_ids {
                    validate_identifier("required progress step id", step_id)?;
                    if !artifact_step_ids.contains(step_id.as_str()) {
                        return Err(format!(
                            "plan_progress check '{}' references unknown approved step '{}'",
                            check.id, step_id
                        ));
                    }
                    if !required_ids.insert(step_id.as_str()) {
                        return Err(format!(
                            "plan_progress check '{}' repeats step '{}'",
                            check.id, step_id
                        ));
                    }
                }
                if check.path.is_some()
                    || check.expected_path_type.is_some()
                    || check.exact_content.is_some()
                    || check.size_bytes.is_some()
                    || check.sha256.is_some()
                    || check.evidence_kind.is_some()
                    || check.tool_name.is_some()
                    || check.arguments.is_some()
                {
                    return Err(format!(
                        "plan_progress check '{}' contains fields for another check kind",
                        check.id
                    ));
                }
            }
            PlanCompletionCheckKind::ToolCallSuccess => {
                let tool_name = check.tool_name.as_deref().ok_or_else(|| {
                    format!("tool_call_success check '{}' requires tool_name", check.id)
                })?;
                validate_text("completion check tool_name", tool_name, 1, 256)?;
                if matches!(tool_name, TOOL_NAME_SUBMIT_PLAN | TOOL_NAME_UPDATE_PLAN) {
                    return Err(format!(
                        "tool_call_success check '{}' cannot target an internal Plan tool",
                        check.id
                    ));
                }
                if !check.arguments.as_ref().is_some_and(Value::is_object) {
                    return Err(format!(
                        "tool_call_success check '{}' requires object arguments",
                        check.id
                    ));
                }
                if check.path.is_some()
                    || check.expected_path_type.is_some()
                    || check.exact_content.is_some()
                    || check.size_bytes.is_some()
                    || check.sha256.is_some()
                    || check.evidence_kind.is_some()
                    || !check.required_step_ids.is_empty()
                {
                    return Err(format!(
                        "tool_call_success check '{}' contains fields for another check kind",
                        check.id
                    ));
                }
            }
        }
    }

    if require_complete_coverage {
        for index in 0..artifact.verification.len() {
            if !covered.contains(&(PlanContractSection::Verification, index)) {
                return Err(format!(
                    "verification item {index} is missing a server-verifiable completion check"
                ));
            }
        }
        for index in 0..artifact.acceptance_criteria.len() {
            if !covered.contains(&(PlanContractSection::AcceptanceCriteria, index)) {
                return Err(format!(
                    "acceptance_criteria item {index} is missing a server-verifiable completion check"
                ));
            }
        }
    }
    Ok(())
}

fn validate_submitted_completion_contract(
    artifact: &PlanArtifact,
    state: PlanSubmissionState,
) -> Result<(), String> {
    match state {
        PlanSubmissionState::NeedsInput => {
            if !artifact.completion_checks.is_empty() {
                return Err("needs_input cannot include completion_checks".to_string());
            }
        }
        PlanSubmissionState::Ready => {
            if artifact.acceptance_criteria.is_empty() {
                return Err("a ready plan must contain acceptance_criteria".to_string());
            }
            if artifact.completion_checks.is_empty() {
                return Err(
                    "a ready plan must bind its verification and acceptance criteria to completion_checks"
                        .to_string(),
                );
            }
            validate_completion_checks(artifact, true)?;
            if artifact.completion_checks.iter().any(|check| {
                check.kind == PlanCompletionCheckKind::ApprovedEvidenceUnchanged
                    && check.evidence_kind == Some(PlanEvidenceKind::DirectoryTree)
            }) {
                return Err(
                    "new approved_evidence_unchanged checks support only file or directory evidence"
                        .to_string(),
                );
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_artifact(
    artifact: &PlanArtifact,
    state: PlanSubmissionState,
) -> Result<(), String> {
    if artifact.schema_version != plan_artifact_schema_version() {
        return Err(format!(
            "unsupported plan artifact schema version {}",
            artifact.schema_version
        ));
    }
    validate_text("title", &artifact.title, 1, 160)?;
    validate_text("goal", &artifact.goal, 1, 2_000)?;
    validate_text("summary", &artifact.summary, 0, 4_000)?;
    if artifact.steps.len() > MAX_PLAN_STEPS {
        return Err(format!("steps must contain at most {MAX_PLAN_STEPS} items"));
    }
    if matches!(state, PlanSubmissionState::Ready) && artifact.steps.is_empty() {
        return Err("a ready plan must contain at least one step".to_string());
    }
    let mut step_ids = HashSet::new();
    for step in &artifact.steps {
        validate_identifier("step id", &step.id)?;
        if !step_ids.insert(step.id.as_str()) {
            return Err(format!("duplicate step id '{}'", step.id));
        }
        validate_text("step title", &step.title, 1, 240)?;
        validate_text("step description", &step.description, 0, 4_000)?;
        validate_string_list("affected_areas", &step.affected_areas, 12, 512)?;
    }
    validate_string_list("assumptions", &artifact.assumptions, 12, 1_000)?;
    validate_string_list("risks", &artifact.risks, 12, 1_000)?;
    validate_string_list("verification", &artifact.verification, 12, 1_000)?;
    validate_string_list(
        "acceptance_criteria",
        &artifact.acceptance_criteria,
        12,
        1_000,
    )?;
    validate_completion_checks(artifact, !artifact.completion_checks.is_empty())?;
    if artifact.questions.len() > MAX_PLAN_QUESTIONS {
        return Err(format!(
            "questions must contain at most {MAX_PLAN_QUESTIONS} items"
        ));
    }
    match state {
        PlanSubmissionState::NeedsInput if artifact.questions.is_empty() => {
            return Err("needs_input requires at least one blocking question".to_string());
        }
        PlanSubmissionState::Ready if !artifact.questions.is_empty() => {
            return Err("a ready plan cannot contain blocking questions".to_string());
        }
        _ => {}
    }
    let mut question_ids = HashSet::new();
    for question in &artifact.questions {
        validate_identifier("question id", &question.id)?;
        if !question_ids.insert(question.id.as_str()) {
            return Err(format!("duplicate question id '{}'", question.id));
        }
        validate_text("question prompt", &question.prompt, 1, 1_000)?;
        if !question.options.is_empty() && !(2..=4).contains(&question.options.len()) {
            return Err("question options must contain 2 to 4 items when present".to_string());
        }
        let mut option_ids = HashSet::new();
        for option in &question.options {
            validate_identifier("option id", &option.id)?;
            if !option_ids.insert(option.id.as_str()) {
                return Err(format!("duplicate option id '{}'", option.id));
            }
            validate_text("option label", &option.label, 1, 240)?;
            validate_text("option description", &option.description, 0, 1_000)?;
        }
    }
    let encoded = serde_json::to_vec(artifact).map_err(|error| error.to_string())?;
    if encoded.len() > MAX_PLAN_BYTES {
        return Err(format!("plan exceeds the {MAX_PLAN_BYTES}-byte limit"));
    }
    Ok(())
}

/// Validate a plan reconstructed from persistent storage without silently
/// normalizing malformed structured data. Initial placeholders and legacy
/// Markdown revisions are the only shapes that intentionally differ from a
/// submitted structured artifact.
pub(crate) fn validate_persisted_plan(plan: &PendingPlan) -> Result<(), String> {
    validate_identifier("plan id", &plan.id)?;
    if plan.revision == 0 {
        return Err("plan revision must be positive".to_string());
    }

    if plan.initial_submission_pending {
        if !matches!(
            plan.status,
            PlanStatus::Planning | PlanStatus::Failed | PlanStatus::Stopped | PlanStatus::Discarded
        ) {
            return Err(
                "initial plan submission marker is invalid for the persisted status".to_string(),
            );
        }
        validate_initial_placeholder(&plan.artifact)?;
        if !plan.progress.is_empty() {
            return Err("initial plan placeholder cannot contain progress steps".to_string());
        }
    } else {
        let artifact_state = if plan.artifact.legacy_markdown.is_some() {
            validate_persisted_legacy_artifact(&plan.artifact)?;
            PlanSubmissionState::Ready
        } else if plan.artifact.questions.is_empty() {
            validate_artifact(&plan.artifact, PlanSubmissionState::Ready)?;
            PlanSubmissionState::Ready
        } else {
            validate_artifact(&plan.artifact, PlanSubmissionState::NeedsInput)?;
            PlanSubmissionState::NeedsInput
        };
        match plan.status {
            PlanStatus::NeedsInput
                if !matches!(artifact_state, PlanSubmissionState::NeedsInput) =>
            {
                return Err("needs_input plan must contain blocking questions".to_string());
            }
            PlanStatus::Ready | PlanStatus::Executing | PlanStatus::Completed
                if !matches!(artifact_state, PlanSubmissionState::Ready) =>
            {
                return Err(format!(
                    "{} plan cannot contain blocking questions",
                    plan.status.label()
                ));
            }
            _ => {}
        }
        validate_persisted_progress(plan)?;
    }

    if plan.evidence.len() > MAX_PLAN_EVIDENCE {
        return Err("plan evidence exceeds its limit".to_string());
    }
    for evidence in &plan.evidence {
        validate_text("evidence path", &evidence.path, 1, 4_096)?;
        validate_text("evidence fingerprint", &evidence.fingerprint, 1, 256)?;
        match evidence.kind {
            PlanEvidenceKind::Git => {
                let selector = evidence
                    .selector
                    .as_deref()
                    .ok_or_else(|| "Git evidence requires a selector".to_string())?;
                validate_text("Git evidence selector", selector, 1, 16_384)?;
                serde_json::from_str::<Value>(selector)
                    .map_err(|error| format!("invalid Git evidence selector: {error}"))?;
            }
            _ if evidence.selector.is_some() => {
                return Err("non-Git evidence cannot contain a selector".to_string());
            }
            _ => {}
        }
    }
    validate_string_list(
        "stale override paths",
        &plan.stale_override_paths,
        MAX_PLAN_EVIDENCE,
        4_096,
    )?;
    if let Some(feedback) = plan.pending_feedback.as_deref() {
        validate_text("pending plan feedback", feedback, 1, MAX_PLAN_BYTES)?;
    }
    Ok(())
}

fn validate_initial_placeholder(artifact: &PlanArtifact) -> Result<(), String> {
    if artifact.schema_version != plan_artifact_schema_version() {
        return Err(format!(
            "unsupported plan artifact schema version {}",
            artifact.schema_version
        ));
    }
    validate_text("placeholder title", &artifact.title, 1, 160)?;
    validate_text("placeholder goal", &artifact.goal, 1, MAX_PLAN_BYTES)?;
    if !artifact.summary.is_empty()
        || !artifact.steps.is_empty()
        || !artifact.assumptions.is_empty()
        || !artifact.risks.is_empty()
        || !artifact.verification.is_empty()
        || !artifact.acceptance_criteria.is_empty()
        || !artifact.completion_checks.is_empty()
        || !artifact.questions.is_empty()
        || artifact.legacy_markdown.is_some()
    {
        return Err("initial plan placeholder contains submitted plan data".to_string());
    }
    let encoded = serde_json::to_vec(artifact).map_err(|error| error.to_string())?;
    if encoded.len() > MAX_PLAN_BYTES {
        return Err(format!(
            "initial plan placeholder exceeds the {MAX_PLAN_BYTES}-byte limit"
        ));
    }
    Ok(())
}

/// Build the transient artifact persisted before the planning run starts.
/// The full user message remains in Session history; this goal is only a
/// bounded preview used by the plan card and crash recovery state.
pub(crate) fn initial_placeholder_artifact(
    request_text: &str,
    has_images: bool,
) -> Result<PlanArtifact, String> {
    let mut goal = request_text.trim().to_string();
    if goal.is_empty() {
        if !has_images {
            return Err("a plan request must include text or an image".to_string());
        }
        goal = INITIAL_IMAGE_PLACEHOLDER_GOAL.to_string();
    }
    if goal.len() > MAX_INITIAL_PLACEHOLDER_GOAL_BYTES {
        let mut end = MAX_INITIAL_PLACEHOLDER_GOAL_BYTES;
        while end > 0 && !goal.is_char_boundary(end) {
            end -= 1;
        }
        goal.truncate(end);
    }

    let artifact = PlanArtifact {
        schema_version: plan_artifact_schema_version(),
        title: "Planning".to_string(),
        goal,
        ..PlanArtifact::default()
    };
    validate_initial_placeholder(&artifact)?;
    Ok(artifact)
}

fn validate_legacy_artifact_shape(artifact: &PlanArtifact) -> Result<(), String> {
    if artifact.schema_version != plan_artifact_schema_version() {
        return Err(format!(
            "unsupported plan artifact schema version {}",
            artifact.schema_version
        ));
    }
    let markdown = artifact
        .legacy_markdown
        .as_deref()
        .ok_or_else(|| "legacy plan is missing Markdown".to_string())?;
    validate_text("legacy title", &artifact.title, 1, 160)?;
    validate_text("legacy goal", &artifact.goal, 1, 2_000)?;
    if !artifact.summary.is_empty()
        || !artifact.assumptions.is_empty()
        || !artifact.risks.is_empty()
        || !artifact.verification.is_empty()
        || !artifact.acceptance_criteria.is_empty()
        || !artifact.completion_checks.is_empty()
        || !artifact.questions.is_empty()
        || artifact.steps.len() != 1
    {
        return Err("legacy plan artifact has an invalid shape".to_string());
    }
    let step = &artifact.steps[0];
    if step.id != "legacy-plan"
        || step.title.trim().is_empty()
        || !step.affected_areas.is_empty()
        || step.description != markdown
    {
        return Err("legacy plan step does not match its Markdown".to_string());
    }
    Ok(())
}

/// Validate a newly produced legacy fallback. The submission limit applies to
/// new plans, but not to v1 plans imported from the former unbounded format.
pub(crate) fn validate_legacy_artifact(artifact: &PlanArtifact) -> Result<(), String> {
    validate_legacy_artifact_shape(artifact)?;
    let encoded = serde_json::to_vec(artifact).map_err(|error| error.to_string())?;
    if encoded.len() > MAX_PLAN_BYTES {
        return Err(format!(
            "legacy plan exceeds the {MAX_PLAN_BYTES}-byte limit"
        ));
    }
    Ok(())
}

/// Validate a legacy artifact reconstructed from durable storage. Schema v1
/// did not impose the current submission-size limit, so applying it while
/// migrating or reloading would make an otherwise valid database unopenable.
pub(crate) fn validate_persisted_legacy_artifact(artifact: &PlanArtifact) -> Result<(), String> {
    validate_legacy_artifact_shape(artifact)
}

fn validate_persisted_progress(plan: &PendingPlan) -> Result<(), String> {
    if plan.progress.len() > MAX_PLAN_STEPS {
        return Err(format!(
            "plan progress contains more than {MAX_PLAN_STEPS} steps"
        ));
    }
    let artifact_steps = plan
        .artifact
        .steps
        .iter()
        .map(|step| (step.id.as_str(), step.title.as_str()))
        .collect::<BTreeMap<_, _>>();
    let mut progress_ids = HashSet::new();
    for step in &plan.progress {
        validate_identifier("progress step id", &step.id)?;
        if !progress_ids.insert(step.id.as_str()) {
            return Err(format!("duplicate progress step id '{}'", step.id));
        }
        validate_text("progress step title", &step.title, 1, 240)?;
        validate_text("progress step note", &step.note, 0, 2_000)?;
        if let Some(expected_title) = artifact_steps.get(step.id.as_str()) {
            if step.title != *expected_title {
                return Err(format!(
                    "progress step '{}' does not match the approved plan title",
                    step.id
                ));
            }
        } else {
            let reason = step.deviation_reason.as_deref().ok_or_else(|| {
                format!("adaptation step '{}' requires a deviation reason", step.id)
            })?;
            validate_text("adaptation deviation reason", reason, 1, 2_000)?;
        }
    }
    for step_id in artifact_steps.keys() {
        if !progress_ids.contains(step_id) {
            return Err(format!(
                "plan progress is missing approved step '{step_id}'"
            ));
        }
    }
    Ok(())
}

fn normalize_artifact(artifact: &mut PlanArtifact) {
    artifact.title = artifact.title.trim().to_string();
    artifact.goal = artifact.goal.trim().to_string();
    artifact.summary = artifact.summary.trim().to_string();
    for step in &mut artifact.steps {
        step.id = step.id.trim().to_string();
        step.title = step.title.trim().to_string();
        step.description = step.description.trim().to_string();
        normalize_strings(&mut step.affected_areas);
    }
    normalize_strings(&mut artifact.assumptions);
    normalize_strings(&mut artifact.risks);
    normalize_strings(&mut artifact.verification);
    normalize_strings(&mut artifact.acceptance_criteria);
    for check in &mut artifact.completion_checks {
        check.id = check.id.trim().to_string();
        check.step_id = check.step_id.trim().to_string();
        check.path = check.path.take().map(|path| path.trim().to_string());
        check.sha256 = check
            .sha256
            .take()
            .map(|hash| hash.trim().to_ascii_lowercase());
        check.tool_name = check
            .tool_name
            .take()
            .map(|tool_name| tool_name.trim().to_string());
        normalize_strings(&mut check.required_step_ids);
    }
    for question in &mut artifact.questions {
        question.id = question.id.trim().to_string();
        question.prompt = question.prompt.trim().to_string();
        for option in &mut question.options {
            option.id = option.id.trim().to_string();
            option.label = option.label.trim().to_string();
            option.description = option.description.trim().to_string();
        }
    }
}

fn normalize_strings(values: &mut Vec<String>) {
    for value in values.iter_mut() {
        *value = value.trim().to_string();
    }
    values.retain(|value| !value.is_empty());
}

fn validate_identifier(field: &str, value: &str) -> Result<(), String> {
    validate_text(field, value, 1, 80)?;
    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(format!(
            "{field} may contain only ASCII letters, digits, '-' and '_'"
        ));
    }
    Ok(())
}

fn validate_text(field: &str, value: &str, min: usize, max: usize) -> Result<(), String> {
    let length = value.chars().count();
    if length < min || length > max {
        return Err(format!("{field} must contain {min} to {max} characters"));
    }
    Ok(())
}

fn validate_string_list(
    field: &str,
    values: &[String],
    max_items: usize,
    max_chars: usize,
) -> Result<(), String> {
    if values.len() > max_items {
        return Err(format!("{field} must contain at most {max_items} items"));
    }
    for value in values {
        validate_text(field, value, 1, max_chars)?;
    }
    Ok(())
}

pub(crate) fn legacy_artifact(markdown: &str) -> PlanArtifact {
    let markdown = markdown.trim();
    PlanArtifact {
        schema_version: 1,
        title: "Proposed plan".to_string(),
        goal: "Execute the approved plan described below.".to_string(),
        summary: String::new(),
        steps: vec![PlanStep {
            id: "legacy-plan".to_string(),
            title: "Execute the approved plan".to_string(),
            description: markdown.to_string(),
            affected_areas: Vec::new(),
        }],
        assumptions: Vec::new(),
        risks: Vec::new(),
        verification: Vec::new(),
        acceptance_criteria: Vec::new(),
        completion_checks: Vec::new(),
        questions: Vec::new(),
        legacy_markdown: Some(markdown.to_string()),
    }
}

pub(crate) fn canonical_markdown(artifact: &PlanArtifact) -> String {
    if let Some(markdown) = artifact.legacy_markdown.as_deref() {
        return markdown.trim().to_string();
    }
    let mut output = format!("# {}\n\n{}", artifact.title, artifact.goal);
    if !artifact.summary.is_empty() {
        output.push_str("\n\n");
        output.push_str(&artifact.summary);
    }
    if !artifact.steps.is_empty() {
        output.push_str("\n\n## Steps");
        for (index, step) in artifact.steps.iter().enumerate() {
            output.push_str(&format!("\n\n{}. **{}**", index + 1, step.title));
            if !step.description.is_empty() {
                output.push_str(" — ");
                output.push_str(&step.description);
            }
            if !step.affected_areas.is_empty() {
                output.push_str("\n   - Areas: ");
                output.push_str(&step.affected_areas.join(", "));
            }
        }
    }
    append_markdown_list(&mut output, "Assumptions", &artifact.assumptions);
    append_markdown_list(&mut output, "Risks", &artifact.risks);
    append_markdown_list(&mut output, "Verification", &artifact.verification);
    append_markdown_list(
        &mut output,
        "Acceptance criteria",
        &artifact.acceptance_criteria,
    );
    if !artifact.completion_checks.is_empty() {
        output.push_str("\n\n## Server completion checks");
        for check in &artifact.completion_checks {
            output.push_str("\n\n- `");
            output.push_str(&check.id);
            output.push_str("` → `");
            output.push_str(&check.step_id);
            output.push_str("`: ");
            output.push_str(&completion_check_summary(check));
        }
    }
    if !artifact.questions.is_empty() {
        output.push_str("\n\n## Questions");
        for question in &artifact.questions {
            output.push_str("\n\n- ");
            output.push_str(&question.prompt);
            for option in &question.options {
                output.push_str("\n  - **");
                output.push_str(&option.label);
                output.push_str("**");
                if !option.description.is_empty() {
                    output.push_str(": ");
                    output.push_str(&option.description);
                }
            }
        }
    }
    output
}

fn append_markdown_list(output: &mut String, title: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    output.push_str("\n\n## ");
    output.push_str(title);
    for value in values {
        output.push_str("\n\n- ");
        output.push_str(value);
    }
}

fn completion_check_summary(check: &PlanCompletionCheck) -> String {
    match check.kind {
        PlanCompletionCheckKind::WorkspacePath => {
            let path = check.path.as_deref().unwrap_or("<missing path>");
            match check.expected_path_type {
                Some(PlanExpectedPathType::Absent) => format!("`{path}` must be absent"),
                Some(PlanExpectedPathType::Directory) => {
                    format!("`{path}` must be a directory")
                }
                Some(PlanExpectedPathType::File) => {
                    let mut constraints = vec![format!("`{path}` must be a file")];
                    if let Some(size) = check.size_bytes {
                        constraints.push(format!("size {size} bytes"));
                    }
                    if let Some(content) = check.exact_content.as_deref() {
                        constraints.push(format!(
                            "exact UTF-8 content `{}`",
                            content.replace('`', "\\`").replace('\n', "\\n")
                        ));
                    }
                    if let Some(hash) = check.sha256.as_deref() {
                        constraints.push(format!("SHA-256 `{hash}`"));
                    }
                    constraints.join(", ")
                }
                None => format!("`{path}` has an invalid path expectation"),
            }
        }
        PlanCompletionCheckKind::ApprovedEvidenceUnchanged => format!(
            "approved {:?} evidence for `{}` must remain unchanged",
            check.evidence_kind,
            check.path.as_deref().unwrap_or("<missing path>")
        ),
        PlanCompletionCheckKind::PlanProgress => format!(
            "approved steps must be completed or skipped: {}",
            check.required_step_ids.join(", ")
        ),
        PlanCompletionCheckKind::ToolCallSuccess => format!(
            "`{}` must succeed with the approved exact arguments",
            check.tool_name.as_deref().unwrap_or("<missing tool>")
        ),
    }
}

pub(crate) fn merge_evidence(
    existing: &mut Vec<PlanEvidence>,
    incoming: impl IntoIterator<Item = PlanEvidence>,
) -> bool {
    let mut by_identity = existing
        .drain(..)
        .map(|evidence| (evidence_identity(&evidence), evidence))
        .collect::<BTreeMap<_, _>>();
    let mut truncated = false;
    for evidence in incoming {
        let identity = evidence_identity(&evidence);
        if let Some(previous) = by_identity.get(&identity)
            && evidence_scope(previous.kind) > evidence_scope(evidence.kind)
        {
            continue;
        }
        if by_identity.contains_key(&identity) || by_identity.len() < MAX_PLAN_EVIDENCE {
            by_identity.insert(identity, evidence);
        } else {
            truncated = true;
        }
    }
    *existing = by_identity.into_values().collect();
    truncated
}

fn evidence_identity(evidence: &PlanEvidence) -> (String, String) {
    let selector = if evidence.kind == PlanEvidenceKind::Git {
        evidence.selector.clone().unwrap_or_default()
    } else {
        String::new()
    };
    (evidence.path.clone(), selector)
}

fn evidence_scope(kind: PlanEvidenceKind) -> u8 {
    match kind {
        PlanEvidenceKind::File | PlanEvidenceKind::Git => 0,
        PlanEvidenceKind::Directory => 1,
        PlanEvidenceKind::DirectoryTree => 2,
    }
}

pub(crate) fn supports_tool_evidence(tool_name: &str) -> bool {
    matches!(
        tool_name,
        crate::tools::TOOL_NAME_READ_FILE
            | crate::tools::TOOL_NAME_VIEW_IMAGE
            | crate::tools::TOOL_NAME_LIST_DIR
            | crate::tools::TOOL_NAME_SEARCH_FILES
            | crate::tools::TOOL_NAME_GIT_INSPECT
    )
}

/// Reconcile snapshots captured immediately before and after a read-only tool.
/// A changed snapshot is retained with an intentionally unverifiable
/// fingerprint so approval must refresh the plan instead of pairing the tool's
/// observation with a later resource state.
pub(crate) fn reconcile_tool_evidence(
    before: Vec<PlanEvidence>,
    after: Vec<PlanEvidence>,
) -> Vec<PlanEvidence> {
    let before = before
        .into_iter()
        .map(|evidence| (evidence_identity(&evidence), evidence))
        .collect::<BTreeMap<_, _>>();
    let after = after
        .into_iter()
        .map(|evidence| (evidence_identity(&evidence), evidence))
        .collect::<BTreeMap<_, _>>();
    if before == after {
        return after.into_values().collect();
    }

    let mut combined = before;
    combined.extend(after);
    combined
        .into_values()
        .map(|mut evidence| {
            evidence.fingerprint = "unstable".to_string();
            evidence
        })
        .collect()
}

#[cfg(test)]
pub(crate) fn try_capture_tool_evidence(
    tool_name: &str,
    args_json: &str,
    workspace: &Path,
) -> Result<CapturedPlanEvidence, String> {
    try_capture_tool_evidence_inner(tool_name, args_json, workspace, None, None)
}

pub(crate) fn try_capture_tool_evidence_with_timeout(
    tool_name: &str,
    args_json: &str,
    workspace: &Path,
    timeout: Duration,
    cancelled: &AtomicBool,
) -> Result<CapturedPlanEvidence, String> {
    try_capture_tool_evidence_inner(
        tool_name,
        args_json,
        workspace,
        Some(timeout),
        Some(cancelled),
    )
}

fn try_capture_tool_evidence_inner(
    tool_name: &str,
    args_json: &str,
    workspace: &Path,
    timeout: Option<Duration>,
    cancelled: Option<&AtomicBool>,
) -> Result<CapturedPlanEvidence, String> {
    if cancelled.is_some_and(|cancelled| cancelled.load(Ordering::Relaxed)) {
        return Err("plan evidence capture cancelled".to_string());
    }
    let args = serde_json::from_str::<Value>(args_json)
        .map_err(|error| format!("invalid evidence arguments: {error}"))?;
    let capture = match tool_name {
        crate::tools::TOOL_NAME_READ_FILE | crate::tools::TOOL_NAME_VIEW_IMAGE => {
            let path = args
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| "evidence path is missing".to_string())?;
            evidence_for_path(workspace, path, PlanEvidenceKind::File).map(
                |(evidence, truncated)| CapturedPlanEvidence {
                    evidence: vec![evidence],
                    truncated,
                },
            )
        }
        crate::tools::TOOL_NAME_LIST_DIR => {
            let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
            evidence_for_path(workspace, path, PlanEvidenceKind::Directory).map(
                |(evidence, truncated)| CapturedPlanEvidence {
                    evidence: vec![evidence],
                    truncated,
                },
            )
        }
        crate::tools::TOOL_NAME_SEARCH_FILES => {
            let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
            evidence_for_path(workspace, path, PlanEvidenceKind::DirectoryTree).map(
                |(evidence, truncated)| CapturedPlanEvidence {
                    evidence: vec![evidence],
                    truncated,
                },
            )
        }
        crate::tools::TOOL_NAME_GIT_INSPECT => {
            let selector = serde_json::to_string(&args).map_err(|error| error.to_string())?;
            let fingerprint = match (timeout, cancelled) {
                (Some(timeout), Some(cancelled)) => {
                    crate::tools::git::inspection_fingerprint_with_cancellation(
                        &args, workspace, timeout, cancelled,
                    )?
                }
                (Some(timeout), None) => crate::tools::git::inspection_fingerprint_with_timeout(
                    &args, workspace, timeout,
                )?,
                (None, _) => crate::tools::git::inspection_fingerprint(&args, workspace)?,
            };
            Ok(CapturedPlanEvidence {
                evidence: vec![PlanEvidence {
                    path: args
                        .get("path")
                        .and_then(Value::as_str)
                        .unwrap_or(".")
                        .to_string(),
                    kind: PlanEvidenceKind::Git,
                    fingerprint,
                    selector: Some(selector),
                }],
                truncated: false,
            })
        }
        _ => Ok(CapturedPlanEvidence::default()),
    }?;
    if cancelled.is_some_and(|cancelled| cancelled.load(Ordering::Relaxed)) {
        return Err("plan evidence capture cancelled".to_string());
    }
    Ok(capture)
}

#[cfg(test)]
pub(crate) fn capture_tool_evidence(
    tool_name: &str,
    args_json: &str,
    workspace: &Path,
) -> Vec<PlanEvidence> {
    try_capture_tool_evidence(tool_name, args_json, workspace)
        .expect("plan evidence fixture should be capturable")
        .evidence
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EvidenceVerificationError {
    Cancelled,
    TimedOut,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EvidenceVerificationSnapshot {
    pub(crate) stale_paths: Vec<String>,
    pub(crate) fingerprint: String,
}

fn hash_evidence_snapshot_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value);
}

pub(crate) fn verify_evidence_snapshot_until(
    workspace: &Path,
    evidence: &[PlanEvidence],
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<EvidenceVerificationSnapshot, EvidenceVerificationError> {
    let mut snapshot_digest = Sha256::new();
    snapshot_digest.update(b"lingclaw-plan-evidence-snapshot-v1");
    let mut stale = std::collections::BTreeSet::new();
    for expected in evidence {
        if cancelled.load(Ordering::Relaxed) {
            return Err(EvidenceVerificationError::Cancelled);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(EvidenceVerificationError::TimedOut);
        }

        let actual_fingerprint = if expected.kind == PlanEvidenceKind::Git {
            expected
                .selector
                .as_deref()
                .and_then(|selector| serde_json::from_str::<Value>(selector).ok())
                .and_then(|args| {
                    crate::tools::git::inspection_fingerprint_with_cancellation(
                        &args, workspace, remaining, cancelled,
                    )
                    .ok()
                })
        } else {
            evidence_for_path(workspace, &expected.path, expected.kind)
                .ok()
                .map(|(actual, _)| actual.fingerprint)
        };

        if cancelled.load(Ordering::Relaxed) {
            return Err(EvidenceVerificationError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(EvidenceVerificationError::TimedOut);
        }

        hash_evidence_snapshot_field(&mut snapshot_digest, expected.path.as_bytes());
        hash_evidence_snapshot_field(
            &mut snapshot_digest,
            match expected.kind {
                PlanEvidenceKind::File => b"file",
                PlanEvidenceKind::Directory => b"directory",
                PlanEvidenceKind::DirectoryTree => b"directory_tree",
                PlanEvidenceKind::Git => b"git",
            },
        );
        hash_evidence_snapshot_field(
            &mut snapshot_digest,
            expected.selector.as_deref().unwrap_or_default().as_bytes(),
        );
        match actual_fingerprint.as_deref() {
            Some(actual) => {
                snapshot_digest.update([1]);
                hash_evidence_snapshot_field(&mut snapshot_digest, actual.as_bytes());
            }
            None => snapshot_digest.update([0]),
        }

        if actual_fingerprint
            .as_ref()
            .is_none_or(|actual| actual != &expected.fingerprint)
        {
            stale.insert(expected.path.clone());
        }
    }
    Ok(EvidenceVerificationSnapshot {
        stale_paths: stale.into_iter().collect(),
        fingerprint: format!("{:x}", snapshot_digest.finalize()),
    })
}

#[cfg(test)]
pub(crate) fn verify_evidence_until(
    workspace: &Path,
    evidence: &[PlanEvidence],
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<Vec<String>, EvidenceVerificationError> {
    verify_evidence_snapshot_until(workspace, evidence, deadline, cancelled)
        .map(|snapshot| snapshot.stale_paths)
}

#[cfg(test)]
pub(crate) fn verify_evidence(workspace: &Path, evidence: &[PlanEvidence]) -> Vec<String> {
    let cancelled = AtomicBool::new(false);
    verify_evidence_until(
        workspace,
        evidence,
        Instant::now() + std::time::Duration::from_secs(60 * 60),
        &cancelled,
    )
    .unwrap_or_else(|_| evidence.iter().map(|item| item.path.clone()).collect())
}

fn evidence_for_path(
    workspace: &Path,
    path: &str,
    kind: PlanEvidenceKind,
) -> Result<(PlanEvidence, bool), String> {
    let resolved = crate::tools::safety::resolve_path_checked(path, workspace)?;
    let relative = if resolved.relative_path().as_os_str().is_empty() {
        ".".to_string()
    } else {
        resolved
            .relative_path()
            .to_string_lossy()
            .replace('\\', "/")
    };
    let (fingerprint, truncated) = match kind {
        PlanEvidenceKind::File => hash_file(&resolved)?,
        PlanEvidenceKind::Directory => (hash_directory(&resolved)?, false),
        PlanEvidenceKind::DirectoryTree => hash_directory_tree(&resolved)?,
        PlanEvidenceKind::Git => return Err("Git evidence requires an inspection selector".into()),
    };
    Ok((
        PlanEvidence {
            path: relative,
            kind,
            fingerprint,
            selector: None,
        },
        truncated,
    ))
}

fn hash_file(path: &crate::tools::safety::CheckedWorkspacePath) -> Result<(String, bool), String> {
    let (mut file, _) = path.open_file_for_read()?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("evidence path is not a file".to_string());
    }
    let mut digest = Sha256::new();
    hash_file_metadata(&metadata, &mut digest);
    let mut remaining = MAX_EVIDENCE_HASH_FILE_BYTES;
    let truncated = hash_file_content(&mut file, metadata.len(), &mut remaining, &mut digest)?;
    Ok((format!("{:x}", digest.finalize()), truncated))
}

fn hash_directory(path: &crate::tools::safety::CheckedWorkspacePath) -> Result<String, String> {
    let entries = path.read_directory()?;
    let mut digest = Sha256::new();
    for entry in entries {
        use crate::tools::safety::CheckedWorkspaceDirEntryKind as Kind;
        digest.update(entry.name.to_string_lossy().as_bytes());
        let kind = match entry.kind {
            Kind::Directory => b'd',
            Kind::File => b'f',
            Kind::LinkOrReparse => b's',
            Kind::Other | Kind::Missing => b'o',
        };
        digest.update([kind]);
        let Some(metadata) = entry.metadata else {
            digest.update(0_u64.to_le_bytes());
            continue;
        };
        digest.update(metadata.len().to_le_bytes());
        if let Ok(modified) = metadata.modified()
            && let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH)
        {
            digest.update(duration.as_nanos().to_le_bytes());
        }
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn hash_directory_tree(
    path: &crate::tools::safety::CheckedWorkspacePath,
) -> Result<(String, bool), String> {
    hash_checked_directory_tree_with_limits(
        path,
        5,
        10_000,
        MAX_EVIDENCE_HASH_FILE_BYTES,
        MAX_EVIDENCE_HASH_TOTAL_BYTES,
    )
}

#[cfg(test)]
fn hash_directory_tree_with_limits(
    path: &Path,
    max_depth: usize,
    max_files: usize,
    max_file_bytes: u64,
    max_total_bytes: u64,
) -> Result<(String, bool), String> {
    let checked = crate::tools::safety::resolve_path_checked(".", path)?;
    hash_checked_directory_tree_with_limits(
        &checked,
        max_depth,
        max_files,
        max_file_bytes,
        max_total_bytes,
    )
}

fn hash_checked_directory_tree_with_limits(
    path: &crate::tools::safety::CheckedWorkspacePath,
    max_depth: usize,
    max_files: usize,
    max_file_bytes: u64,
    max_total_bytes: u64,
) -> Result<(String, bool), String> {
    const SKIP_DIRS: &[&str] = &[
        "node_modules",
        "target",
        ".git",
        "__pycache__",
        "dist",
        "build",
        ".next",
        "vendor",
    ];

    struct DirectoryHashState<'a> {
        files_seen: usize,
        content_bytes_remaining: u64,
        max_depth: usize,
        max_files: usize,
        max_file_bytes: u64,
        digest: &'a mut Sha256,
    }

    fn visit(
        root: &crate::tools::safety::CheckedWorkspacePath,
        directory: &crate::tools::safety::CheckedWorkspacePath,
        depth: usize,
        state: &mut DirectoryHashState<'_>,
    ) -> Result<bool, String> {
        use crate::tools::safety::CheckedWorkspaceDirEntryKind as Kind;
        let entries = directory.read_directory()?;

        for entry in entries {
            if state.files_seen >= state.max_files {
                state.digest.update(b"file-limit-reached");
                return Ok(true);
            }
            let relative = entry
                .path
                .relative_path()
                .strip_prefix(root.relative_path())
                .unwrap_or_else(|_| entry.path.relative_path())
                .to_string_lossy();
            if entry.kind == Kind::LinkOrReparse {
                state.digest.update(relative.as_bytes());
                state.digest.update(b"s");
                continue;
            }
            if entry.kind == Kind::Directory {
                let name = &entry.name;
                let name = name.to_string_lossy();
                if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
                    continue;
                }
                state.digest.update(relative.as_bytes());
                state.digest.update(b"d");
                if depth < state.max_depth {
                    if visit(root, &entry.path, depth + 1, state)? {
                        return Ok(true);
                    }
                } else {
                    state.digest.update(b"depth-limit-reached");
                    return Ok(true);
                }
                continue;
            }
            if entry.kind != Kind::File {
                continue;
            }

            state.files_seen += 1;
            state.digest.update(relative.as_bytes());
            state.digest.update(b"f");
            let metadata = match entry.metadata {
                Some(metadata) => metadata,
                None => {
                    state.digest.update(b"metadata-error");
                    return Ok(true);
                }
            };
            hash_file_metadata(&metadata, state.digest);
            let Ok((mut file, _)) = entry.path.open_file_for_read() else {
                state.digest.update(b"read-error");
                return Ok(true);
            };
            match hash_file_content_with_limit(
                &mut file,
                metadata.len(),
                &mut state.content_bytes_remaining,
                state.max_file_bytes,
                state.digest,
            ) {
                Ok(true) => return Ok(true),
                Ok(false) => {}
                Err(error) => {
                    state.digest.update(b"read-error");
                    state.digest.update(error.as_bytes());
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    let mut digest = Sha256::new();
    let mut state = DirectoryHashState {
        files_seen: 0,
        content_bytes_remaining: max_total_bytes,
        max_depth,
        max_files,
        max_file_bytes,
        digest: &mut digest,
    };
    let truncated = visit(path, path, 0, &mut state)?;
    let files_seen = state.files_seen;
    let content_bytes_remaining = state.content_bytes_remaining;
    digest.update(files_seen.to_le_bytes());
    digest.update(content_bytes_remaining.to_le_bytes());
    Ok((format!("{:x}", digest.finalize()), truncated))
}

fn hash_file_metadata(metadata: &fs::Metadata, digest: &mut Sha256) {
    digest.update(metadata.len().to_le_bytes());
    if let Ok(modified) = metadata.modified()
        && let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH)
    {
        digest.update(duration.as_nanos().to_le_bytes());
    }
}

fn hash_file_content<R: Read + Seek>(
    file: &mut R,
    file_len: u64,
    total_bytes_remaining: &mut u64,
    digest: &mut Sha256,
) -> Result<bool, String> {
    hash_file_content_with_limit(
        file,
        file_len,
        total_bytes_remaining,
        MAX_EVIDENCE_HASH_FILE_BYTES,
        digest,
    )
}

fn hash_file_content_with_limit<R: Read + Seek>(
    file: &mut R,
    file_len: u64,
    total_bytes_remaining: &mut u64,
    max_file_bytes: u64,
    digest: &mut Sha256,
) -> Result<bool, String> {
    let budget = file_len.min(max_file_bytes).min(*total_bytes_remaining);
    if budget == 0 {
        digest.update(b"content-budget-exhausted");
        return Ok(file_len > 0);
    }

    if file_len <= budget {
        hash_reader_bytes(file, budget, digest)?;
    } else {
        let prefix_bytes = budget.div_ceil(2);
        let suffix_bytes = budget / 2;
        digest.update(b"sampled-prefix");
        hash_reader_bytes(file, prefix_bytes, digest)?;
        if suffix_bytes > 0 {
            let suffix_offset = i64::try_from(suffix_bytes)
                .map_err(|_| "evidence sample offset is too large".to_string())?;
            file.seek(SeekFrom::End(-suffix_offset))
                .map_err(|error| error.to_string())?;
            digest.update(b"sampled-suffix");
            hash_reader_bytes(file, suffix_bytes, digest)?;
        }
        digest.update(b"content-sampled");
    }
    *total_bytes_remaining = total_bytes_remaining.saturating_sub(budget);
    Ok(budget < file_len)
}

fn hash_reader_bytes(
    reader: &mut impl Read,
    mut remaining: u64,
    digest: &mut Sha256,
) -> Result<(), String> {
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        let requested = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| "evidence read size is too large".to_string())?;
        let count = reader
            .read(&mut buffer[..requested])
            .map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
        remaining = remaining.saturating_sub(count as u64);
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlanCompletionFailure {
    pub(crate) check_id: String,
    pub(crate) step_id: String,
    pub(crate) reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlanCompletionReport {
    pub(crate) plan_id: String,
    pub(crate) revision: u32,
    pub(crate) failures: Vec<PlanCompletionFailure>,
}

impl PlanCompletionReport {
    pub(crate) fn passed(&self) -> bool {
        self.failures.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlanCompletionVerificationError {
    Cancelled,
    TimedOut,
}

struct CompletionVerificationControl<'a> {
    deadline: Instant,
    cancelled: &'a AtomicBool,
}

impl CompletionVerificationControl<'_> {
    fn checkpoint(&self) -> Result<(), PlanCompletionVerificationError> {
        if self.cancelled.load(Ordering::Relaxed) {
            return Err(PlanCompletionVerificationError::Cancelled);
        }
        if Instant::now() >= self.deadline {
            return Err(PlanCompletionVerificationError::TimedOut);
        }
        Ok(())
    }
}

enum CompletionCheckError {
    Failed(String),
    Interrupted(PlanCompletionVerificationError),
}

impl From<String> for CompletionCheckError {
    fn from(value: String) -> Self {
        Self::Failed(value)
    }
}

impl From<PlanCompletionVerificationError> for CompletionCheckError {
    fn from(value: PlanCompletionVerificationError) -> Self {
        Self::Interrupted(value)
    }
}

type CompletionCheckResult = Result<(), CompletionCheckError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CompletionPathCheckStage {
    AfterResolve,
    AfterOpen,
}

pub(crate) fn matching_tool_completion_check_ids(
    plan: &PendingPlan,
    tool_name: &str,
    arguments: &Value,
) -> Vec<String> {
    plan.artifact
        .completion_checks
        .iter()
        .filter(|check| {
            check.kind == PlanCompletionCheckKind::ToolCallSuccess
                && check.tool_name.as_deref() == Some(tool_name)
                && check.arguments.as_ref() == Some(arguments)
        })
        .map(|check| check.id.clone())
        .collect()
}

fn completion_failure(
    check: &PlanCompletionCheck,
    reason: impl Into<String>,
) -> PlanCompletionFailure {
    PlanCompletionFailure {
        check_id: check.id.clone(),
        step_id: check.step_id.clone(),
        reason: reason.into(),
    }
}

fn metadata_modified(metadata: &fs::Metadata) -> Option<std::time::SystemTime> {
    metadata.modified().ok()
}

fn ensure_opened_metadata_stable(
    before: &fs::Metadata,
    after: &fs::Metadata,
) -> CompletionCheckResult {
    if before.len() != after.len()
        || before.is_file() != after.is_file()
        || before.is_dir() != after.is_dir()
        || metadata_modified(before) != metadata_modified(after)
    {
        return Err(CompletionCheckError::Failed(
            "the checked object changed while completion evidence was being read".to_string(),
        ));
    }
    Ok(())
}

fn evaluate_checked_completion_file(
    mut entry: crate::tools::safety::CheckedWorkspaceEntry,
    resolved: &crate::tools::safety::CheckedWorkspacePath,
    check: &PlanCompletionCheck,
    control: &CompletionVerificationControl<'_>,
) -> CompletionCheckResult {
    let initial_len = entry.metadata.len();
    if let Some(expected_size) = check.size_bytes
        && initial_len != expected_size
    {
        return Err(CompletionCheckError::Failed(format!(
            "file size is {initial_len} bytes; the approved contract requires {expected_size} bytes"
        )));
    }
    if let Some(expected_content) = check.exact_content.as_deref()
        && initial_len != expected_content.len() as u64
    {
        return Err(CompletionCheckError::Failed(format!(
            "file size is {initial_len} bytes; the approved exact content is {} bytes",
            expected_content.len()
        )));
    }
    if check.sha256.is_some() && initial_len > MAX_PLAN_COMPLETION_FILE_BYTES {
        return Err(CompletionCheckError::Failed(format!(
            "file exceeds the {MAX_PLAN_COMPLETION_FILE_BYTES}-byte completion-hash limit"
        )));
    }

    let must_read = check.exact_content.is_some() || check.sha256.is_some();
    let mut actual = check.exact_content.as_ref().map(|content| {
        Vec::with_capacity(content.len().min(MAX_PLAN_COMPLETION_EXACT_CONTENT_BYTES))
    });
    let mut digest = Sha256::new();
    let mut bytes_read = 0_u64;
    if must_read {
        let hard_limit = check
            .exact_content
            .as_ref()
            .map_or(MAX_PLAN_COMPLETION_FILE_BYTES, |content| {
                content.len() as u64
            });
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            control.checkpoint()?;
            let remaining_with_sentinel = hard_limit
                .saturating_sub(bytes_read)
                .saturating_add(1)
                .min(buffer.len() as u64);
            let requested = usize::try_from(remaining_with_sentinel).map_err(|_| {
                CompletionCheckError::Failed("completion read limit is invalid".into())
            })?;
            let count = entry.file.read(&mut buffer[..requested]).map_err(|error| {
                CompletionCheckError::Failed(format!(
                    "file content could not be read safely: {error}"
                ))
            })?;
            control.checkpoint()?;
            if count == 0 {
                break;
            }
            bytes_read = bytes_read.saturating_add(count as u64);
            if bytes_read > hard_limit {
                return Err(CompletionCheckError::Failed(format!(
                    "file exceeded the {hard_limit}-byte completion read limit while it was being read"
                )));
            }
            digest.update(&buffer[..count]);
            if let Some(actual) = actual.as_mut() {
                actual.extend_from_slice(&buffer[..count]);
            }
        }
    }

    control.checkpoint()?;
    let final_metadata = entry.file.metadata().map_err(|error| {
        CompletionCheckError::Failed(format!(
            "file metadata could not be rechecked safely: {error}"
        ))
    })?;
    ensure_opened_metadata_stable(&entry.metadata, &final_metadata)?;
    crate::tools::safety::reverify_checked_workspace_entry(&entry, resolved)
        .map_err(CompletionCheckError::Failed)?;
    if must_read && bytes_read != final_metadata.len() {
        return Err(CompletionCheckError::Failed(
            "file length changed or the checked handle ended before all bytes were read"
                .to_string(),
        ));
    }
    if let (Some(expected), Some(actual)) = (check.exact_content.as_deref(), actual.as_deref())
        && actual != expected.as_bytes()
    {
        return Err(CompletionCheckError::Failed(
            "file bytes differ from the approved exact content".to_string(),
        ));
    }
    if let Some(expected_hash) = check.sha256.as_deref() {
        let actual_hash = format!("{:x}", digest.finalize());
        if !actual_hash.eq_ignore_ascii_case(expected_hash) {
            return Err(CompletionCheckError::Failed(
                "file SHA-256 differs from the approved value".to_string(),
            ));
        }
    }
    control.checkpoint()?;
    Ok(())
}

fn evaluate_workspace_path_check_with_hook(
    workspace: &Path,
    check: &PlanCompletionCheck,
    control: &CompletionVerificationControl<'_>,
    hook: &mut dyn FnMut(CompletionPathCheckStage, &Path),
) -> CompletionCheckResult {
    control.checkpoint()?;
    let path = check
        .path
        .as_deref()
        .ok_or_else(|| CompletionCheckError::Failed("missing path".to_string()))?;
    let resolved = crate::tools::safety::resolve_path_checked(path, workspace)
        .map_err(CompletionCheckError::Failed)?;
    hook(
        CompletionPathCheckStage::AfterResolve,
        resolved.display_path(),
    );
    control.checkpoint()?;
    let expected_type = check
        .expected_path_type
        .ok_or_else(|| CompletionCheckError::Failed("missing expected path type".to_string()))?;
    let entry = match expected_type {
        PlanExpectedPathType::File => Some(
            resolved
                .open_file_entry_for_read()
                .map_err(CompletionCheckError::Failed)?,
        ),
        PlanExpectedPathType::Directory | PlanExpectedPathType::Absent => {
            crate::tools::safety::open_checked_workspace_entry(&resolved)
                .map_err(CompletionCheckError::Failed)?
        }
    };
    hook(CompletionPathCheckStage::AfterOpen, resolved.display_path());
    control.checkpoint()?;

    match expected_type {
        PlanExpectedPathType::Absent => {
            if entry.is_some() {
                return Err(CompletionCheckError::Failed(
                    "the path exists but the approved contract requires it to be absent".into(),
                ));
            }
            control.checkpoint()?;
            if crate::tools::safety::open_checked_workspace_entry(&resolved)
                .map_err(CompletionCheckError::Failed)?
                .is_some()
            {
                return Err(CompletionCheckError::Failed(
                    "the path appeared while its approved absence was being checked".into(),
                ));
            }
            Ok(())
        }
        PlanExpectedPathType::Directory => {
            let entry = entry
                .filter(|entry| entry.metadata.is_dir())
                .ok_or_else(|| {
                    CompletionCheckError::Failed(
                        "the path is missing or is not a checked directory".into(),
                    )
                })?;
            crate::tools::safety::reverify_checked_workspace_entry(&entry, &resolved)
                .map_err(CompletionCheckError::Failed)?;
            control.checkpoint()?;
            Ok(())
        }
        PlanExpectedPathType::File => {
            let entry = entry.ok_or_else(|| {
                CompletionCheckError::Failed(
                    "the path is missing or is not a checked regular file".into(),
                )
            })?;
            evaluate_checked_completion_file(entry, &resolved, check, control)
        }
    }
}

fn evaluate_workspace_path_check(
    workspace: &Path,
    check: &PlanCompletionCheck,
    control: &CompletionVerificationControl<'_>,
) -> CompletionCheckResult {
    evaluate_workspace_path_check_with_hook(workspace, check, control, &mut |_, _| {})
}

fn hash_checked_evidence_file(
    mut entry: crate::tools::safety::CheckedWorkspaceEntry,
    resolved: &crate::tools::safety::CheckedWorkspacePath,
    control: &CompletionVerificationControl<'_>,
) -> Result<String, CompletionCheckError> {
    let mut digest = Sha256::new();
    hash_file_metadata(&entry.metadata, &mut digest);
    let file_len = entry.metadata.len();
    let budget = file_len.min(MAX_EVIDENCE_HASH_FILE_BYTES);
    if budget == 0 {
        digest.update(b"content-budget-exhausted");
    } else if file_len <= budget {
        hash_reader_bytes_until(&mut entry.file, budget, &mut digest, control)?;
    } else {
        let prefix_bytes = budget.div_ceil(2);
        let suffix_bytes = budget / 2;
        digest.update(b"sampled-prefix");
        hash_reader_bytes_until(&mut entry.file, prefix_bytes, &mut digest, control)?;
        if suffix_bytes > 0 {
            let suffix_offset = i64::try_from(suffix_bytes).map_err(|_| {
                CompletionCheckError::Failed("evidence sample offset is too large".into())
            })?;
            control.checkpoint()?;
            entry
                .file
                .seek(SeekFrom::End(-suffix_offset))
                .map_err(|error| CompletionCheckError::Failed(error.to_string()))?;
            digest.update(b"sampled-suffix");
            hash_reader_bytes_until(&mut entry.file, suffix_bytes, &mut digest, control)?;
        }
        digest.update(b"content-sampled");
    }
    control.checkpoint()?;
    let final_metadata = entry.file.metadata().map_err(|error| {
        CompletionCheckError::Failed(format!("evidence metadata could not be rechecked: {error}"))
    })?;
    ensure_opened_metadata_stable(&entry.metadata, &final_metadata)?;
    crate::tools::safety::reverify_checked_workspace_entry(&entry, resolved)
        .map_err(CompletionCheckError::Failed)?;
    control.checkpoint()?;
    Ok(format!("{:x}", digest.finalize()))
}

fn hash_reader_bytes_until(
    reader: &mut impl Read,
    mut remaining: u64,
    digest: &mut Sha256,
    control: &CompletionVerificationControl<'_>,
) -> CompletionCheckResult {
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        control.checkpoint()?;
        let requested = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| CompletionCheckError::Failed("evidence read size is too large".into()))?;
        let count = reader.read(&mut buffer[..requested]).map_err(|error| {
            CompletionCheckError::Failed(format!("evidence bytes could not be read: {error}"))
        })?;
        control.checkpoint()?;
        if count == 0 {
            return Err(CompletionCheckError::Failed(
                "the checked file ended before its opened metadata length".into(),
            ));
        }
        digest.update(&buffer[..count]);
        remaining = remaining.saturating_sub(count as u64);
    }
    Ok(())
}

fn hash_checked_evidence_directory(
    entry: crate::tools::safety::CheckedWorkspaceEntry,
    resolved: &crate::tools::safety::CheckedWorkspacePath,
    control: &CompletionVerificationControl<'_>,
) -> Result<String, CompletionCheckError> {
    let mut entries = Vec::new();
    control.checkpoint()?;
    let directory = resolved
        .read_directory_from_entry(&entry)
        .map_err(CompletionCheckError::Failed)?;
    control.checkpoint()?;
    for item in directory {
        control.checkpoint()?;
        if entries.len() >= MAX_PLAN_COMPLETION_DIRECTORY_ENTRIES {
            return Err(CompletionCheckError::Failed(format!(
                "checked directory exceeds the {MAX_PLAN_COMPLETION_DIRECTORY_ENTRIES}-entry completion limit"
            )));
        }
        use crate::tools::safety::CheckedWorkspaceDirEntryKind as Kind;
        let kind = match item.kind {
            Kind::LinkOrReparse => b's',
            Kind::Directory => b'd',
            Kind::File => b'f',
            Kind::Other | Kind::Missing => b'o',
        };
        entries.push((item.name, kind, item.metadata));
    }
    control.checkpoint()?;
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut digest = Sha256::new();
    for (name, kind, metadata) in entries {
        control.checkpoint()?;
        digest.update(name.to_string_lossy().as_bytes());
        digest.update([kind]);
        let Some(metadata) = metadata else {
            digest.update(0_u64.to_le_bytes());
            continue;
        };
        digest.update(metadata.len().to_le_bytes());
        if let Ok(modified) = metadata.modified()
            && let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH)
        {
            digest.update(duration.as_nanos().to_le_bytes());
        }
    }
    crate::tools::safety::reverify_checked_workspace_entry(&entry, resolved)
        .map_err(CompletionCheckError::Failed)?;
    let final_metadata = entry.file.metadata().map_err(|error| {
        CompletionCheckError::Failed(format!(
            "directory metadata could not be rechecked: {error}"
        ))
    })?;
    ensure_opened_metadata_stable(&entry.metadata, &final_metadata)?;
    control.checkpoint()?;
    Ok(format!("{:x}", digest.finalize()))
}

fn evaluate_approved_evidence_check(
    plan: &PendingPlan,
    workspace: &Path,
    check: &PlanCompletionCheck,
    control: &CompletionVerificationControl<'_>,
) -> CompletionCheckResult {
    control.checkpoint()?;
    let path = check
        .path
        .as_deref()
        .ok_or_else(|| CompletionCheckError::Failed("missing evidence path".to_string()))?;
    let kind = check
        .evidence_kind
        .ok_or_else(|| CompletionCheckError::Failed("missing evidence kind".to_string()))?;
    let expected = plan
        .evidence
        .iter()
        .find(|item| {
            normalize_contract_path(&item.path) == normalize_contract_path(path)
                && item.kind == kind
        })
        .ok_or_else(|| {
            CompletionCheckError::Failed(
                "the approved revision did not capture the required evidence".to_string(),
            )
        })?;
    let resolved = crate::tools::safety::resolve_path_checked(path, workspace)
        .map_err(CompletionCheckError::Failed)?;
    let actual_fingerprint = match kind {
        PlanEvidenceKind::File => {
            let entry = resolved
                .open_file_entry_for_read()
                .map_err(CompletionCheckError::Failed)?;
            hash_checked_evidence_file(entry, &resolved, control)?
        }
        PlanEvidenceKind::Directory => {
            let entry = crate::tools::safety::open_checked_workspace_entry(&resolved)
                .map_err(CompletionCheckError::Failed)?
                .filter(|entry| entry.metadata.is_dir())
                .ok_or_else(|| {
                    CompletionCheckError::Failed(
                        "approved directory evidence is no longer a checked directory".into(),
                    )
                })?;
            hash_checked_evidence_directory(entry, &resolved, control)?
        }
        PlanEvidenceKind::DirectoryTree | PlanEvidenceKind::Git => {
            return Err(CompletionCheckError::Failed(
                "this evidence kind is not supported by completion verification".into(),
            ));
        }
    };
    if actual_fingerprint != expected.fingerprint {
        return Err(CompletionCheckError::Failed(
            "workspace evidence changed after the approved revision was captured".into(),
        ));
    }
    Ok(())
}

fn evaluate_progress_check(
    plan: &PendingPlan,
    check: &PlanCompletionCheck,
) -> CompletionCheckResult {
    for step_id in &check.required_step_ids {
        let Some(step) = plan.progress.iter().find(|step| &step.id == step_id) else {
            return Err(CompletionCheckError::Failed(format!(
                "approved step '{step_id}' has no progress record"
            )));
        };
        if !matches!(
            step.status,
            PlanStepStatus::Completed | PlanStepStatus::Skipped
        ) {
            return Err(CompletionCheckError::Failed(format!(
                "approved step '{step_id}' is {}",
                step.status.label()
            )));
        }
    }
    Ok(())
}

pub(crate) fn evaluate_completion_contract_until(
    plan: &PendingPlan,
    workspace: &Path,
    tool_evidence_epochs: &BTreeMap<String, u64>,
    current_mutation_epoch: u64,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<PlanCompletionReport, PlanCompletionVerificationError> {
    let control = CompletionVerificationControl {
        deadline,
        cancelled,
    };
    control.checkpoint()?;
    let mut report = PlanCompletionReport {
        plan_id: plan.id.clone(),
        revision: plan.revision,
        failures: Vec::new(),
    };

    if plan.artifact.completion_checks.is_empty() {
        if !plan.artifact.verification.is_empty() || !plan.artifact.acceptance_criteria.is_empty() {
            report.failures.push(PlanCompletionFailure {
                check_id: "completion-contract-missing".to_string(),
                step_id: plan
                    .artifact
                    .steps
                    .first()
                    .map(|step| step.id.clone())
                    .unwrap_or_else(|| "approved-plan".to_string()),
                reason: "the approved revision has verification or acceptance criteria but no server-verifiable completion checks".to_string(),
            });
        }
        return Ok(report);
    }

    if let Err(error) = validate_completion_checks(&plan.artifact, true) {
        report.failures.push(PlanCompletionFailure {
            check_id: "completion-contract-invalid".to_string(),
            step_id: plan
                .artifact
                .steps
                .first()
                .map(|step| step.id.clone())
                .unwrap_or_else(|| "approved-plan".to_string()),
            reason: error,
        });
        return Ok(report);
    }

    for check in &plan.artifact.completion_checks {
        control.checkpoint()?;
        let result = match check.kind {
            PlanCompletionCheckKind::WorkspacePath => {
                evaluate_workspace_path_check(workspace, check, &control)
            }
            PlanCompletionCheckKind::ApprovedEvidenceUnchanged => {
                evaluate_approved_evidence_check(plan, workspace, check, &control)
            }
            PlanCompletionCheckKind::PlanProgress => evaluate_progress_check(plan, check),
            PlanCompletionCheckKind::ToolCallSuccess => {
                if tool_evidence_epochs.get(&check.id) == Some(&current_mutation_epoch) {
                    Ok(())
                } else {
                    Err(CompletionCheckError::Failed("the approved exact tool call did not succeed after the final unverified mutation".to_string()))
                }
            }
        };
        match result {
            Ok(()) => {}
            Err(CompletionCheckError::Failed(reason)) => {
                report.failures.push(completion_failure(check, reason));
            }
            Err(CompletionCheckError::Interrupted(error)) => return Err(error),
        }
    }
    control.checkpoint()?;
    Ok(report)
}

#[cfg(test)]
pub(crate) fn evaluate_completion_contract(
    plan: &PendingPlan,
    workspace: &Path,
    tool_evidence_epochs: &BTreeMap<String, u64>,
    current_mutation_epoch: u64,
) -> PlanCompletionReport {
    let cancelled = AtomicBool::new(false);
    evaluate_completion_contract_until(
        plan,
        workspace,
        tool_evidence_epochs,
        current_mutation_epoch,
        Instant::now() + Duration::from_secs(60 * 60),
        &cancelled,
    )
    .expect("completion verification fixture should not time out")
}

pub(crate) fn apply_completion_failures(
    plan: &mut PendingPlan,
    report: &PlanCompletionReport,
) -> Result<(), String> {
    if plan.id != report.plan_id || plan.revision != report.revision {
        return Err("completion report does not match the active plan revision".to_string());
    }
    for failure in &report.failures {
        let Some(step) = plan
            .progress
            .iter_mut()
            .find(|step| step.id == failure.step_id)
        else {
            return Err(format!(
                "completion failure '{}' is not bound to an active approved step",
                failure.check_id
            ));
        };
        step.status = PlanStepStatus::Blocked;
        let server_note = format!(
            "Server completion check '{}' failed for revision {}: {}",
            failure.check_id, report.revision, failure.reason
        );
        if step.note.is_empty() {
            step.note = server_note;
        } else if !step.note.contains(&server_note) {
            step.note.push_str(" | ");
            step.note.push_str(&server_note);
        }
        if step.note.chars().count() > 2_000 {
            step.note = step.note.chars().take(2_000).collect();
        }
    }
    Ok(())
}

pub(crate) fn submit_plan_tool_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "state": { "type": "string", "enum": ["needs_input", "ready"] },
            "title": { "type": "string", "minLength": 1, "maxLength": 160 },
            "goal": { "type": "string", "minLength": 1, "maxLength": 2000 },
            "summary": { "type": "string", "maxLength": 4000 },
            "steps": {
                "type": "array", "maxItems": MAX_PLAN_STEPS,
                "items": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "minLength": 1, "maxLength": 80 },
                        "title": { "type": "string", "minLength": 1, "maxLength": 240 },
                        "description": { "type": "string", "maxLength": 4000 },
                        "affected_areas": { "type": "array", "maxItems": 12, "items": { "type": "string", "maxLength": 512 } }
                    },
                    "required": ["id", "title"],
                    "additionalProperties": false
                }
            },
            "assumptions": string_array_schema(12, 1000),
            "risks": string_array_schema(12, 1000),
            "verification": string_array_schema(12, 1000),
            "acceptance_criteria": string_array_schema(12, 1000),
            "completion_checks": {
                "type": "array", "maxItems": MAX_PLAN_COMPLETION_CHECKS,
                "description": "Server-verifiable checks for every zero-based verification and acceptance_criteria item. Checks are immutable with this revision and must bind to an original step. plan_progress may add a progress gate, but it does not provide contract-clause coverage; each covered clause also requires workspace_path, approved_evidence_unchanged, or tool_call_success.",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "minLength": 1, "maxLength": 80 },
                        "step_id": { "type": "string", "minLength": 1, "maxLength": 80 },
                        "covers": {
                            "type": "array", "minItems": 1, "maxItems": 24,
                            "items": {
                                "type": "object",
                                "properties": {
                                    "section": { "type": "string", "enum": ["verification", "acceptance_criteria"] },
                                    "index": { "type": "integer", "minimum": 0, "maximum": 11 }
                                },
                                "required": ["section", "index"],
                                "additionalProperties": false
                            }
                        },
                        "kind": { "type": "string", "enum": ["workspace_path", "approved_evidence_unchanged", "plan_progress", "tool_call_success"] },
                        "path": { "type": "string", "minLength": 1, "maxLength": 4096 },
                        "expected_path_type": { "type": "string", "enum": ["file", "directory", "absent"] },
                        "exact_content": { "type": "string", "maxLength": MAX_PLAN_COMPLETION_EXACT_CONTENT_BYTES },
                        "size_bytes": { "type": "integer", "minimum": 0 },
                        "sha256": { "type": "string", "minLength": 64, "maxLength": 64 },
                        "evidence_kind": { "type": "string", "enum": ["file", "directory"] },
                        "required_step_ids": { "type": "array", "maxItems": MAX_PLAN_STEPS, "items": { "type": "string", "minLength": 1, "maxLength": 80 } },
                        "tool_name": { "type": "string", "minLength": 1, "maxLength": 256 },
                        "arguments": { "type": "object" }
                    },
                    "required": ["id", "step_id", "covers", "kind"],
                    "additionalProperties": false
                }
            },
            "questions": {
                "type": "array", "maxItems": MAX_PLAN_QUESTIONS,
                "items": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "minLength": 1, "maxLength": 80 },
                        "prompt": { "type": "string", "minLength": 1, "maxLength": 1000 },
                        "options": {
                            "type": "array", "minItems": 2, "maxItems": 4,
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": { "type": "string", "minLength": 1, "maxLength": 80 },
                                    "label": { "type": "string", "minLength": 1, "maxLength": 240 },
                                    "description": { "type": "string", "maxLength": 1000 }
                                },
                                "required": ["id", "label"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["id", "prompt"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["state", "title", "goal", "steps"],
        "additionalProperties": false
    })
}

pub(crate) fn update_plan_tool_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "base_revision": { "type": "integer", "minimum": 1 },
            "updates": {
                "type": "array", "maxItems": MAX_PLAN_STEPS,
                "items": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "minLength": 1, "maxLength": 80 },
                        "status": { "type": "string", "enum": ["pending", "in_progress", "completed", "blocked", "skipped"] },
                        "note": { "type": "string", "maxLength": 2000 }
                    },
                    "required": ["id", "status"],
                    "additionalProperties": false
                }
            },
            "append_steps": {
                "type": "array", "maxItems": MAX_PLAN_STEPS,
                "items": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "minLength": 1, "maxLength": 80 },
                        "title": { "type": "string", "minLength": 1, "maxLength": 240 },
                        "note": { "type": "string", "maxLength": 2000 },
                        "deviation_reason": { "type": "string", "minLength": 1, "maxLength": 2000 }
                    },
                    "required": ["id", "title", "deviation_reason"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["base_revision"],
        "additionalProperties": false
    })
}

pub(crate) fn tool_definition(
    provider: crate::Provider,
    name: &str,
    description: &str,
    parameters: Value,
) -> Value {
    match provider {
        crate::Provider::Anthropic => json!({
            "name": name,
            "description": description,
            "input_schema": parameters,
        }),
        crate::Provider::Gemini => json!({
            "name": name,
            "description": description,
            "parameters": crate::tools::gemini_tool_parameters(parameters),
        }),
        crate::Provider::OpenAI | crate::Provider::OpenAIResponses | crate::Provider::Ollama => {
            json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": description,
                    "parameters": parameters,
                }
            })
        }
    }
}

fn string_array_schema(max_items: usize, max_length: usize) -> Value {
    json!({
        "type": "array",
        "maxItems": max_items,
        "items": { "type": "string", "minLength": 1, "maxLength": max_length }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CompletionFilesystemGuard {
        root: std::path::PathBuf,
        links: Vec<(std::path::PathBuf, bool)>,
        #[cfg(target_os = "linux")]
        mounts: Vec<std::path::PathBuf>,
    }

    impl CompletionFilesystemGuard {
        fn new(label: &str) -> Self {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            Self {
                root: std::env::temp_dir().join(format!("lingclaw-completion-{label}-{unique}")),
                links: Vec::new(),
                #[cfg(target_os = "linux")]
                mounts: Vec::new(),
            }
        }

        fn track_link(&mut self, path: std::path::PathBuf, directory: bool) {
            self.links.push((path, directory));
        }

        #[cfg(target_os = "linux")]
        fn track_mount(&mut self, path: std::path::PathBuf) {
            self.mounts.push(path);
        }
    }

    impl Drop for CompletionFilesystemGuard {
        fn drop(&mut self) {
            #[cfg(target_os = "linux")]
            for path in self.mounts.iter().rev() {
                use std::os::unix::ffi::OsStrExt as _;
                if let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) {
                    // SAFETY: the path is a NUL-terminated test-owned mountpoint.
                    let _ = unsafe { libc::umount2(path.as_ptr(), libc::MNT_DETACH) };
                }
            }
            for (path, directory) in self.links.iter().rev() {
                if *directory {
                    let _ = std::fs::remove_dir(path);
                } else {
                    let _ = std::fs::remove_file(path);
                }
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn completion_workspace_check(
        path: &str,
        expected_path_type: PlanExpectedPathType,
    ) -> PlanCompletionCheck {
        PlanCompletionCheck {
            id: "secure-path".into(),
            step_id: "verify".into(),
            covers: vec![PlanContractClauseRef {
                section: PlanContractSection::AcceptanceCriteria,
                index: 0,
            }],
            kind: PlanCompletionCheckKind::WorkspacePath,
            path: Some(path.into()),
            expected_path_type: Some(expected_path_type),
            exact_content: None,
            size_bytes: None,
            sha256: None,
            evidence_kind: None,
            required_step_ids: Vec::new(),
            tool_name: None,
            arguments: None,
        }
    }

    fn completion_check_failure(result: CompletionCheckResult) -> String {
        match result {
            Err(CompletionCheckError::Failed(reason)) => reason,
            Err(CompletionCheckError::Interrupted(error)) => {
                panic!("completion check was unexpectedly interrupted: {error:?}")
            }
            Ok(()) => panic!("completion check unexpectedly passed"),
        }
    }

    #[test]
    fn gemini_plan_tool_schema_uses_compatible_keywords() {
        let definition = tool_definition(
            crate::Provider::Gemini,
            TOOL_NAME_SUBMIT_PLAN,
            "Submit a plan",
            submit_plan_tool_parameters(),
        );
        let parameters = &definition["parameters"];

        assert!(parameters.get("additionalProperties").is_none());
        assert!(
            parameters["properties"]["steps"]["items"]
                .get("additionalProperties")
                .is_none()
        );
        assert!(
            parameters["properties"]["questions"]["items"]
                .get("additionalProperties")
                .is_none()
        );
    }

    #[test]
    fn submission_json_accepts_escaped_nested_strings_and_rejects_raw_escapes() {
        let encoded = serde_json::to_string(&json!({
            "state": "ready",
            "title": "Write the result",
            "goal": "Create the requested artifact",
            "summary": "Keep nested string data intact",
            "steps": [{
                "id": "write-result",
                "title": "Write result.txt",
                "description": "Read C:\\workspace\\spec.txt\nThen write result.txt",
                "affected_areas": ["nested\\folder", "result.txt"]
            }],
            "verification": ["Line one\nLine two"],
            "acceptance_criteria": ["result.txt exists."],
            "completion_checks": [
                {
                    "id": "write-progress",
                    "step_id": "write-result",
                    "covers": [
                        {"section": "verification", "index": 0},
                        {"section": "acceptance_criteria", "index": 0}
                    ],
                    "kind": "plan_progress",
                    "required_step_ids": ["write-result"]
                },
                {
                    "id": "result-file",
                    "step_id": "write-result",
                    "covers": [
                        {"section": "verification", "index": 0},
                        {"section": "acceptance_criteria", "index": 0}
                    ],
                    "kind": "workspace_path",
                    "path": "result.txt",
                    "expected_path_type": "file"
                }
            ]
        }))
        .expect("valid plan arguments should encode");

        let submission = validate_submission_json(&encoded)
            .expect("JSON-escaped backslashes and newlines should validate");
        assert_eq!(
            submission.artifact.steps[0].description,
            "Read C:\\workspace\\spec.txt\nThen write result.txt"
        );
        assert_eq!(
            submission.artifact.steps[0].affected_areas[0],
            "nested\\folder"
        );

        let raw_windows_escape = r#"{"state":"ready","title":"Write","goal":"Write","steps":[{"id":"write","title":"Write","description":"Use \\?\E:\work\spec.txt"}]}"#;
        assert!(
            validate_submission_json(raw_windows_escape)
                .expect_err("an unescaped Windows path must remain invalid")
                .contains("invalid escape")
        );

        let raw_newline = "{\"state\":\"ready\",\"title\":\"Write\",\"goal\":\"Write\",\"steps\":[{\"id\":\"write\",\"title\":\"Write\",\"description\":\"line one\nline two\"}]}";
        assert!(
            validate_submission_json(raw_newline)
                .expect_err("a raw newline inside a JSON string must remain invalid")
                .contains("control character")
        );
    }

    fn ready_plan_json(extra: &str) -> String {
        format!(
            r#"{{
                "state":"ready",
                "title":"Implement the change",
                "goal":"Ship a verified implementation",
                "steps":[{{"id":"inspect","title":"Inspect the code"}}],
                "acceptance_criteria":["The workspace remains available for the approved inspection."],
                "completion_checks":[{{
                    "id":"inspect-workspace",
                    "step_id":"inspect",
                    "covers":[{{"section":"acceptance_criteria","index":0}}],
                    "kind":"workspace_path",
                    "path":".",
                    "expected_path_type":"directory"
                }}]
                {extra}
            }}"#
        )
    }

    #[test]
    fn submission_validation_enforces_state_shape_and_unknown_fields() {
        let ready = validate_submission_json(&ready_plan_json(""))
            .expect("a minimal ready plan should validate");
        assert!(matches!(ready.state, PlanSubmissionState::Ready));
        assert_eq!(ready.artifact.steps[0].id, "inspect");

        let missing_questions = ready_plan_json("").replace("\"ready\"", "\"needs_input\"");
        assert!(
            validate_submission_json(&missing_questions)
                .expect_err("needs_input without questions must fail")
                .contains("blocking question")
        );
        assert!(
            validate_submission_json(&ready_plan_json(",\"unexpected\":true"))
                .expect_err("unknown fields must fail closed")
                .contains("unknown field")
        );
        assert!(
            validate_submission_json(&ready_plan_json(",\"schema_version\":999"))
                .expect_err("explicit unsupported schema versions must fail closed")
                .contains("unsupported plan artifact schema version 999")
        );
    }

    #[test]
    fn completion_contract_rejects_missing_and_contradictory_acceptance_evidence() {
        let missing = serde_json::to_string(&json!({
            "state": "ready",
            "title": "Verify both outcomes",
            "goal": "Produce a verified artifact",
            "steps": [{"id": "write", "title": "Write the artifact"}],
            "acceptance_criteria": ["result.txt exists", "result.txt has the approved bytes"],
            "completion_checks": [{
                "id": "result-exists",
                "step_id": "write",
                "covers": [{"section": "acceptance_criteria", "index": 0}],
                "kind": "workspace_path",
                "path": "result.txt",
                "expected_path_type": "file"
            }]
        }))
        .expect("test contract should encode");
        assert!(
            validate_submission_json(&missing)
                .expect_err("every acceptance item requires executable evidence")
                .contains("acceptance_criteria item 1")
        );

        let contradictory = serde_json::to_string(&json!({
            "state": "ready",
            "title": "Reject contradictory output",
            "goal": "Produce one exact artifact",
            "steps": [{"id": "write", "title": "Write the artifact"}],
            "acceptance_criteria": ["result.txt equals A", "result.txt has two bytes"],
            "completion_checks": [
                {
                    "id": "result-a",
                    "step_id": "write",
                    "covers": [{"section": "acceptance_criteria", "index": 0}],
                    "kind": "workspace_path",
                    "path": "result.txt",
                    "expected_path_type": "file",
                    "exact_content": "A"
                },
                {
                    "id": "result-b",
                    "step_id": "write",
                    "covers": [{"section": "acceptance_criteria", "index": 1}],
                    "kind": "workspace_path",
                    "path": "./result.txt",
                    "expected_path_type": "file",
                    "size_bytes": 2
                }
            ]
        }))
        .expect("test contract should encode");
        assert!(
            validate_submission_json(&contradictory)
                .expect_err("one revision cannot approve conflicting final states")
                .contains("contradictory expectations")
        );

        let recursive_evidence = serde_json::to_string(&json!({
            "state": "ready",
            "title": "Reject unsafe recursive evidence",
            "goal": "Keep the inspected tree unchanged",
            "steps": [{"id": "verify", "title": "Verify the tree"}],
            "acceptance_criteria": ["The inspected tree is unchanged"],
            "completion_checks": [{
                "id": "tree-unchanged",
                "step_id": "verify",
                "covers": [{"section": "acceptance_criteria", "index": 0}],
                "kind": "approved_evidence_unchanged",
                "path": ".",
                "evidence_kind": "directory_tree"
            }]
        }))
        .expect("test contract should encode");
        assert!(
            validate_submission_json(&recursive_evidence)
                .expect_err("new recursive completion evidence must fail closed")
                .contains("only file or directory evidence")
        );
    }

    #[test]
    fn plan_progress_is_only_a_supplemental_completion_gate() {
        let progress_only = serde_json::to_string(&json!({
            "state": "ready",
            "title": "Write exact approved bytes",
            "goal": "Create the immutable approved result",
            "steps": [{"id": "write", "title": "Write result.txt"}],
            "acceptance_criteria": ["result.txt contains exactly V2"],
            "completion_checks": [{
                "id": "write-progress",
                "step_id": "write",
                "covers": [{"section": "acceptance_criteria", "index": 0}],
                "kind": "plan_progress",
                "required_step_ids": ["write"]
            }]
        }))
        .expect("progress-only contract should encode");
        assert!(
            validate_submission_json(&progress_only)
                .expect_err("Agent-reported progress must not prove exact acceptance")
                .contains("acceptance_criteria item 0 is missing a server-verifiable")
        );

        let split_coverage = serde_json::to_string(&json!({
            "state": "ready",
            "title": "Verify and write",
            "goal": "Create the immutable approved result",
            "steps": [{"id": "write", "title": "Write result.txt"}],
            "verification": ["The write step is reported complete"],
            "acceptance_criteria": ["result.txt contains exactly V2"],
            "completion_checks": [
                {
                    "id": "write-progress",
                    "step_id": "write",
                    "covers": [{"section": "verification", "index": 0}],
                    "kind": "plan_progress",
                    "required_step_ids": ["write"]
                },
                {
                    "id": "result-bytes",
                    "step_id": "write",
                    "covers": [{"section": "acceptance_criteria", "index": 0}],
                    "kind": "workspace_path",
                    "path": "result.txt",
                    "expected_path_type": "file",
                    "exact_content": "V2",
                    "size_bytes": 2
                }
            ]
        }))
        .expect("split contract should encode");
        assert!(
            validate_submission_json(&split_coverage)
                .expect_err("each clause needs independent server-verifiable coverage")
                .contains("verification item 0 is missing a server-verifiable")
        );

        let jointly_bound = serde_json::to_string(&json!({
            "state": "ready",
            "title": "Write and report exact approved bytes",
            "goal": "Create the immutable approved result",
            "steps": [{"id": "write", "title": "Write result.txt"}],
            "acceptance_criteria": ["result.txt contains exactly V2"],
            "completion_checks": [
                {
                    "id": "write-progress",
                    "step_id": "write",
                    "covers": [{"section": "acceptance_criteria", "index": 0}],
                    "kind": "plan_progress",
                    "required_step_ids": ["write"]
                },
                {
                    "id": "result-bytes",
                    "step_id": "write",
                    "covers": [{"section": "acceptance_criteria", "index": 0}],
                    "kind": "workspace_path",
                    "path": "result.txt",
                    "expected_path_type": "file",
                    "exact_content": "V2",
                    "size_bytes": 2
                }
            ]
        }))
        .expect("joint contract should encode");
        let artifact = validate_submission_json(&jointly_bound)
            .expect("server evidence may share a clause with a supplemental progress gate")
            .artifact;
        let guard = CompletionFilesystemGuard::new("progress-supplement");
        std::fs::create_dir_all(&guard.root).expect("create completion workspace");
        std::fs::write(guard.root.join("result.txt"), b"V2").expect("seed exact result");
        let mut plan = PendingPlan::new(
            "plan-progress-supplement".into(),
            0,
            1,
            1,
            1,
            PlanStatus::Executing,
            artifact,
            Vec::new(),
            false,
        );

        let pending = evaluate_completion_contract(&plan, &guard.root, &BTreeMap::new(), 0);
        assert!(
            !pending.passed(),
            "the supplemental progress gate still applies"
        );
        assert_eq!(pending.failures[0].check_id, "write-progress");

        plan.progress[0].status = PlanStepStatus::Completed;
        let completed = evaluate_completion_contract(&plan, &guard.root, &BTreeMap::new(), 0);
        assert!(completed.passed(), "both progress and exact bytes now pass");
    }

    #[test]
    fn completion_contract_distinguishes_cancellation_from_deadline_expiry() {
        let artifact = validate_submission_json(&ready_plan_json(""))
            .expect("test plan should validate")
            .artifact;
        let mut plan = PendingPlan::new(
            "plan-bounded-verifier".into(),
            0,
            1,
            1,
            2,
            PlanStatus::Executing,
            artifact,
            Vec::new(),
            false,
        );
        plan.progress[0].status = PlanStepStatus::Completed;

        let cancelled = AtomicBool::new(true);
        assert_eq!(
            evaluate_completion_contract_until(
                &plan,
                Path::new("."),
                &BTreeMap::new(),
                0,
                Instant::now() + Duration::from_secs(1),
                &cancelled,
            ),
            Err(PlanCompletionVerificationError::Cancelled)
        );

        cancelled.store(false, Ordering::Relaxed);
        assert_eq!(
            evaluate_completion_contract_until(
                &plan,
                Path::new("."),
                &BTreeMap::new(),
                0,
                Instant::now(),
                &cancelled,
            ),
            Err(PlanCompletionVerificationError::TimedOut)
        );
    }

    #[test]
    fn initial_placeholder_is_valid_for_image_only_and_adversarial_text() {
        let image_only = initial_placeholder_artifact("", true)
            .expect("an image-only planning request needs a durable placeholder");
        assert!(!image_only.goal.is_empty());
        validate_initial_placeholder(&image_only).expect("image placeholder should validate");

        let mut pending = PendingPlan::new(
            "plan-image-only".into(),
            0,
            1,
            1,
            1,
            PlanStatus::Planning,
            image_only,
            Vec::new(),
            false,
        );
        pending.initial_submission_pending = true;
        let live = pending.to_live_value();
        assert_eq!(live["initial_submission_pending"], true);
        assert_eq!(live["initial_request_image_only"], true);

        let escaped = "\0".repeat(100_000);
        let bounded = initial_placeholder_artifact(&escaped, false)
            .expect("large request text should be reduced to a safe preview");
        validate_initial_placeholder(&bounded).expect("bounded placeholder should validate");
        assert!(serde_json::to_vec(&bounded).unwrap().len() <= MAX_PLAN_BYTES);

        let mut text_pending = PendingPlan::new(
            "plan-text".into(),
            0,
            1,
            1,
            1,
            PlanStatus::Planning,
            bounded,
            Vec::new(),
            false,
        );
        text_pending.initial_submission_pending = true;
        assert_eq!(
            text_pending.to_live_value()["initial_request_image_only"],
            false
        );

        assert!(initial_placeholder_artifact("   ", false).is_err());
    }

    #[test]
    fn legacy_plan_size_limit_covers_the_complete_serialized_artifact() {
        let artifact = legacy_artifact(&"x".repeat(MAX_PLAN_BYTES / 2));
        assert!(serde_json::to_vec(&artifact).unwrap().len() > MAX_PLAN_BYTES);
        assert!(
            validate_legacy_artifact(&artifact)
                .expect_err("duplicated legacy Markdown must count toward the artifact limit")
                .contains("exceeds")
        );

        validate_legacy_artifact(&legacy_artifact("Inspect, implement, and verify."))
            .expect("a small legacy plan should remain valid");
    }

    #[test]
    fn progress_updates_preserve_the_immutable_revision_artifact() {
        let artifact = validate_submission_json(&ready_plan_json(""))
            .expect("plan should validate")
            .artifact;
        let mut plan = PendingPlan::new(
            "plan-1".into(),
            0,
            1,
            1,
            3,
            PlanStatus::Executing,
            artifact,
            Vec::new(),
            false,
        );
        let original_artifact = plan.artifact.clone();
        let update = validate_progress_json(
            r#"{
                "base_revision":3,
                "updates":[{"id":"inspect","status":"completed","note":"Checked"}],
                "append_steps":[{
                    "id":"adapt",
                    "title":"Handle the discovered edge case",
                    "deviation_reason":"Inspection exposed a new compatibility requirement"
                }]
            }"#,
        )
        .expect("progress update should validate");

        apply_progress_update(&mut plan, update).expect("progress should apply");

        assert_eq!(plan.artifact, original_artifact);
        assert_eq!(plan.progress.len(), 2);
        assert_eq!(plan.progress[0].status, PlanStepStatus::Completed);
        assert_eq!(plan.progress[1].id, "adapt");
        let prompt = plan.approved_prompt_section();
        assert!(prompt.contains("### Current execution progress"));
        assert!(prompt.contains("`inspect` [completed] Inspect the code — note: Checked"));
        assert!(
            prompt
                .contains("`adapt` [pending] Handle the discovered edge case (runtime adaptation)")
        );
        assert!(
            prompt.contains("deviation reason: Inspection exposed a new compatibility requirement")
        );
    }

    #[test]
    fn rejected_progress_update_does_not_apply_an_earlier_change() {
        let artifact = validate_submission_json(&ready_plan_json(""))
            .expect("plan should validate")
            .artifact;
        let mut plan = PendingPlan::new(
            "plan-atomic".into(),
            0,
            1,
            1,
            2,
            PlanStatus::Executing,
            artifact,
            Vec::new(),
            false,
        );
        let original_progress = plan.progress.clone();
        let update = validate_progress_json(
            r#"{
                "base_revision":2,
                "updates":[
                    {"id":"inspect","status":"completed","note":"Must roll back"},
                    {"id":"missing","status":"blocked","note":"Unknown step"}
                ]
            }"#,
        )
        .expect("update shape should validate");

        assert!(
            apply_progress_update(&mut plan, update)
                .expect_err("unknown step should reject the complete update")
                .contains("unknown plan step")
        );
        assert_eq!(plan.progress, original_progress);
    }

    #[test]
    fn persisted_contract_without_server_checks_cannot_complete_from_notes_alone() {
        let artifact = PlanArtifact {
            title: "Legacy structured plan".into(),
            goal: "Produce a verified result".into(),
            steps: vec![PlanStep {
                id: "write".into(),
                title: "Write the result".into(),
                ..Default::default()
            }],
            verification: vec!["Verify result.txt".into()],
            acceptance_criteria: vec!["result.txt has the requested bytes".into()],
            ..Default::default()
        };
        let mut plan = PendingPlan::new(
            "plan-missing-contract".into(),
            0,
            1,
            1,
            4,
            PlanStatus::Executing,
            artifact.clone(),
            Vec::new(),
            false,
        );
        plan.progress[0].status = PlanStepStatus::Completed;
        plan.progress[0].note = "Everything is correct".into();

        let report = evaluate_completion_contract(&plan, Path::new("."), &BTreeMap::new(), 0);
        assert!(!report.passed());
        assert_eq!(report.plan_id, "plan-missing-contract");
        assert_eq!(report.revision, 4);
        assert_eq!(report.failures[0].check_id, "completion-contract-missing");

        apply_completion_failures(&mut plan, &report)
            .expect("the server failure should bind to the original step");
        assert_eq!(plan.artifact, artifact);
        assert_eq!(plan.progress[0].status, PlanStepStatus::Blocked);
        assert!(
            plan.progress[0]
                .note
                .contains("completion-contract-missing")
        );
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn completion_file_and_absent_checks_reject_a_final_symbolic_link() {
        let mut guard = CompletionFilesystemGuard::new("file-link");
        let workspace = guard.root.join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace should be created");
        let target = workspace.join("target.txt");
        std::fs::write(&target, "inside").expect("target fixture should be written");
        let link = workspace.join("linked.txt");
        #[cfg(unix)]
        let link_result = std::os::unix::fs::symlink(&target, &link);
        #[cfg(windows)]
        let link_result = std::os::windows::fs::symlink_file(&target, &link);
        #[cfg(windows)]
        if let Err(error) = &link_result {
            eprintln!("skipping Windows file-symlink completion test: {error}");
            return;
        }
        link_result.expect("symbolic link should be created");
        guard.track_link(link, false);
        let cancelled = AtomicBool::new(false);
        let control = CompletionVerificationControl {
            deadline: Instant::now() + Duration::from_secs(5),
            cancelled: &cancelled,
        };

        for expected_type in [PlanExpectedPathType::File, PlanExpectedPathType::Absent] {
            let check = completion_workspace_check("linked.txt", expected_type);
            let reason = completion_check_failure(evaluate_workspace_path_check(
                &workspace, &check, &control,
            ));
            assert!(
                reason.contains("link") || reason.contains("reparse"),
                "unexpected link rejection: {reason}"
            );
        }
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn completion_directory_check_rejects_a_symbolic_link() {
        let mut guard = CompletionFilesystemGuard::new("directory-link");
        let workspace = guard.root.join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace should be created");
        let target = workspace.join("target-directory");
        std::fs::create_dir_all(&target).expect("target fixture should be created");
        let link = workspace.join("linked-directory");
        #[cfg(unix)]
        let link_result = std::os::unix::fs::symlink(&target, &link);
        #[cfg(windows)]
        let link_result = std::os::windows::fs::symlink_dir(&target, &link);
        #[cfg(windows)]
        if let Err(error) = &link_result {
            eprintln!("skipping Windows directory-symlink completion test: {error}");
            return;
        }
        link_result.expect("directory symbolic link should be created");
        guard.track_link(link, cfg!(windows));
        let cancelled = AtomicBool::new(false);
        let control = CompletionVerificationControl {
            deadline: Instant::now() + Duration::from_secs(5),
            cancelled: &cancelled,
        };
        let check = completion_workspace_check("linked-directory", PlanExpectedPathType::Directory);

        let reason =
            completion_check_failure(evaluate_workspace_path_check(&workspace, &check, &control));
        assert!(
            reason.contains("link") || reason.contains("reparse") || reason.contains("outside"),
            "unexpected directory link rejection: {reason}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn completion_directory_check_rejects_a_windows_junction() {
        let mut guard = CompletionFilesystemGuard::new("junction");
        let workspace = guard.root.join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace should be created");
        let target = workspace.join("target-directory");
        std::fs::create_dir_all(&target).expect("target fixture should be created");
        let junction = workspace.join("linked-directory");
        let output = std::process::Command::new("cmd")
            .args([
                "/C",
                "mklink",
                "/J",
                &junction.to_string_lossy(),
                &target.to_string_lossy(),
            ])
            .output()
            .expect("junction command should run");
        if !output.status.success() {
            eprintln!(
                "skipping Windows junction completion test: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            return;
        }
        guard.track_link(junction, true);
        let cancelled = AtomicBool::new(false);
        let control = CompletionVerificationControl {
            deadline: Instant::now() + Duration::from_secs(5),
            cancelled: &cancelled,
        };
        let check = completion_workspace_check("linked-directory", PlanExpectedPathType::Directory);

        let reason =
            completion_check_failure(evaluate_workspace_path_check(&workspace, &check, &control));
        assert!(
            reason.contains("reparse") || reason.contains("outside") || reason.contains("link"),
            "unexpected junction rejection: {reason}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn completion_checks_reject_a_workspace_root_replaced_by_a_windows_junction() {
        let mut guard = CompletionFilesystemGuard::new("root-junction");
        let workspace = guard.root.join("workspace");
        let original_workspace = guard.root.join("original-workspace");
        let outside = guard.root.join("outside-directory");
        std::fs::create_dir_all(workspace.join("original-only"))
            .expect("original workspace should be created");
        std::fs::create_dir_all(outside.join("output")).expect("outside fixture should be created");
        std::fs::write(outside.join("result.txt"), "outside")
            .expect("outside file should be written");
        std::fs::rename(&workspace, &original_workspace)
            .expect("workspace root should be moved before replacement");
        let output = std::process::Command::new("cmd")
            .args([
                "/C",
                "mklink",
                "/J",
                &workspace.to_string_lossy(),
                &outside.to_string_lossy(),
            ])
            .output()
            .expect("junction command should run");
        if !output.status.success() {
            eprintln!(
                "skipping Windows root-junction completion test: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            return;
        }
        guard.track_link(workspace.clone(), true);
        let cancelled = AtomicBool::new(false);
        let control = CompletionVerificationControl {
            deadline: Instant::now() + Duration::from_secs(5),
            cancelled: &cancelled,
        };

        for (path, expected_type) in [
            ("result.txt", PlanExpectedPathType::File),
            ("output", PlanExpectedPathType::Directory),
            ("missing", PlanExpectedPathType::Absent),
        ] {
            let check = completion_workspace_check(path, expected_type);
            let reason = completion_check_failure(evaluate_workspace_path_check(
                &workspace, &check, &control,
            ));
            assert!(
                reason.contains("workspace")
                    || reason.contains("junction")
                    || reason.contains("reparse"),
                "unexpected root-junction rejection: {reason}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn completion_checks_reject_a_workspace_root_replaced_by_a_symbolic_link() {
        let mut guard = CompletionFilesystemGuard::new("root-symlink");
        let workspace = guard.root.join("workspace");
        let original_workspace = guard.root.join("original-workspace");
        let outside = guard.root.join("outside-directory");
        std::fs::create_dir_all(workspace.join("original-only"))
            .expect("original workspace should be created");
        std::fs::create_dir_all(outside.join("output")).expect("outside fixture should be created");
        std::fs::write(outside.join("result.txt"), "outside")
            .expect("outside file should be written");
        std::fs::rename(&workspace, &original_workspace)
            .expect("workspace root should be moved before replacement");
        std::os::unix::fs::symlink(&outside, &workspace)
            .expect("workspace root replacement symlink should be created");
        guard.track_link(workspace.clone(), true);
        let cancelled = AtomicBool::new(false);
        let control = CompletionVerificationControl {
            deadline: Instant::now() + Duration::from_secs(5),
            cancelled: &cancelled,
        };

        for (path, expected_type) in [
            ("result.txt", PlanExpectedPathType::File),
            ("output", PlanExpectedPathType::Directory),
            ("missing", PlanExpectedPathType::Absent),
        ] {
            let check = completion_workspace_check(path, expected_type);
            let reason = completion_check_failure(evaluate_workspace_path_check(
                &workspace, &check, &control,
            ));
            assert!(
                reason.contains("workspace") || reason.contains("link"),
                "unexpected root-symlink rejection: {reason}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn checked_windows_root_handle_prevents_namespace_replacement_during_open() {
        let mut guard = CompletionFilesystemGuard::new("opened-root-windows");
        let workspace = guard.root.join("workspace");
        let moved_workspace = guard.root.join("moved-workspace");
        std::fs::create_dir_all(&workspace).expect("workspace should be created");
        std::fs::write(workspace.join("result.txt"), "inside")
            .expect("inside fixture should be written");
        guard.track_link(workspace.clone(), true);
        let mut rename_error = None;
        let mut hook = || {
            if let Err(error) = std::fs::rename(&workspace, &moved_workspace) {
                rename_error = Some(error);
            }
        };

        let checked = crate::tools::safety::resolve_path_checked("result.txt", &workspace)
            .expect("path capability should open");
        let opened =
            crate::tools::safety::open_checked_workspace_entry_after_root_hook(&checked, &mut hook);
        if rename_error.is_some() {
            assert!(
                opened.is_ok(),
                "a namespace that remained stable should open"
            );
        } else {
            let error = match opened {
                Err(error) => error,
                Ok(_) => panic!("a moved root namespace must fail closed"),
            };
            assert!(error.contains("root") && error.contains("changed"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn checked_unix_root_handle_fails_closed_after_namespace_replacement() {
        let mut guard = CompletionFilesystemGuard::new("opened-root-unix");
        let workspace = guard.root.join("workspace");
        let moved_workspace = guard.root.join("moved-workspace");
        let outside = guard.root.join("outside");
        std::fs::create_dir_all(&workspace).expect("workspace should be created");
        std::fs::create_dir_all(&outside).expect("outside fixture should be created");
        std::fs::write(workspace.join("result.txt"), "inside")
            .expect("inside fixture should be written");
        std::fs::write(outside.join("result.txt"), "outside")
            .expect("outside fixture should be written");
        guard.track_link(workspace.clone(), true);
        let mut hook = || {
            std::fs::rename(&workspace, &moved_workspace)
                .expect("Unix should permit renaming an opened directory");
            std::os::unix::fs::symlink(&outside, &workspace)
                .expect("replacement root symlink should be created");
        };

        let checked = crate::tools::safety::resolve_path_checked("result.txt", &workspace)
            .expect("path capability should open");
        let error = match crate::tools::safety::open_checked_workspace_entry_after_root_hook(
            &checked, &mut hook,
        ) {
            Err(error) => error,
            Ok(_) => panic!("a replaced root namespace must fail closed"),
        };
        assert!(error.contains("root") && error.contains("changed"));
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn initial_plan_evidence_hash_rejects_an_intermediate_replacement_after_resolution() {
        #[cfg(unix)]
        let mut guard = CompletionFilesystemGuard::new("initial-evidence-race");
        #[cfg(windows)]
        let guard = CompletionFilesystemGuard::new("initial-evidence-race");
        let workspace = guard.root.join("workspace");
        #[cfg(unix)]
        let original = guard.root.join("original-subtree");
        let outside = guard.root.join("outside");
        std::fs::create_dir_all(workspace.join("subtree"))
            .expect("workspace subtree should be created");
        std::fs::create_dir_all(&outside).expect("outside subtree should be created");
        std::fs::write(workspace.join("subtree/evidence.txt"), "inside")
            .expect("inside evidence should be written");
        std::fs::write(outside.join("evidence.txt"), "outside")
            .expect("outside evidence should be written");
        let checked =
            crate::tools::safety::resolve_path_checked("subtree/evidence.txt", &workspace)
                .expect("initial evidence path should resolve");
        #[cfg(windows)]
        {
            let moved_file = guard.root.join("moved-evidence.txt");
            let replacement_file = workspace.join("subtree/replacement.txt");
            std::fs::write(&replacement_file, "replacement")
                .expect("replacement evidence should be written");
            std::fs::rename(workspace.join("subtree/evidence.txt"), &moved_file)
                .expect("resolved evidence file should be moved");
            std::fs::rename(&replacement_file, workspace.join("subtree/evidence.txt"))
                .expect("replacement evidence should be installed");
            let error = hash_file(&checked)
                .expect_err("Plan evidence must reject a replaced final file identity");
            assert!(
                error.contains("changed") || error.contains("identity"),
                "unexpected evidence identity rejection: {error}"
            );
        }
        #[cfg(unix)]
        {
            std::fs::rename(workspace.join("subtree"), &original)
                .expect("workspace subtree should be moved");
            let replacement = workspace.join("subtree");
            #[cfg(unix)]
            std::os::unix::fs::symlink(&outside, &replacement)
                .expect("replacement symlink should be created");
            #[cfg(windows)]
            {
                let output = std::process::Command::new("cmd.exe")
                    .arg("/c")
                    .arg("mklink")
                    .arg("/J")
                    .arg(&replacement)
                    .arg(&outside)
                    .output()
                    .expect("junction command should run");
                assert!(
                    output.status.success(),
                    "replacement junction should be created: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            guard.track_link(replacement, cfg!(windows));

            let error = hash_file(&checked)
                .expect_err("Plan evidence must not hash the replacement outside file");

            assert!(
                error.contains("link")
                    || error.contains("reparse")
                    || error.contains("mount")
                    || error.contains("cannot open")
                    || error.contains("changed"),
                "unexpected evidence race rejection: {error}"
            );
        }
    }

    #[cfg(target_os = "linux")]
    fn run_bind_mount_completion_check() -> Result<(), String> {
        use std::os::unix::ffi::OsStrExt as _;

        let mut guard = CompletionFilesystemGuard::new("bind-mount");
        let workspace = guard.root.join("workspace");
        let mountpoint = workspace.join("mounted");
        let outside = guard.root.join("outside");
        std::fs::create_dir_all(&mountpoint).expect("mountpoint should be created");
        std::fs::create_dir_all(outside.join("output")).expect("outside fixture should be created");
        std::fs::write(outside.join("result.txt"), "outside")
            .expect("outside fixture should be written");
        let source = std::ffi::CString::new(outside.as_os_str().as_bytes())
            .expect("source path should not contain NUL");
        let target = std::ffi::CString::new(mountpoint.as_os_str().as_bytes())
            .expect("target path should not contain NUL");
        // SAFETY: both paths are NUL-terminated and point to test-owned directories.
        let result = unsafe {
            libc::mount(
                source.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND,
                std::ptr::null(),
            )
        };
        if result == -1 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        guard.track_mount(mountpoint);
        let cancelled = AtomicBool::new(false);
        let control = CompletionVerificationControl {
            deadline: Instant::now() + Duration::from_secs(5),
            cancelled: &cancelled,
        };

        for (path, expected_type) in [
            ("mounted/result.txt", PlanExpectedPathType::File),
            ("mounted/output", PlanExpectedPathType::Directory),
            ("mounted/missing", PlanExpectedPathType::Absent),
        ] {
            let check = completion_workspace_check(path, expected_type);
            let reason = completion_check_failure(evaluate_workspace_path_check(
                &workspace, &check, &control,
            ));
            assert!(
                reason.contains("mount boundary"),
                "unexpected bind-mount rejection: {reason}"
            );
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn report_bind_mount_test_status(message: &str) {
        let bytes = format!("{message}\n").into_bytes();
        let mut offset = 0usize;
        while offset < bytes.len() {
            // SAFETY: bytes points to a live buffer and STDERR_FILENO is the
            // process diagnostic stream. Writing directly intentionally
            // bypasses libtest's successful-test capture so a skipped real
            // mount probe is visible in an ordinary cargo test run.
            let written = unsafe {
                libc::write(
                    libc::STDERR_FILENO,
                    bytes[offset..].as_ptr().cast(),
                    bytes.len() - offset,
                )
            };
            if written <= 0 {
                break;
            }
            offset += written as usize;
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "run only inside the bind-mount namespace harness"]
    fn completion_bind_mount_namespace_helper() {
        assert_eq!(
            std::env::var("LINGCLAW_BIND_MOUNT_NAMESPACE_HELPER").as_deref(),
            Ok("1"),
            "the ignored helper must be invoked only by its namespace harness"
        );
        run_bind_mount_completion_check()
            .expect("the isolated namespace must permit the real bind-mount check");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn completion_checks_reject_a_bind_mounted_workspace_subtree() {
        let current_exe = std::env::current_exe().expect("locate Rust test executable");
        let helper_name = "plan::tests::completion_bind_mount_namespace_helper";
        let namespace = std::process::Command::new("unshare")
            .args(["--user", "--map-root-user", "--mount", "--fork"])
            .arg(&current_exe)
            .args(["--ignored", "--exact", helper_name, "--nocapture"])
            .env("LINGCLAW_BIND_MOUNT_NAMESPACE_HELPER", "1")
            .output();
        if namespace
            .as_ref()
            .is_ok_and(|output| output.status.success())
        {
            report_bind_mount_test_status(
                "REAL BIND-MOUNT TEST EXECUTED inside an isolated user+mount namespace",
            );
            return;
        }

        // Rootful builders may forbid user namespaces while still allowing a
        // tightly scoped bind mount. Try that path before reporting a skip.
        if run_bind_mount_completion_check().is_ok() {
            report_bind_mount_test_status(
                "REAL BIND-MOUNT TEST EXECUTED through the scoped direct-mount fallback",
            );
            return;
        }

        let namespace_error = match namespace {
            Ok(output) => format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
            Err(error) => error.to_string(),
        };
        let message = format!(
            "REAL BIND-MOUNT TEST NOT EXECUTED: user/mount namespace and direct mount were unavailable ({})",
            namespace_error.trim()
        );
        if std::env::var("LINGCLAW_REQUIRE_MOUNT_TEST").as_deref() == Ok("1") {
            panic!("{message}");
        }
        report_bind_mount_test_status(&format!(
            "{message}; set LINGCLAW_REQUIRE_MOUNT_TEST=1 to make this a hard failure"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn completion_file_read_stays_on_the_opened_handle_during_path_replacement() {
        let mut guard = CompletionFilesystemGuard::new("opened-handle-race");
        let workspace = guard.root.join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace should be created");
        let result_path = workspace.join("result.txt");
        let moved_path = workspace.join("opened-result.txt");
        let outside = guard.root.join("outside.txt");
        std::fs::write(&result_path, "inside").expect("inside fixture should be written");
        std::fs::write(&outside, "secret").expect("outside fixture should be written");
        guard.track_link(result_path.clone(), false);
        let mut check = completion_workspace_check("result.txt", PlanExpectedPathType::File);
        check.exact_content = Some("secret".into());
        check.size_bytes = Some(6);
        let cancelled = AtomicBool::new(false);
        let control = CompletionVerificationControl {
            deadline: Instant::now() + Duration::from_secs(5),
            cancelled: &cancelled,
        };
        let mut replaced = false;
        let mut hook = |stage: CompletionPathCheckStage, _: &Path| {
            if stage == CompletionPathCheckStage::AfterOpen && !replaced {
                std::fs::rename(&result_path, &moved_path)
                    .expect("the opened file path should be movable on Unix");
                std::os::unix::fs::symlink(&outside, &result_path)
                    .expect("replacement link should be created");
                replaced = true;
            }
        };

        let reason = completion_check_failure(evaluate_workspace_path_check_with_hook(
            &workspace, &check, &control, &mut hook,
        ));
        assert!(replaced);
        assert!(
            reason.contains("changed") || reason.contains("differ"),
            "the verifier must fail closed on the replacement: {reason}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn completion_file_read_is_bound_to_the_opened_windows_handle_during_replacement() {
        let guard = CompletionFilesystemGuard::new("opened-windows-handle-race");
        let workspace = guard.root.join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace should be created");
        let result_path = workspace.join("result.txt");
        let moved_path = workspace.join("opened-result.txt");
        std::fs::write(&result_path, "inside").expect("inside fixture should be written");
        let mut check = completion_workspace_check("result.txt", PlanExpectedPathType::File);
        check.exact_content = Some("inside".into());
        check.size_bytes = Some(6);
        let cancelled = AtomicBool::new(false);
        let control = CompletionVerificationControl {
            deadline: Instant::now() + Duration::from_secs(5),
            cancelled: &cancelled,
        };
        let mut attempted = false;
        let mut replaced = false;
        let result = {
            let mut hook = |stage: CompletionPathCheckStage, _: &Path| {
                if stage == CompletionPathCheckStage::AfterOpen && !attempted {
                    attempted = true;
                    if std::fs::rename(&result_path, &moved_path).is_ok() {
                        replaced = true;
                        std::fs::write(&result_path, "secret")
                            .expect("replacement fixture should be written");
                    }
                }
            };
            evaluate_workspace_path_check_with_hook(&workspace, &check, &control, &mut hook)
        };
        assert!(attempted);
        if replaced {
            let reason = completion_check_failure(result);
            assert!(reason.contains("changed"));
        } else {
            result.unwrap_or_else(|_| {
                panic!("delete sharing was denied, so the checked path should remain stable")
            });
        }
    }

    #[test]
    fn completion_hash_enforces_the_byte_limit_when_the_opened_file_grows() {
        let guard = CompletionFilesystemGuard::new("growing-file");
        let workspace = guard.root.join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace should be created");
        let result_path = workspace.join("result.bin");
        std::fs::write(&result_path, b"small").expect("fixture should be written");
        let mut check = completion_workspace_check("result.bin", PlanExpectedPathType::File);
        check.sha256 = Some(format!("{:x}", Sha256::digest(b"small")));
        let cancelled = AtomicBool::new(false);
        let control = CompletionVerificationControl {
            deadline: Instant::now() + Duration::from_secs(30),
            cancelled: &cancelled,
        };
        let mut grew = false;
        let mut hook = |stage: CompletionPathCheckStage, _: &Path| {
            if stage == CompletionPathCheckStage::AfterOpen && !grew {
                let file = std::fs::OpenOptions::new()
                    .write(true)
                    .open(&result_path)
                    .expect("the checked file should remain share-write capable");
                file.set_len(MAX_PLAN_COMPLETION_FILE_BYTES + 1)
                    .expect("the sparse fixture should grow");
                grew = true;
            }
        };

        let reason = completion_check_failure(evaluate_workspace_path_check_with_hook(
            &workspace, &check, &control, &mut hook,
        ));
        assert!(grew);
        assert!(
            reason.contains("read limit") || reason.contains("changed"),
            "unexpected growing-file rejection: {reason}"
        );
    }

    #[test]
    fn completion_hash_rejects_an_initial_file_above_the_hard_limit() {
        let guard = CompletionFilesystemGuard::new("large-file");
        let workspace = guard.root.join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace should be created");
        let result_path = workspace.join("result.bin");
        let file = std::fs::File::create(&result_path).expect("fixture should be created");
        file.set_len(MAX_PLAN_COMPLETION_FILE_BYTES + 1)
            .expect("sparse fixture should be extended");
        let mut check = completion_workspace_check("result.bin", PlanExpectedPathType::File);
        check.sha256 = Some("0".repeat(64));
        let cancelled = AtomicBool::new(false);
        let control = CompletionVerificationControl {
            deadline: Instant::now() + Duration::from_secs(5),
            cancelled: &cancelled,
        };

        let reason =
            completion_check_failure(evaluate_workspace_path_check(&workspace, &check, &control));
        assert!(reason.contains("completion-hash limit"));
    }

    #[test]
    fn completion_absence_rechecks_the_retained_parent_after_the_first_probe() {
        let guard = CompletionFilesystemGuard::new("absent-race");
        let workspace = guard.root.join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace should be created");
        let result_path = workspace.join("unexpected.txt");
        let check = completion_workspace_check("unexpected.txt", PlanExpectedPathType::Absent);
        let cancelled = AtomicBool::new(false);
        let control = CompletionVerificationControl {
            deadline: Instant::now() + Duration::from_secs(5),
            cancelled: &cancelled,
        };
        let mut created = false;
        let mut hook = |stage: CompletionPathCheckStage, _: &Path| {
            if stage == CompletionPathCheckStage::AfterOpen && !created {
                std::fs::write(&result_path, "late")
                    .expect("late absence-race fixture should be created");
                created = true;
            }
        };

        let reason = completion_check_failure(evaluate_workspace_path_check_with_hook(
            &workspace, &check, &control, &mut hook,
        ));

        assert!(created);
        assert!(
            reason.contains("appeared"),
            "unexpected absence race: {reason}"
        );
    }

    #[test]
    fn completion_path_checks_accept_a_checked_directory_and_missing_path() {
        let guard = CompletionFilesystemGuard::new("normal-paths");
        let workspace = guard.root.join("workspace");
        std::fs::create_dir_all(workspace.join("output"))
            .expect("workspace directory should be created");
        let cancelled = AtomicBool::new(false);
        let control = CompletionVerificationControl {
            deadline: Instant::now() + Duration::from_secs(5),
            cancelled: &cancelled,
        };

        evaluate_workspace_path_check(
            &workspace,
            &completion_workspace_check("output", PlanExpectedPathType::Directory),
            &control,
        )
        .unwrap_or_else(|_| panic!("checked directory should pass"));
        evaluate_workspace_path_check(
            &workspace,
            &completion_workspace_check("missing", PlanExpectedPathType::Absent),
            &control,
        )
        .unwrap_or_else(|_| panic!("checked absence should pass"));
    }

    #[test]
    fn evidence_fingerprint_detects_workspace_changes() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be valid")
            .as_nanos();
        let workspace = std::env::temp_dir().join(format!("lingclaw-plan-evidence-{unique}"));
        std::fs::create_dir_all(&workspace).expect("workspace should be created");
        let file = workspace.join("notes.txt");
        std::fs::write(&file, "before").expect("fixture should be written");

        let evidence = capture_tool_evidence(
            crate::tools::TOOL_NAME_READ_FILE,
            r#"{"path":"notes.txt"}"#,
            &workspace,
        );
        assert_eq!(evidence.len(), 1);
        assert!(verify_evidence(&workspace, &evidence).is_empty());

        std::fs::write(&file, "after").expect("fixture should change");
        assert_eq!(verify_evidence(&workspace, &evidence), vec!["notes.txt"]);
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn evidence_capture_reports_failures_instead_of_returning_an_empty_snapshot() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be valid")
            .as_nanos();
        let workspace = std::env::temp_dir().join(format!("lingclaw-plan-missing-{unique}"));
        std::fs::create_dir_all(&workspace).expect("workspace should be created");

        let error = try_capture_tool_evidence(
            crate::tools::TOOL_NAME_SEARCH_FILES,
            r#"{"path":"missing","pattern":"anything"}"#,
            &workspace,
        )
        .expect_err("an unreadable evidence root must not look like an empty snapshot");
        assert!(!error.is_empty());

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn tool_evidence_requires_a_stable_execution_window() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be valid")
            .as_nanos();
        let workspace = std::env::temp_dir().join(format!("lingclaw-plan-stable-{unique}"));
        std::fs::create_dir_all(&workspace).expect("workspace should be created");
        let file = workspace.join("notes.txt");
        std::fs::write(&file, "before").expect("fixture should be written");
        let args = r#"{"path":"notes.txt"}"#;

        let stable_before =
            capture_tool_evidence(crate::tools::TOOL_NAME_READ_FILE, args, &workspace);
        let stable_after =
            capture_tool_evidence(crate::tools::TOOL_NAME_READ_FILE, args, &workspace);
        let stable = reconcile_tool_evidence(stable_before, stable_after);
        assert!(verify_evidence(&workspace, &stable).is_empty());

        let changed_before =
            capture_tool_evidence(crate::tools::TOOL_NAME_READ_FILE, args, &workspace);
        std::fs::write(&file, "after").expect("fixture should change during the read window");
        let changed_after =
            capture_tool_evidence(crate::tools::TOOL_NAME_READ_FILE, args, &workspace);
        let unstable = reconcile_tool_evidence(changed_before, changed_after);
        assert_eq!(unstable[0].fingerprint, "unstable");
        assert_eq!(verify_evidence(&workspace, &unstable), vec!["notes.txt"]);

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn evidence_verification_stops_when_cancelled() {
        let evidence = [PlanEvidence {
            path: "notes.txt".into(),
            kind: PlanEvidenceKind::File,
            fingerprint: "unused".into(),
            selector: None,
        }];
        let cancelled = AtomicBool::new(true);

        assert_eq!(
            verify_evidence_until(
                Path::new("."),
                &evidence,
                Instant::now() + std::time::Duration::from_secs(1),
                &cancelled,
            ),
            Err(EvidenceVerificationError::Cancelled)
        );
    }

    #[test]
    fn evidence_verification_stops_after_deadline() {
        let evidence = [PlanEvidence {
            path: "notes.txt".into(),
            kind: PlanEvidenceKind::File,
            fingerprint: "unused".into(),
            selector: None,
        }];
        let cancelled = AtomicBool::new(false);

        assert_eq!(
            verify_evidence_until(Path::new("."), &evidence, Instant::now(), &cancelled,),
            Err(EvidenceVerificationError::TimedOut)
        );
    }

    #[test]
    fn evidence_hash_samples_large_file_tail() {
        use std::io::{Seek as _, Write as _};

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be valid")
            .as_nanos();
        let workspace = std::env::temp_dir().join(format!("lingclaw-plan-large-{unique}"));
        std::fs::create_dir_all(&workspace).expect("workspace should be created");
        let path = workspace.join("large.bin");
        let mut file = std::fs::File::create(&path).expect("fixture should be created");
        file.write_all(b"head").expect("prefix should be written");
        file.set_len(MAX_EVIDENCE_HASH_FILE_BYTES + 1_024)
            .expect("fixture should be extended");
        file.seek(std::io::SeekFrom::End(-4))
            .expect("fixture tail should be seekable");
        file.write_all(b"tail").expect("tail should be written");
        drop(file);

        let capture = try_capture_tool_evidence(
            crate::tools::TOOL_NAME_READ_FILE,
            r#"{"path":"large.bin"}"#,
            &workspace,
        )
        .expect("large-file evidence should be captured");
        assert!(capture.truncated);
        let evidence = capture.evidence;
        assert_eq!(evidence.len(), 1);
        assert!(verify_evidence(&workspace, &evidence).is_empty());

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("fixture should reopen");
        file.seek(std::io::SeekFrom::End(-4))
            .expect("fixture tail should be seekable");
        file.write_all(b"fail").expect("tail should change");
        drop(file);

        assert_eq!(verify_evidence(&workspace, &evidence), vec!["large.bin"]);
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn search_evidence_detects_same_size_changes_in_nested_files() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be valid")
            .as_nanos();
        let workspace = std::env::temp_dir().join(format!("lingclaw-plan-search-{unique}"));
        let nested = workspace.join("src").join("nested");
        std::fs::create_dir_all(&nested).expect("nested fixture should be created");
        let file = nested.join("notes.txt");
        std::fs::write(&file, "alpha").expect("fixture should be written");

        let capture = try_capture_tool_evidence(
            crate::tools::TOOL_NAME_SEARCH_FILES,
            r#"{"path":".","pattern":"alpha"}"#,
            &workspace,
        )
        .expect("search evidence should be captured");
        assert!(!capture.truncated);
        let evidence = capture.evidence;
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].kind, PlanEvidenceKind::DirectoryTree);
        assert!(verify_evidence(&workspace, &evidence).is_empty());

        std::fs::write(&file, "bravo").expect("fixture should change without changing size");
        assert_eq!(verify_evidence(&workspace, &evidence), vec!["."]);
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn search_evidence_marks_depth_limit_as_incomplete() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be valid")
            .as_nanos();
        let workspace = std::env::temp_dir().join(format!("lingclaw-plan-depth-{unique}"));
        let mut nested = workspace.clone();
        for level in 0..=5 {
            nested.push(format!("level-{level}"));
        }
        std::fs::create_dir_all(&nested).expect("deep fixture should be created");
        std::fs::write(nested.join("notes.txt"), "needle").expect("deep fixture should be written");

        let capture = try_capture_tool_evidence(
            crate::tools::TOOL_NAME_SEARCH_FILES,
            r#"{"path":".","pattern":"needle"}"#,
            &workspace,
        )
        .expect("bounded search evidence should still be captured");

        assert!(capture.truncated);
        assert_eq!(capture.evidence.len(), 1);
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn directory_tree_hash_marks_file_limit_as_incomplete() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be valid")
            .as_nanos();
        let workspace = std::env::temp_dir().join(format!("lingclaw-plan-files-{unique}"));
        std::fs::create_dir_all(&workspace).expect("workspace should be created");
        std::fs::write(workspace.join("a.txt"), "a").expect("fixture should be written");
        std::fs::write(workspace.join("b.txt"), "b").expect("fixture should be written");

        let (_, truncated) = hash_directory_tree_with_limits(&workspace, 5, 1, 4, 8)
            .expect("bounded tree hash should succeed");

        assert!(truncated);
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn directory_tree_hash_marks_content_budget_as_incomplete() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be valid")
            .as_nanos();
        let workspace = std::env::temp_dir().join(format!("lingclaw-plan-budget-{unique}"));
        std::fs::create_dir_all(&workspace).expect("workspace should be created");
        std::fs::write(workspace.join("a.txt"), "abcd").expect("fixture should be written");
        std::fs::write(workspace.join("b.txt"), "efgh").expect("fixture should be written");

        let (_, truncated) = hash_directory_tree_with_limits(&workspace, 5, 10, 4, 4)
            .expect("budgeted tree hash should succeed");

        assert!(truncated);
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn shallow_directory_evidence_does_not_replace_recursive_evidence() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be valid")
            .as_nanos();
        let workspace = std::env::temp_dir().join(format!("lingclaw-plan-merge-{unique}"));
        let nested = workspace.join("src").join("nested");
        std::fs::create_dir_all(&nested).expect("nested fixture should be created");
        let file = nested.join("notes.txt");
        std::fs::write(&file, "alpha").expect("fixture should be written");

        let mut evidence = capture_tool_evidence(
            crate::tools::TOOL_NAME_SEARCH_FILES,
            r#"{"path":".","pattern":"alpha"}"#,
            &workspace,
        );
        let shallow = capture_tool_evidence(
            crate::tools::TOOL_NAME_LIST_DIR,
            r#"{"path":"."}"#,
            &workspace,
        );
        assert!(!merge_evidence(&mut evidence, shallow));
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].kind, PlanEvidenceKind::DirectoryTree);

        std::fs::write(&file, "bravo").expect("nested fixture should change");
        assert_eq!(verify_evidence(&workspace, &evidence), vec!["."]);
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn git_evidence_ignores_unrelated_metadata_and_detects_workspace_changes() {
        fn git(workspace: &Path, args: &[&str]) {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(workspace)
                .output()
                .expect("git should run");
            assert!(
                output.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be valid")
            .as_nanos();
        let workspace = std::env::temp_dir().join(format!("lingclaw-plan-git-{unique}"));
        std::fs::create_dir_all(&workspace).expect("workspace should be created");
        git(&workspace, &["init"]);
        git(
            &workspace,
            &["config", "user.email", "plan@example.invalid"],
        );
        git(&workspace, &["config", "user.name", "Plan Test"]);
        git(&workspace, &["commit", "--allow-empty", "-m", "initial"]);

        let evidence = capture_tool_evidence(
            crate::tools::TOOL_NAME_GIT_INSPECT,
            r#"{"operation":"log","max_entries":20}"#,
            &workspace,
        );
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].kind, PlanEvidenceKind::Git);
        assert!(verify_evidence(&workspace, &evidence).is_empty());

        git(
            &workspace,
            &["commit", "--allow-empty", "-m", "metadata only"],
        );
        assert!(
            verify_evidence(&workspace, &evidence).is_empty(),
            "path-scoped Git evidence must ignore commits that touched no workspace path"
        );

        std::fs::write(workspace.join("tracked.txt"), "workspace change")
            .expect("workspace fixture should write");
        git(&workspace, &["add", "tracked.txt"]);
        git(&workspace, &["commit", "-m", "workspace change"]);
        assert_eq!(verify_evidence(&workspace, &evidence), vec!["."]);
        let _ = std::fs::remove_dir_all(workspace);
    }
}
