// ── Shared types ──

export interface ImageAttachment {
  url: string;
  name?: string;
  mime_type?: string;
  object_key?: string;
  attachment_token?: string;
  s3_config_id?: string;
}

export interface HistoryMessage {
  role: 'user' | 'assistant' | 'tool_call' | 'tool_result';
  content: string;
  message_index?: number;
  images?: ImageAttachment[];
  id?: string;
  timestamp?: number;
  name?: string;
  arguments?: string;
  result?: string;
  is_error?: boolean;
  duration_ms?: number;
  thinking?: string;
  subagent_snapshot?: SubagentHistorySnapshot;
}

export interface SubagentToolHistorySnapshot {
  id: string;
  name: string;
  arguments?: string;
  result?: string;
  is_error?: boolean;
  duration_ms?: number;
  images?: ImageAttachment[];
}

export interface SubagentHistorySnapshot {
  reasoning?: string;
  tools?: SubagentToolHistorySnapshot[];
  cycles?: number;
  tool_calls?: number;
  duration_ms?: number;
  input_tokens?: number;
  output_tokens?: number;
  success?: boolean;
  result_excerpt?: string;
  error?: string;
}

export type ReactPhase = 'analyze' | 'act' | 'observe' | 'finish' | '';

// ── WebSocket event types ──

export interface SessionSummary {
  id: string;
  name: string;
  updated_at?: number;
  corrupt?: boolean;
}

export interface SessionGroupSummary {
  id: string;
  name: string;
  members?: number;
  messages?: number;
  running?: number;
  updated_at?: number;
  corrupt?: boolean;
}

export interface GroupMemberDetail {
  id: string;
  name: string;
  role: 'owner' | 'admin' | 'member';
}

export interface GroupVote {
  id: string;
  action: string;
  target_session_id: string;
  requester_session_id: string;
  approvals: string[];
  threshold: number;
  created_at: number;
  updated_at: number;
}

export interface SessionGroupDetail {
  id: string;
  name: string;
  members: string[];
  admins?: string[];
  pending_votes?: GroupVote[];
  member_details?: GroupMemberDetail[];
  model_override_members?: string[];
  model_configured_members?: string[];
  model_member_ids?: string[];
  explicitPrimaryModelConfigured?: boolean;
  configRevision?: number;
  capabilities?: { s3?: boolean; s3_config_id?: string | null };
  messages?: unknown[];
  runs?: unknown[];
  created_at?: number;
  updated_at?: number;
  version?: number;
}

export type TodoStatus = 'pending' | 'in_progress' | 'completed';

export interface TodoItem {
  id: string;
  content: string;
  status: TodoStatus;
}

export interface TodosStateEvent {
  type: 'todos_state';
  revision: number;
  items: TodoItem[];
  last_updated_by: 'user' | 'assistant';
  updated_at: number;
}

export interface TodosUpdateResponse {
  ok: boolean;
  conflict: boolean;
  revision: number;
  items: TodoItem[];
  last_updated_by: 'user' | 'assistant';
  updated_at: number;
}

export interface SessionEvent {
  type: 'session';
  id: string;
  name?: string;
  capabilities?: { image?: boolean; s3?: boolean; s3_config_id?: string | null };
  explicitPrimaryModelConfigured?: boolean;
  modelOverridePresent?: boolean;
  modelOverrideConfigured?: boolean;
  effectiveModelConfigured?: boolean;
  configRevision?: number;
  usage?: {
    daily_input?: number;
    daily_output?: number;
    total_input?: number;
    total_output?: number;
  };
  show_tools?: boolean;
  show_reasoning?: boolean;
}

export interface HistoryEvent {
  type: 'history';
  messages?: HistoryMessage[];
  plans?: PlanStatePayload[];
  pending_plan?: PlanReadyPayload;
}

export interface DeltaEvent {
  type: 'delta';
  content: string;
}

export interface ToolCallEvent {
  type: 'tool_call';
  name: string;
  arguments: string;
  id: string;
}

export interface ToolProgressEvent {
  type: 'tool_progress';
  id: string;
  name?: string;
  elapsed_ms?: number;
}

export interface ToolOutputEvent {
  type: 'tool_output';
  id: string;
  name?: string;
  stream?: 'stdout' | 'stderr';
  chunk: string;
  subagent?: string;
  task_id?: string;
}

export interface ToolResultEvent {
  type: 'tool_result';
  name: string;
  result?: string;
  id: string;
  duration_ms?: number;
  is_error?: boolean;
  subagent?: string;
  task_id?: string;
  images?: ImageAttachment[];
}

export interface ToolImageCompatibilityWarningEvent {
  type: 'tool_image_compatibility_warning';
  provider: 'openai_chat';
}

export interface TaskEvent {
  type: 'task_started' | 'task_progress' | 'task_tool' | 'task_completed' | 'task_failed';
  agent: string;
  id?: string;
  task_id?: string;
  prompt?: string;
  cycle?: number;
  tool?: string;
  arguments?: string;
  cycles?: number;
  tool_calls?: number;
  duration_ms?: number;
  input_tokens?: number;
  output_tokens?: number;
  error?: string;
  result_preview?: string;
  result_excerpt?: string;
}

export interface SystemEvent {
  type: 'system' | 'success' | 'error' | 'progress';
  content: string;
  code?: string;
}

export interface ReactPhaseEvent {
  type: 'react_phase';
  phase: ReactPhase;
  cycle: number;
}

export interface StartEvent {
  type: 'start';
  run_mode?: 'execute' | 'plan_only';
}
export interface DoneEvent {
  type: 'done';
}
export interface ViewStateEvent {
  type: 'view_state';
  show_tools?: boolean;
  show_reasoning?: boolean;
}
export interface ThinkingStartEvent {
  type: 'thinking_start';
  subagent?: string;
  task_id?: string;
}
export interface ThinkingDeltaEvent {
  type: 'thinking_delta';
  content: string;
  subagent?: string;
  task_id?: string;
}
export interface ThinkingDoneEvent {
  type: 'thinking_done';
  subagent?: string;
  task_id?: string;
}
export interface ContextCompressedEvent {
  type: 'context_compressed';
  saved_tokens?: number;
  saved_percent?: number;
}

export interface ContextPrunedEvent {
  type: 'context_pruned';
  messages_removed?: number;
}

export interface ContextCompressSkippedEvent {
  type: 'context_compress_skipped';
  reason?: string;
}

export interface ContextCompressFailedEvent {
  type: 'context_compress_failed';
  error?: string;
}

export interface CompressionOutcome {
  outcome: 'compressed' | 'skipped' | 'failed';
  saved_tokens?: number;
  saved_percent?: number;
  reason?: string;
}

export interface AutoTraceSignals {
  intent: string;
  user_msg_chars: number;
  observation_strength: string;
  tool_results_count: number;
  tool_error_count: number;
  summary_count: number;
  summary_bytes: number;
  stagnation_streak: number;
  error_streak: number;
  task_pressure: number;
  ready_to_finish: boolean;
  action_oriented: boolean;
  has_blocking_uncertainty: boolean;
  progress_made: boolean;
  retry_pattern: string;
  error_kind: string;
  evidence_delta_quality: string;
}

export interface AutoTraceEvent {
  type: 'auto_trace';
  round: number;
  cycle: number;
  phase: string;
  model: string;
  provider: string;
  selected_think: string;
  baseline_level: string;
  baseline_reason: string;
  escalators: string[];
  dampeners: string[];
  clamps: string[];
  signals: AutoTraceSignals;
  compression?: CompressionOutcome;
}

export interface TaskPlanStep {
  id: string;
  title: string;
  status: string;
}

export interface TaskPlanToolSuggestion {
  name: string;
  reason: string;
  score?: number;
  source?: string;
}

export interface TaskPlanAgentSuggestion {
  name: string;
  reason: string;
  score?: number;
}

export interface TaskPlanVerificationSuggestion {
  command: string;
  reason: string;
  confidence: string;
  when: string;
}

export interface TaskPlanPayload {
  goal: string;
  intent: string;
  steps: TaskPlanStep[];
  openQuestions?: string[];
  suggestedTools?: TaskPlanToolSuggestion[];
  suggestedAgents?: TaskPlanAgentSuggestion[];
  verificationSuggestions?: TaskPlanVerificationSuggestion[];
  acceptanceCriteria?: string[];
  status: string;
}

export interface TaskPlanEvent {
  type: 'task_plan';
  round: number;
  cycle: number;
  plan: TaskPlanPayload;
}

export interface PlanReadyPayload {
  plan_id: string;
  revision?: number;
  message_index: number;
  created_at: number;
}

export interface PlanReadyEvent extends PlanReadyPayload {
  type: 'plan_ready';
}

export interface PlanStateEvent {
  type: 'plan_state';
  plan: PlanStatePayload;
}

export interface PlanStaleEvent {
  type: 'plan_stale';
  code: 'plan_stale';
  plan_id: string;
  revision: number;
  paths: string[];
  confirmation_token: string;
}

export type PlanStatus =
  | 'planning'
  | 'needs_input'
  | 'ready'
  | 'executing'
  | 'completed'
  | 'failed'
  | 'stopped'
  | 'discarded';

export type PlanStepStatus = 'pending' | 'in_progress' | 'completed' | 'blocked' | 'skipped';

export interface PlanQuestionOption {
  id: string;
  label: string;
  description?: string;
}

export interface PlanQuestion {
  id: string;
  prompt: string;
  options?: PlanQuestionOption[];
}

export interface PlanArtifact {
  schema_version?: number;
  title: string;
  goal: string;
  summary?: string;
  steps?: Array<{
    id: string;
    title: string;
    description?: string;
    affected_areas?: string[];
  }>;
  assumptions?: string[];
  risks?: string[];
  verification?: string[];
  acceptance_criteria?: string[];
  questions?: PlanQuestion[];
  legacy_markdown?: string;
}

export interface PlanProgressStep {
  id: string;
  title: string;
  status: PlanStepStatus;
  note?: string;
  deviation_reason?: string;
}

export interface PlanStatePayload {
  plan_id: string;
  revision: number;
  status: PlanStatus;
  message_index: number;
  created_at: number;
  updated_at: number;
  approved_at?: number | null;
  finished_at?: number | null;
  execution_attempt?: number;
  artifact: PlanArtifact;
  progress: PlanProgressStep[];
  evidence_count?: number;
  evidence_truncated?: boolean;
  stale_override_paths?: string[];
  stale_override_confirmed_at?: number | null;
  pending_feedback?: string | null;
  initial_submission_pending?: boolean;
  initial_request_image_only?: boolean;
  unfinished_steps?: number;
  run_finished_with_unreported_steps?: boolean;
  historical?: boolean;
}

export interface UsageEvent {
  type: 'usage';
  daily_input_tokens?: number;
  daily_output_tokens?: number;
  total_input_tokens?: number;
  total_output_tokens?: number;
}

export interface OrchestrateStartedEvent {
  type: 'orchestrate_started';
  orchestrate_id: string;
  task_count?: number;
  layer_count?: number;
  tasks: {
    id: string;
    agent: string;
    depends_on?: string[];
    prompt_preview?: string;
  }[];
}

export interface OrchestrateLayerEvent {
  type: 'orchestrate_layer';
  orchestrate_id: string;
  layer: number;
  total_layers?: number;
  tasks: string[];
}

export interface OrchestrateTaskEvent {
  type:
    | 'orchestrate_task_started'
    | 'orchestrate_task_completed'
    | 'orchestrate_task_failed'
    | 'orchestrate_task_skipped';
  orchestrate_id: string;
  id: string;
  agent?: string;
  prompt?: string;
  reason?: string;
  result_preview?: string;
  result_excerpt?: string;
  error?: string;
  cycles?: number;
  tool_calls?: number;
  duration_ms?: number;
  input_tokens?: number;
  output_tokens?: number;
}

export interface OrchestrateCompletedEvent {
  type: 'orchestrate_completed';
  orchestrate_id: string;
  completed?: number;
  failed?: number;
  skipped?: number;
  total_tasks?: number;
  input_tokens?: number;
  output_tokens?: number;
  duration_ms?: number;
  aborted?: boolean;
}

export type WebSocketMessage =
  | SessionEvent
  | TodosStateEvent
  | HistoryEvent
  | DeltaEvent
  | ToolCallEvent
  | ToolProgressEvent
  | ToolOutputEvent
  | ToolResultEvent
  | ToolImageCompatibilityWarningEvent
  | PlanReadyEvent
  | PlanStateEvent
  | PlanStaleEvent
  | TaskEvent
  | SystemEvent
  | ReactPhaseEvent
  | StartEvent
  | DoneEvent
  | ViewStateEvent
  | ThinkingStartEvent
  | ThinkingDeltaEvent
  | ThinkingDoneEvent
  | AutoTraceEvent
  | TaskPlanEvent
  | ContextCompressedEvent
  | ContextPrunedEvent
  | ContextCompressSkippedEvent
  | ContextCompressFailedEvent
  | UsageEvent
  | OrchestrateStartedEvent
  | OrchestrateLayerEvent
  | OrchestrateTaskEvent
  | OrchestrateCompletedEvent;
