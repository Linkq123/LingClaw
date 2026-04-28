import { dom, state } from '../state.js';
import {
  escHtml,
  formatToolDuration,
  formatTokenCount,
  inlinePreview,
  stripDelegatedPromptRuntimeContext,
  pulseFocus,
} from '../utils.js';
import { scrollDown } from '../scroll.js';
import {
  wrapInTimeline,
  animatePanelIn,
  animateCollapsibleSection,
  removeTimelinePanel,
} from './timeline.js';
import { pinReactStatusToBottom } from './react-status.js';
import {
  closeSubagentModal,
  createDetachedSubagentPanel,
  finishSubagentPanel,
  openSubagentPanelModal,
  updateSubagentPrompt,
} from './subagent.js';

type TaskStatus = 'pending' | 'running' | 'completed' | 'failed' | 'skipped';

function ensureRegistry() {
  return state.activeOrchestrations;
}

function escapeAttr(value: string) {
  return typeof CSS !== 'undefined' && typeof CSS.escape === 'function'
    ? CSS.escape(value)
    : value.replace(/"/g, '\\"');
}

function getTaskRows(panel: HTMLElement | null): HTMLElement[] {
  if (!panel) return [];

  const orchestrateId = panel.dataset.orchestrateId || '';
  if (orchestrateId && state.activeOrchestrations.has(orchestrateId)) {
    return Array.from(state.activeOrchestrations.get(orchestrateId)?.taskRows.values() || []);
  }

  if (orchestrateId) {
    return Array.from(
      document.querySelectorAll(
        `.orchestrate-task[data-orchestrate-id="${escapeAttr(orchestrateId)}"]`,
      ),
    ) as HTMLElement[];
  }

  return Array.from(panel.querySelectorAll('.orchestrate-task')) as HTMLElement[];
}

function summarizeTaskCounts(rows: HTMLElement[]) {
  const total = rows.length;
  const completed = rows.filter((row) =>
    row.classList.contains('orchestrate-task-completed'),
  ).length;
  const failed = rows.filter((row) => row.classList.contains('orchestrate-task-failed')).length;
  const running = rows.filter((row) => row.classList.contains('orchestrate-task-running')).length;
  const skipped = rows.filter((row) => row.classList.contains('orchestrate-task-skipped')).length;
  const pending = Math.max(0, total - completed - failed - running - skipped);

  return { total, completed, failed, running, skipped, pending };
}

function syncTaskHighlights(panel: HTMLElement | null) {
  if (!panel) return;

  const rows = getTaskRows(panel);
  const hasRunning = rows.some((row) => row.classList.contains('orchestrate-task-running'));
  rows.forEach((row) => {
    row.classList.toggle(
      'orchestrate-task-current',
      hasRunning && row.classList.contains('orchestrate-task-running'),
    );
  });
}

function syncProgressVisuals(entry) {
  if (!entry?.panel) return;

  const panel = entry.panel;
  const counts = summarizeTaskCounts(getTaskRows(panel));
  const total = Math.max(1, counts.total);
  const completionPercent =
    counts.total > 0 ? Math.round((counts.completed / counts.total) * 100) : 0;
  const progressLabel = panel.querySelector(
    '[data-orchestrate-progress-label]',
  ) as HTMLElement | null;

  if (progressLabel) {
    const parts = [`${counts.completed}/${counts.total} completed`, `${completionPercent}%`];
    if (counts.running) parts.push(`${counts.running} running`);
    if (counts.failed) parts.push(`${counts.failed} failed`);
    if (counts.skipped) parts.push(`${counts.skipped} skipped`);
    if (counts.pending) parts.push(`${counts.pending} pending`);
    progressLabel.textContent = parts.join(' / ');
  }

  const segmentCounts = {
    completed: counts.completed,
    running: counts.running,
    failed: counts.failed,
    skipped: counts.skipped,
    pending: counts.pending,
  };

  Object.entries(segmentCounts).forEach(([key, value]) => {
    const segment = panel.querySelector(
      `[data-orchestrate-progress="${key}"]`,
    ) as HTMLElement | null;
    if (!segment) return;

    segment.style.width = `${(value / total) * 100}%`;
    segment.hidden = value === 0;

    const segmentLabel =
      {
        completed: 'Completed',
        running: 'Running',
        failed: 'Failed',
        skipped: 'Skipped',
        pending: 'Pending',
      }[key] || key;
    segment.title = `${segmentLabel}: ${value}`;
  });

  syncTaskHighlights(panel);
}

function updateHeaderProgress(entry) {
  syncProgressVisuals(entry);

  const rows = Array.from(entry.taskRows.values()) as HTMLElement[];
  const { total, completed, failed, running, skipped, pending } = summarizeTaskCounts(rows);
  const statusEl = entry.panel.querySelector('.orchestrate-status') as HTMLElement | null;
  if (!statusEl || !entry.panel.classList.contains('orchestrate-active')) return;

  const parts = [`${completed}/${total}`];
  if (running) parts.push(`${running} running`);
  if (failed) parts.push(`${failed} failed`);
  if (skipped) parts.push(`${skipped} skipped`);
  if (!running && pending && completed < total) parts.push(`${pending} pending`);
  statusEl.textContent = parts.join(' / ');
}

function statusText(status: TaskStatus) {
  switch (status) {
    case 'running':
      return 'Running';
    case 'completed':
      return 'Completed';
    case 'failed':
      return 'Failed';
    case 'skipped':
      return 'Skipped';
    case 'pending':
    default:
      return 'Pending';
  }
}

function statusIcon(status: TaskStatus) {
  switch (status) {
    case 'running':
      return '\u25cf';
    case 'completed':
      return '\u2713';
    case 'failed':
      return '\u2715';
    case 'skipped':
      return '\u21b7';
    case 'pending':
    default:
      return '\u2026';
  }
}

function compositeTaskId(orchestrateId: string, taskId: string) {
  return `${orchestrateId}:${taskId}`;
}

function setTaskPreview(row: HTMLElement, text: string) {
  const previewEl = row.querySelector('.orchestrate-task-preview') as HTMLElement | null;
  if (!previewEl) return;
  previewEl.textContent = inlinePreview(text || 'Waiting to run');
}

function setTaskStatusTone(row: HTMLElement, status: TaskStatus) {
  row.classList.toggle('orchestrate-task-has-error', status === 'failed');
  row.classList.toggle('orchestrate-task-has-result', status === 'completed');
}

function buildDagLayout(layersContainer: HTMLElement, tasks, orchestrateId: string) {
  const layer = new Map<string, number>();
  const deps = new Map<string, string[]>();

  for (const task of tasks) {
    deps.set(task.id, task.depends_on || []);
  }

  let changed = true;
  while (changed) {
    changed = false;
    for (const task of tasks) {
      if (layer.has(task.id)) continue;
      const depLayers = (deps.get(task.id) || []).map((depId) => layer.get(depId));
      if (depLayers.some((depLayer) => depLayer === undefined)) continue;
      const maxLayer = depLayers.length ? Math.max(...depLayers) : -1;
      layer.set(task.id, maxLayer + 1);
      changed = true;
    }
  }

  for (const task of tasks) {
    if (!layer.has(task.id)) layer.set(task.id, 0);
  }

  const layerCount = Math.max(0, ...Array.from(layer.values())) + 1;
  const buckets = Array.from({ length: layerCount }, () => [] as typeof tasks);
  for (const task of tasks) {
    buckets[layer.get(task.id) || 0].push(task);
  }

  const taskRows = new Map<string, HTMLElement>();
  const taskPanels = new Map<string, HTMLElement>();
  const taskLayer = new Map<string, number>();

  for (let layerIndex = 0; layerIndex < buckets.length; layerIndex += 1) {
    const layerEl = document.createElement('div');
    layerEl.className = 'orchestrate-layer';
    layerEl.dataset.layerIndex = String(layerIndex);

    const header = document.createElement('div');
    header.className = 'orchestrate-layer-header';
    header.textContent = `Layer ${layerIndex + 1}${
      buckets[layerIndex].length > 1 ? ' (parallel)' : ''
    }`;
    layerEl.appendChild(header);

    const taskContainer = document.createElement('div');
    taskContainer.className = 'orchestrate-task-grid';

    for (const task of buckets[layerIndex]) {
      const displayPrompt = stripDelegatedPromptRuntimeContext(task.prompt_preview || '');
      const row = document.createElement('div');
      row.className = 'orchestrate-task orchestrate-task-pending';
      row.dataset.orchestrateId = orchestrateId;
      row.dataset.taskId = task.id;
      if (displayPrompt) row.dataset.promptPreview = displayPrompt;
      row.innerHTML = `
        <div
          class="orchestrate-task-summary"
          data-action="open-orchestrate-task-modal"
          role="button"
          tabindex="0"
          aria-expanded="false"
          aria-haspopup="dialog"
        >
          <span class="orchestrate-task-icon">${statusIcon('pending')}</span>
          <span class="orchestrate-task-main">
            <span class="orchestrate-task-title">
              <span class="orchestrate-task-id">${escHtml(task.id)}</span>
              <span class="orchestrate-task-agent">${escHtml(task.agent)}</span>
            </span>
            <span class="orchestrate-task-preview">${escHtml(
              inlinePreview(displayPrompt || 'Waiting to run'),
            )}</span>
          </span>
          <span class="orchestrate-task-status">${statusText('pending')}</span>
          <span class="chevron">\u25b8</span>
        </div>
      `;

      const panel = createDetachedSubagentPanel(
        task.agent,
        displayPrompt,
        compositeTaskId(orchestrateId, task.id),
      );
      panel.dataset.orchestrateId = orchestrateId;
      panel.dataset.orchestrateTaskId = task.id;

      const anchor = panel.parentElement;
      if (anchor) row.appendChild(anchor);

      taskContainer.appendChild(row);
      taskRows.set(task.id, row);
      taskPanels.set(task.id, panel);
      taskLayer.set(task.id, layerIndex);
    }

    layerEl.appendChild(taskContainer);
    layersContainer.appendChild(layerEl);
  }

  return { taskRows, taskPanels, taskLayer, layerCount };
}

function syncSharedTaskPanel(entry, data, status: Exclude<TaskStatus, 'pending' | 'running'> | 'running') {
  const panel = entry?.taskPanels.get(data?.id);
  if (!panel) return;
  const shouldCollapseImmediately = !panel.classList.contains('subagent-modal-open');

  const ref = {
    task_id: compositeTaskId(data.orchestrate_id, data.id),
    agent: panel.dataset.agent || data.agent || '',
  };

  if (data.prompt) {
    updateSubagentPrompt(ref, data.prompt);
  }

  if (status === 'completed') {
    finishSubagentPanel(
      ref,
      true,
      {
        cycles: data.cycles,
        tool_calls: data.tool_calls,
        duration_ms: data.duration_ms,
        input_tokens: data.input_tokens,
        output_tokens: data.output_tokens,
        result_excerpt: data.result_excerpt,
        result_preview: data.result_preview,
      },
      { immediate: shouldCollapseImmediately },
    );
    return;
  }

  if (status === 'failed') {
    finishSubagentPanel(
      ref,
      false,
      {
        cycles: data.cycles,
        tool_calls: data.tool_calls,
        duration_ms: data.duration_ms,
        input_tokens: data.input_tokens,
        output_tokens: data.output_tokens,
        error: data.error,
      },
      { immediate: shouldCollapseImmediately },
    );
    return;
  }

  if (status === 'skipped') {
    finishSubagentPanel(
      ref,
      true,
      {
        cycles: data.cycles,
        tool_calls: data.tool_calls,
        duration_ms: data.duration_ms,
        status_label: 'Skipped',
        summary_title: 'Skip reason',
        summary_tone: 'muted',
        summary_body: data.reason || 'Task skipped.',
      },
      { immediate: shouldCollapseImmediately },
    );
  }
}

export function closeOrchestrateTaskModal() {
  closeSubagentModal();
}

export function openOrchestrateTaskModal(trigger: HTMLElement | null) {
  const row = trigger?.closest?.('.orchestrate-task') as HTMLElement | null;
  if (!row) return;

  const orchestrateId = row.dataset.orchestrateId || '';
  const taskId = row.dataset.taskId || '';
  const entry = state.activeOrchestrations.get(orchestrateId);
  const panel = entry?.taskPanels.get(taskId) || null;
  if (!panel) return;

  row.querySelector('.orchestrate-task-summary')?.setAttribute('aria-expanded', 'true');
  openSubagentPanelModal(panel);
}

export function createOrchestratePanel(data) {
  const registry = ensureRegistry();
  if (!data?.orchestrate_id) return;

  const existing = registry.get(data.orchestrate_id);
  if (existing?.panel) {
    removeTimelinePanel(existing.panel);
    registry.delete(data.orchestrate_id);
  }

  const panel = document.createElement('div');
  panel.className = 'orchestrate-panel orchestrate-active';
  panel.dataset.orchestrateId = data.orchestrate_id;

  const header = document.createElement('div');
  header.className = 'orchestrate-header';
  header.dataset.action = 'toggle-tool';
  header.innerHTML = `
    <span class="orchestrate-icon">\u2699</span>
    <span class="orchestrate-label">Orchestrate / ${data.task_count || 0} tasks / ${
      data.layer_count || 0
    } layers</span>
    <span class="orchestrate-status">Running</span>
    <span class="chevron">\u25b8</span>
  `;

  const body = document.createElement('div');
  body.className = 'orchestrate-body';

  const overview = document.createElement('div');
  overview.className = 'orchestrate-overview';
  overview.innerHTML = `
    <div class="orchestrate-progress">
      <div class="orchestrate-progress-bar">
        <span class="orchestrate-progress-segment is-completed" data-orchestrate-progress="completed" hidden></span>
        <span class="orchestrate-progress-segment is-running" data-orchestrate-progress="running" hidden></span>
        <span class="orchestrate-progress-segment is-failed" data-orchestrate-progress="failed" hidden></span>
        <span class="orchestrate-progress-segment is-skipped" data-orchestrate-progress="skipped" hidden></span>
        <span class="orchestrate-progress-segment is-pending" data-orchestrate-progress="pending"></span>
      </div>
      <div class="orchestrate-progress-label" data-orchestrate-progress-label>
        0/${Array.isArray(data.tasks) ? data.tasks.length : 0} completed
      </div>
    </div>
  `;
  body.appendChild(overview);

  const layers = document.createElement('div');
  layers.className = 'orchestrate-layers';
  body.appendChild(layers);

  const summary = document.createElement('div');
  summary.className = 'orchestrate-summary hidden';
  body.appendChild(summary);

  panel.appendChild(header);
  panel.appendChild(body);

  const tasks = Array.isArray(data.tasks) ? data.tasks : [];
  const layout = buildDagLayout(layers, tasks, data.orchestrate_id);

  if (!data.layer_count && layout.layerCount) {
    const label = panel.querySelector('.orchestrate-label') as HTMLElement | null;
    if (label) {
      label.textContent = `Orchestrate / ${tasks.length} tasks / ${layout.layerCount} layers`;
    }
  }

  const currentRow = state.currentMsg ? state.currentMsg.closest('.msg-row') : null;
  const wrapper = wrapInTimeline(panel, 'orchestrate');
  if (currentRow) {
    dom.chat.insertBefore(wrapper, currentRow);
  } else {
    dom.chat.appendChild(wrapper);
  }

  pinReactStatusToBottom();
  animatePanelIn(panel);
  scrollDown();

  registry.set(data.orchestrate_id, {
    panel,
    taskRows: layout.taskRows,
    taskPanels: layout.taskPanels,
    taskLayer: layout.taskLayer,
    layerCount: layout.layerCount,
    live: true,
  });

  updateHeaderProgress(registry.get(data.orchestrate_id));
}

export function updateOrchestrateLayer(data) {
  const entry = ensureRegistry().get(data?.orchestrate_id);
  if (!entry) return;

  const layerIndex = (data.layer || 1) - 1;
  const layers = entry.panel.querySelectorAll('.orchestrate-layer');
  layers.forEach((layerEl, index) => {
    layerEl.classList.toggle('orchestrate-layer-active', index === layerIndex);
  });
}

export function markOrchestrateTask(data, status: Exclude<TaskStatus, 'pending'>) {
  const entry = ensureRegistry().get(data?.orchestrate_id);
  if (!entry) return;

  const row = entry.taskRows.get(data.id);
  if (!row) return;

  row.classList.remove(
    'orchestrate-task-pending',
    'orchestrate-task-running',
    'orchestrate-task-completed',
    'orchestrate-task-failed',
    'orchestrate-task-skipped',
  );
  row.classList.add(`orchestrate-task-${status}`);
  setTaskStatusTone(row, status);

  const iconEl = row.querySelector('.orchestrate-task-icon') as HTMLElement | null;
  if (iconEl) iconEl.textContent = statusIcon(status);

  const statusEl = row.querySelector('.orchestrate-task-status') as HTMLElement | null;
  if (statusEl) {
    const parts = [statusText(status)];
    if (status === 'completed' || status === 'failed') {
      if (data.duration_ms != null) {
        const duration = formatToolDuration(data.duration_ms);
        if (duration) parts.push(duration);
      }
      if (data.input_tokens != null || data.output_tokens != null) {
        const tokens: string[] = [];
        if (data.input_tokens != null) tokens.push(formatTokenCount(data.input_tokens));
        if (data.output_tokens != null) tokens.push(formatTokenCount(data.output_tokens));
        if (tokens.length) parts.push(`${tokens.join('/')} tok`);
      }
    } else if (status === 'skipped' && data.reason) {
      parts.push(String(data.reason).slice(0, 60));
    }
    statusEl.textContent = parts.join(' / ');
  }

  const displayPrompt = stripDelegatedPromptRuntimeContext(data.prompt || '');
  if (data.prompt) {
    row.dataset.promptPreview = displayPrompt;
  }

  if (status === 'running') {
    setTaskPreview(row, displayPrompt || row.dataset.promptPreview || 'Running');
    row.removeAttribute('title');
    pulseFocus(row);
  } else if (status === 'completed') {
    setTaskPreview(row, data.result_excerpt || data.result_preview || 'Task completed');
    row.removeAttribute('title');
  } else if (status === 'failed') {
    setTaskPreview(row, data.error || 'Task failed');
    if (data.error) {
      row.title = String(data.error).slice(0, 200);
    } else {
      row.removeAttribute('title');
    }
    pulseFocus(row);
  } else if (status === 'skipped') {
    setTaskPreview(row, data.reason || 'Task skipped');
    row.removeAttribute('title');
  }

  syncSharedTaskPanel(entry, data, status);
  updateHeaderProgress(entry);
}

export function finishOrchestratePanel(data) {
  const entry = ensureRegistry().get(data?.orchestrate_id);
  if (!entry) return;

  const { panel } = entry;
  panel.classList.remove('orchestrate-active');
  panel.classList.add(data.aborted ? 'orchestrate-aborted' : 'orchestrate-done');

  panel.querySelectorAll('.orchestrate-layer-active').forEach((el) => {
    el.classList.remove('orchestrate-layer-active');
  });

  const status = panel.querySelector('.orchestrate-status') as HTMLElement | null;
  if (status) {
    const parts = [`${data.completed || 0} completed`];
    if (data.failed) parts.push(`${data.failed} failed`);
    if (data.skipped) parts.push(`${data.skipped} skipped`);
    if (data.duration_ms != null) {
      const duration = formatToolDuration(data.duration_ms);
      if (duration) parts.push(duration);
    }
    status.textContent = data.aborted
      ? `Aborted (${parts.join(' / ')})`
      : `Completed (${parts.join(' / ')})`;
  }

  const summary = panel.querySelector('.orchestrate-summary') as HTMLElement | null;
  if (summary) {
    const metrics = [
      `${data.completed || 0} completed`,
      data.failed ? `${data.failed} failed` : '',
      data.skipped ? `${data.skipped} skipped` : '',
      data.duration_ms != null ? `Duration ${formatToolDuration(data.duration_ms)}` : '',
      data.input_tokens != null ? `In ${formatTokenCount(data.input_tokens)}` : '',
      data.output_tokens != null ? `Out ${formatTokenCount(data.output_tokens)}` : '',
    ].filter(Boolean);

    summary.innerHTML = `
      <div class="orchestrate-summary-head">
        <div class="orchestrate-summary-title">${
          data.aborted ? 'Execution aborted' : 'Execution summary'
        }</div>
        <div class="orchestrate-summary-metrics">
          ${metrics
            .map((metric) => `<span class="orchestrate-summary-chip">${escHtml(metric)}</span>`)
            .join('')}
        </div>
      </div>
    `;
    summary.classList.remove('hidden');
  }

  const body = panel.querySelector('.orchestrate-body') as HTMLElement | null;
  const chevron = panel.querySelector('.orchestrate-header .chevron') as HTMLElement | null;
  if (body?.classList.contains('show')) {
    animateCollapsibleSection(body, false);
  }
  chevron?.classList.remove('open');

  syncProgressVisuals(entry);
  entry.live = false;
}
