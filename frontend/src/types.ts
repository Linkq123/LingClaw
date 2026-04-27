// ── Shared types ──

export interface ImageAttachment {
  url: string;
  object_key?: string;
  attachment_token?: string;
}

export interface HistoryMessage {
  role: 'user' | 'assistant' | 'tool_call' | 'tool_result';
  content: string;
  images?: ImageAttachment[];
  id?: string;
  timestamp?: number;
  name?: string;
  arguments?: string;
  result?: string;
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

export interface SessionEvent {
  type: 'session';
  id: string;
  name?: string;
  capabilities?: { image?: boolean; s3?: boolean };
  show_tools?: boolean;
  show_reasoning?: boolean;
}

export interface HistoryEvent {
  type: 'history';
  messages?: HistoryMessage[];
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

export interface ToolResultEvent {
  type: 'tool_result';
  name: string;
  result?: string;
  id: string;
  duration_ms?: number;
  is_error?: boolean;
  subagent?: string;
  task_id?: string;
}

export interface TaskEvent {
  type: 'task_started' | 'task_progress' | 'task_tool' | 'task_completed' | 'task_failed';
  agent: string;
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
}

export interface ReactPhaseEvent {
  type: 'react_phase';
  phase: ReactPhase;
  cycle: number;
}

export interface StartEvent {
  type: 'start';
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
}
export interface ThinkingDeltaEvent {
  type: 'thinking_delta';
  content: string;
}
export interface ThinkingDoneEvent {
  type: 'thinking_done';
}
export interface ContextCompressedEvent {
  type: 'context_compressed';
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
  plan: { id: string; agent: string; prompt: string; depends_on?: string[] }[];
}

export interface OrchestrateLayerEvent {
  type: 'orchestrate_layer';
  orchestrate_id: string;
  layer: number;
  task_ids: string[];
}

export interface OrchestrateTaskEvent {
  type:
    | 'orchestrate_task_started'
    | 'orchestrate_task_completed'
    | 'orchestrate_task_failed'
    | 'orchestrate_task_skipped';
  orchestrate_id: string;
  task_id: string;
  agent?: string;
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
}

export type WebSocketMessage =
  | SessionEvent
  | HistoryEvent
  | DeltaEvent
  | ToolCallEvent
  | ToolProgressEvent
  | ToolResultEvent
  | TaskEvent
  | SystemEvent
  | ReactPhaseEvent
  | StartEvent
  | DoneEvent
  | ViewStateEvent
  | ThinkingStartEvent
  | ThinkingDeltaEvent
  | ThinkingDoneEvent
  | ContextCompressedEvent
  | UsageEvent
  | OrchestrateStartedEvent
  | OrchestrateLayerEvent
  | OrchestrateTaskEvent
  | OrchestrateCompletedEvent;
