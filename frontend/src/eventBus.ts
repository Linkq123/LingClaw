// Lightweight typed event bus for bridging imperative WS events to React components.
// Used by streaming panels (sub-agent, orchestrate) to decouple main.ts from React state.

type Listener<T> = (data: T) => void;

class EventBus<EventMap extends object> {
  private listeners: { [K in keyof EventMap]?: Array<Listener<EventMap[K]>> } = {};

  on<K extends keyof EventMap>(event: K, listener: Listener<EventMap[K]>): () => void {
    if (!this.listeners[event]) {
      this.listeners[event] = [];
    }
    this.listeners[event]!.push(listener);
    return () => this.off(event, listener);
  }

  off<K extends keyof EventMap>(event: K, listener: Listener<EventMap[K]>): void {
    const arr = this.listeners[event];
    if (!arr) return;
    const idx = arr.indexOf(listener);
    if (idx >= 0) arr.splice(idx, 1);
  }

  emit<K extends keyof EventMap>(event: K, data: EventMap[K]): void {
    const arr = this.listeners[event];
    if (!arr) return;
    for (const fn of arr.slice()) fn(data);
  }
}

// ── Event shapes ──────────────────────────────────────────────────────────────

export interface SubagentEvents {
  task_started: { taskId: string; agentName: string; prompt: string };
  task_progress: { taskId: string; cycle: number; phase: string };
  task_tool: { taskId: string; toolCallId: string; name: string; input: string };
  task_tool_result: {
    taskId: string;
    toolCallId: string;
    content: string;
    isError: boolean;
    durationMs?: number;
  };
  task_thinking_start: { taskId: string };
  task_thinking_delta: { taskId: string; delta: string };
  task_thinking_done: { taskId: string; text: string };
  task_completed: { taskId: string; result: string; cycles: number; durationMs?: number };
  task_failed: { taskId: string; error: string };
}

export interface OrchestrateEvents {
  orchestrate_started: { orchestrateId: string; goal: string };
  orchestrate_layer: { orchestrateId: string; layer: number; taskIds: string[] };
  orchestrate_task_started: {
    orchestrateId: string;
    taskId: string;
    agent: string;
    prompt: string;
  };
  orchestrate_task_completed: { orchestrateId: string; taskId: string; result: string };
  orchestrate_task_failed: { orchestrateId: string; taskId: string; error: string };
  orchestrate_task_skipped: { orchestrateId: string; taskId: string; reason: string };
  orchestrate_completed: { orchestrateId: string; summary: string; durationMs?: number };
}

// ── Exported buses ────────────────────────────────────────────────────────────

export const subagentBus = new EventBus<SubagentEvents>();
export const orchestrateBus = new EventBus<OrchestrateEvents>();
