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
        let unfinished_steps = self
            .progress
            .iter()
            .filter(|step| {
                !matches!(
                    step.status,
                    PlanStepStatus::Completed | PlanStepStatus::Skipped
                )
            })
            .count();
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
            "\n\nFollow this approved plan as an execution contract. Use `update_plan` to report progress. You may append an adaptation step only when new evidence requires it, and must include a deviation reason. Do not silently change the goal or acceptance criteria.",
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
    let relative = workspace_relative_path(workspace, &resolved)?;
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

fn workspace_relative_path(workspace: &Path, path: &Path) -> Result<String, String> {
    let root = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let relative = canonical
        .strip_prefix(&root)
        .map_err(|_| "evidence path is outside the session workspace".to_string())?;
    let value = if relative.as_os_str().is_empty() {
        ".".to_string()
    } else {
        relative.to_string_lossy().replace('\\', "/")
    };
    Ok(value)
}

fn hash_file(path: &Path) -> Result<(String, bool), String> {
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("evidence path is not a file".to_string());
    }
    let mut digest = Sha256::new();
    hash_file_metadata(&metadata, &mut digest);
    let mut remaining = MAX_EVIDENCE_HASH_FILE_BYTES;
    let truncated = hash_file_content(path, metadata.len(), &mut remaining, &mut digest)?;
    Ok((format!("{:x}", digest.finalize()), truncated))
}

fn hash_directory(path: &Path) -> Result<String, String> {
    let mut entries = fs::read_dir(path)
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    entries.sort_by_key(|entry| entry.file_name());
    let mut digest = Sha256::new();
    for entry in entries {
        let metadata = entry.metadata().map_err(|error| error.to_string())?;
        digest.update(entry.file_name().to_string_lossy().as_bytes());
        digest.update(if metadata.is_dir() { b"d" } else { b"f" });
        digest.update(metadata.len().to_le_bytes());
        if let Ok(modified) = metadata.modified()
            && let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH)
        {
            digest.update(duration.as_nanos().to_le_bytes());
        }
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn hash_directory_tree(path: &Path) -> Result<(String, bool), String> {
    hash_directory_tree_with_limits(
        path,
        5,
        10_000,
        MAX_EVIDENCE_HASH_FILE_BYTES,
        MAX_EVIDENCE_HASH_TOTAL_BYTES,
    )
}

fn hash_directory_tree_with_limits(
    path: &Path,
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
        root: &Path,
        directory: &Path,
        depth: usize,
        state: &mut DirectoryHashState<'_>,
    ) -> Result<bool, String> {
        let mut entries = fs::read_dir(directory)
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        entries.sort_by_key(|entry| entry.file_name());

        for entry in entries {
            if state.files_seen >= state.max_files {
                state.digest.update(b"file-limit-reached");
                return Ok(true);
            }
            let path = entry.path();
            let relative = path.strip_prefix(root).unwrap_or(&path);
            let relative = relative.to_string_lossy();
            let file_type = entry.file_type().map_err(|error| error.to_string())?;
            if file_type.is_symlink() {
                state.digest.update(relative.as_bytes());
                state.digest.update(b"s");
                continue;
            }
            if file_type.is_dir() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
                    continue;
                }
                state.digest.update(relative.as_bytes());
                state.digest.update(b"d");
                if depth < state.max_depth {
                    if visit(root, &path, depth + 1, state)? {
                        return Ok(true);
                    }
                } else {
                    state.digest.update(b"depth-limit-reached");
                    return Ok(true);
                }
                continue;
            }
            if !file_type.is_file() {
                continue;
            }

            state.files_seen += 1;
            state.digest.update(relative.as_bytes());
            state.digest.update(b"f");
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(_) => {
                    state.digest.update(b"metadata-error");
                    return Ok(true);
                }
            };
            hash_file_metadata(&metadata, state.digest);
            match hash_file_content_with_limit(
                &path,
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

fn hash_file_content(
    path: &Path,
    file_len: u64,
    total_bytes_remaining: &mut u64,
    digest: &mut Sha256,
) -> Result<bool, String> {
    hash_file_content_with_limit(
        path,
        file_len,
        total_bytes_remaining,
        MAX_EVIDENCE_HASH_FILE_BYTES,
        digest,
    )
}

fn hash_file_content_with_limit(
    path: &Path,
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

    let mut file = fs::File::open(path).map_err(|error| error.to_string())?;
    if file_len <= budget {
        hash_reader_bytes(&mut file, budget, digest)?;
    } else {
        let prefix_bytes = budget.div_ceil(2);
        let suffix_bytes = budget / 2;
        digest.update(b"sampled-prefix");
        hash_reader_bytes(&mut file, prefix_bytes, digest)?;
        if suffix_bytes > 0 {
            let suffix_offset = i64::try_from(suffix_bytes)
                .map_err(|_| "evidence sample offset is too large".to_string())?;
            file.seek(SeekFrom::End(-suffix_offset))
                .map_err(|error| error.to_string())?;
            digest.update(b"sampled-suffix");
            hash_reader_bytes(&mut file, suffix_bytes, digest)?;
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

    fn ready_plan_json(extra: &str) -> String {
        format!(
            r#"{{
                "state":"ready",
                "title":"Implement the change",
                "goal":"Ship a verified implementation",
                "steps":[{{"id":"inspect","title":"Inspect the code"}}]
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
