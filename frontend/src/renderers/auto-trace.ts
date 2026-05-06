import { invalidateChatScrollCache, scrollDown } from '../scroll.js';
import { dom, state } from '../state.js';
import type { AutoTraceEvent } from '../types.js';
import { escHtml, hideWelcome } from '../utils.js';

function flag(value: boolean): string {
  return value ? 'yes' : 'no';
}

function reasonList(values: string[]): string {
  return values.length > 0 ? values.join(', ') : 'none';
}

function signalSummary(trace: AutoTraceEvent): string {
  const { signals } = trace;
  return [
    `intent=${signals.intent}`,
    `chars=${signals.user_msg_chars}`,
    `obs=${signals.observation_strength}`,
    `results=${signals.tool_results_count}`,
    `tool_errors=${signals.tool_error_count}`,
    `summaries=${signals.summary_count}`,
    `bytes=${signals.summary_bytes}`,
    `stagnation=${signals.stagnation_streak}`,
    `errors=${signals.error_streak}`,
    `pressure=${signals.task_pressure}`,
    `ready=${flag(signals.ready_to_finish)}`,
    `action=${flag(signals.action_oriented)}`,
    `blocked=${flag(signals.has_blocking_uncertainty)}`,
    `finish_deferrals=${signals.finish_deferral_count}`,
    `progress=${flag(signals.progress_made)}`,
    `retry=${signals.retry_pattern}`,
    `error_kind=${signals.error_kind}`,
    `evidence=${signals.evidence_delta_quality}`,
  ].join(' ');
}

function ensureAutoDebugRow(): HTMLElement | null {
  if (!dom.chat) return null;
  if (!state.autoDebugRow) {
    const row = document.createElement('div');
    row.className = 'msg-row system auto-debug-row';
    state.autoDebugRow = row;
  }
  if (!state.autoDebugRow.isConnected) {
    dom.chat.appendChild(state.autoDebugRow);
    invalidateChatScrollCache();
    hideWelcome();
  }
  return state.autoDebugRow;
}

function renderAutoDebugPanel(): void {
  if (!state.autoDebugEnabled || !state.latestAutoTrace) {
    clearAutoTracePanel();
    return;
  }

  const row = ensureAutoDebugRow();
  if (!row) return;
  const trace = state.latestAutoTrace;
  row.innerHTML = `
    <div class="system-card auto-debug-card" data-auto-trace-panel="true">
      <div class="auto-debug-header">
        <span class="auto-debug-tag">Auto Debug</span>
        <span class="auto-debug-meta">round ${trace.round} · cycle ${trace.cycle} · ${escHtml(trace.phase)}</span>
        <span class="auto-debug-meta">${escHtml(trace.provider)} · ${escHtml(trace.model)}</span>
      </div>
      <div class="auto-debug-line">
        selected=<strong>${escHtml(trace.selected_think)}</strong>
        baseline=${escHtml(trace.baseline_level)}
        reason=${escHtml(trace.baseline_reason)}
      </div>
      <div class="auto-debug-line auto-debug-list">
        escalators=${escHtml(reasonList(trace.escalators))}
        dampeners=${escHtml(reasonList(trace.dampeners))}
        clamps=${escHtml(reasonList(trace.clamps))}
      </div>
      <pre class="auto-debug-signals">${escHtml(signalSummary(trace))}</pre>
    </div>
  `;
  scrollDown();
}

export function updateAutoDebugToggleButton(): void {
  if (!dom.toggleAutoDebugBtn) return;
  dom.toggleAutoDebugBtn.textContent = `Auto Debug: ${state.autoDebugEnabled ? 'On' : 'Off'}`;
  dom.toggleAutoDebugBtn.classList.toggle('is-active', state.autoDebugEnabled);
}

export function clearAutoTracePanel(): void {
  if (!state.autoDebugRow) return;
  state.autoDebugRow.remove();
  state.autoDebugRow = null;
  invalidateChatScrollCache();
}

export function clearActiveAutoTrace(): void {
  state.latestAutoTrace = null;
  clearAutoTracePanel();
}

export function applyAutoTrace(trace: AutoTraceEvent): void {
  state.latestAutoTrace = trace;
  if (state.autoDebugEnabled) {
    renderAutoDebugPanel();
  }
}

export function applyTopLevelAutoTrace(trace: AutoTraceEvent & { subagent?: string | null }): void {
  if (trace.subagent) {
    return;
  }
  applyAutoTrace(trace);
}

export function setAutoDebugEnabled(enabled: boolean): void {
  state.autoDebugEnabled = enabled;
  updateAutoDebugToggleButton();
  if (enabled) {
    renderAutoDebugPanel();
  } else {
    clearAutoTracePanel();
  }
}

export function toggleAutoDebug(): void {
  setAutoDebugEnabled(!state.autoDebugEnabled);
}
