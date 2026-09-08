import { dom, state } from '../state.js';
import type { AutoTraceEvent } from '../types.js';
import { escHtml } from '../utils.js';
import { tr } from '../i18n.js';
import { iconMarkup } from '../icons.js';

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
    `ready_signal=${flag(signals.ready_to_finish)}`,
    `action=${flag(signals.action_oriented)}`,
    `blocked_signal=${flag(signals.has_blocking_uncertainty)}`,
    `progress=${flag(signals.progress_made)}`,
    `retry=${signals.retry_pattern}`,
    `error_kind=${signals.error_kind}`,
    `evidence=${signals.evidence_delta_quality}`,
  ].join(' ');
}

function compressionSummary(trace: AutoTraceEvent): string | null {
  const compression = trace.compression;
  if (!compression) return null;
  if (compression.outcome === 'compressed') {
    return `compression=compressed saved_tokens=${compression.saved_tokens ?? 0} saved_percent=${compression.saved_percent ?? 0}`;
  }
  if (compression.outcome === 'skipped') {
    return `compression=skipped reason=${compression.reason ?? 'unknown'}`;
  }
  return `compression=failed reason=${compression.reason ?? 'unknown'}`;
}

function ensureAutoDebugRow(): HTMLElement | null {
  if (!dom.autoDebugHost) return null;
  if (!state.autoDebugRow) {
    const panel = document.createElement('div');
    panel.className = 'auto-debug-panel';
    state.autoDebugRow = panel;
  }
  if (!state.autoDebugRow.isConnected) {
    dom.autoDebugHost.appendChild(state.autoDebugRow);
  }
  dom.autoDebugHost.hidden = false;
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
    <div class="auto-debug-card" data-auto-trace-panel="true">
      <div class="auto-debug-header">
        <span class="auto-debug-tag">Auto Debug</span>
        <span class="auto-debug-meta">round ${trace.round} · cycle ${trace.cycle} · ${escHtml(trace.phase)}</span>
        <span class="auto-debug-meta">${escHtml(trace.provider)} · ${escHtml(trace.model)}</span>
        <button type="button" class="auto-debug-close" data-action="close-auto-debug" aria-label="${escHtml(tr('common.close'))}" title="${escHtml(tr('common.close'))}">
          ${iconMarkup('close')}
        </button>
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
      ${compressionSummary(trace) ? `<div class="auto-debug-line auto-debug-list">${escHtml(compressionSummary(trace)!)}</div>` : ''}
      <pre class="auto-debug-signals">${escHtml(signalSummary(trace))}</pre>
    </div>
  `;
}

export function updateAutoDebugToggleButton(): void {
  if (!dom.toggleAutoDebugBtn) return;
  const label = dom.toggleAutoDebugBtn.querySelector('.control-label');
  if (label) {
    label.textContent = tr('common.autoDebug');
  } else {
    dom.toggleAutoDebugBtn.textContent = `${tr('common.autoDebug')}: ${
      state.autoDebugEnabled ? tr('common.on') : tr('common.off')
    }`;
  }
  dom.toggleAutoDebugBtn.classList.toggle('is-active', state.autoDebugEnabled);
  dom.toggleAutoDebugBtn.setAttribute('aria-pressed', String(state.autoDebugEnabled));
}

export function clearAutoTracePanel(): void {
  if (!state.autoDebugRow) return;
  state.autoDebugRow.remove();
  state.autoDebugRow = null;
  if (dom.autoDebugHost) dom.autoDebugHost.hidden = true;
}

export function clearActiveAutoTrace(): void {
  state.latestAutoTrace = null;
  clearAutoTracePanel();
}

export function clearCompressionOutcome(): void {
  state.latestCompression = null;
}

export function clearCompressionOutcomeForNewAnalyzeCycle(nextCycle: number): void {
  const currentCycle = state.latestAutoTrace?.cycle ?? state.reactStatusCycle;
  if (currentCycle != null && nextCycle > currentCycle) {
    clearCompressionOutcome();
  }
}

export function clearCompressionOutcomeForNewRound(nextCycle: number | null | undefined): void {
  const currentCycle = state.latestAutoTrace?.cycle ?? state.reactStatusCycle;
  if (currentCycle != null && nextCycle != null && nextCycle > currentCycle) {
    clearCompressionOutcome();
  }
}

export function applyCompressionOutcome(
  compression: NonNullable<AutoTraceEvent['compression']>,
): void {
  state.latestCompression = compression;
  if (state.latestAutoTrace) {
    state.latestAutoTrace = {
      ...state.latestAutoTrace,
      compression,
    };
  }
  if (state.autoDebugEnabled && state.latestAutoTrace) {
    renderAutoDebugPanel();
  }
}

export function applyAutoTrace(trace: AutoTraceEvent): void {
  const previousCycle = state.latestAutoTrace?.cycle;
  const compression =
    previousCycle != null && trace.cycle > previousCycle
      ? trace.compression
      : (trace.compression ?? state.latestCompression ?? undefined);
  if (previousCycle != null && trace.cycle > previousCycle && !trace.compression) {
    clearCompressionOutcome();
  }
  state.latestAutoTrace = {
    ...trace,
    compression,
  };
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
