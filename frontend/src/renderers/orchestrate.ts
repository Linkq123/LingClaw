import { state } from '../state.js';
import {
  escHtml,
  formatToolDuration,
  formatTokenCount,
  inlinePreview,
  stripDelegatedPromptRuntimeContext,
  pulseFocus,
} from '../utils.js';
import { scrollDown } from '../scroll.js';
import { animatePanelIn, animateCollapsibleSection, linkCollapsibleControl } from './timeline.js';
import {
  mountExecutionPanel,
  refreshExecutionStackForPanel,
  removeExecutionPanel,
} from './execution-stack.js';
import { pinReactStatusToBottom } from './react-status.js';
import {
  closeSubagentModal,
  createDetachedSubagentPanel,
  finishSubagentPanel,
  openSubagentPanelModal,
  updateSubagentPrompt,
} from './subagent.js';
import { iconMarkup } from '../icons.js';
import type { IconName } from '../icons.js';
import { tr } from '../i18n.js';

type TaskStatus = 'pending' | 'running' | 'completed' | 'failed' | 'skipped';

function ensureRegistry() {
  return state.activeOrchestrations;
}

function orchestrationLabel(taskCount: number, layerCount: number): string {
  return `${tr('execution.orchestration')} · ${tr('execution.orchestrationSummary', {
    tasks: taskCount,
    layers: layerCount,
  })}`;
}

function layerLabel(layerIndex: number, parallel: boolean): string {
  const label = tr('execution.layerLabel', { layer: layerIndex + 1 });
  return parallel ? `${label} (${tr('execution.parallel')})` : label;
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
    const parts = [
      tr('execution.progressCompleted', {
        completed: counts.completed,
        total: counts.total,
      }),
      `${completionPercent}%`,
    ];
    if (counts.running) parts.push(tr('execution.runningCount', { count: counts.running }));
    if (counts.failed) parts.push(tr('execution.failedCount', { count: counts.failed }));
    if (counts.skipped) parts.push(tr('execution.skippedCount', { count: counts.skipped }));
    if (counts.pending) parts.push(tr('execution.pendingCount', { count: counts.pending }));
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

    const segmentLabel = statusText(key as TaskStatus);
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
  if (running) parts.push(tr('execution.runningCount', { count: running }));
  if (failed) parts.push(tr('execution.failedCount', { count: failed }));
  if (skipped) parts.push(tr('execution.skippedCount', { count: skipped }));
  if (!running && pending && completed < total) {
    parts.push(tr('execution.pendingCount', { count: pending }));
  }
  statusEl.textContent = parts.join(' / ');
  refreshExecutionStackForPanel(entry.panel);
}

function statusText(status: TaskStatus) {
  switch (status) {
    case 'running':
      return tr('execution.running');
    case 'completed':
      return tr('execution.completed');
    case 'failed':
      return tr('tool.failed');
    case 'skipped':
      return tr('execution.skipped');
    case 'pending':
    default:
      return tr('execution.waiting');
  }
}

function statusIcon(status: TaskStatus): IconName {
  switch (status) {
    case 'running':
      return 'circle-dot';
    case 'completed':
      return 'check';
    case 'failed':
      return 'close';
    case 'skipped':
      return 'skip';
    case 'pending':
    default:
      return 'more';
  }
}

function compositeTaskId(orchestrateId: string, taskId: string) {
  return `${orchestrateId}:${taskId}`;
}

function setTaskPreview(row: HTMLElement, text: string) {
  const previewEl = row.querySelector('.orchestrate-task-preview') as HTMLElement | null;
  if (!previewEl) return;
  previewEl.textContent = inlinePreview(text || tr('execution.waitingToRun'));
}

function taskPreviewFallback(kind: string): string {
  switch (kind) {
    case 'running':
      return tr('execution.running');
    case 'completed':
      return tr('execution.taskCompleted');
    case 'failed':
      return tr('execution.taskFailed');
    case 'skipped':
      return tr('execution.taskSkipped');
    case 'waiting':
    default:
      return tr('execution.waitingToRun');
  }
}

function syncTaskRowPresentation(row: HTMLElement): void {
  const status = (row.dataset.taskStatus || 'pending') as TaskStatus;
  const statusParts = [statusText(status)];
  if (row.dataset.statusDurationMs) {
    const duration = formatToolDuration(Number(row.dataset.statusDurationMs));
    if (duration) statusParts.push(duration);
  }
  if (row.dataset.statusInputTokens || row.dataset.statusOutputTokens) {
    const tokens = [row.dataset.statusInputTokens, row.dataset.statusOutputTokens].filter(Boolean);
    if (tokens.length) {
      statusParts.push(
        tr('execution.tokensMetric', {
          count: tokens.map((token) => formatTokenCount(Number(token))).join('/'),
        }),
      );
    }
  }
  if (status === 'skipped' && row.dataset.statusReason) {
    statusParts.push(row.dataset.statusReason.slice(0, 60));
  }
  const statusEl = row.querySelector<HTMLElement>('.orchestrate-task-status');
  if (statusEl) statusEl.textContent = statusParts.join(' / ');

  setTaskPreview(
    row,
    row.dataset.previewText || taskPreviewFallback(row.dataset.previewKind || 'waiting'),
  );
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
    header.dataset.layerIndex = String(layerIndex);
    header.dataset.layerParallel = String(buckets[layerIndex].length > 1);
    header.textContent = layerLabel(layerIndex, buckets[layerIndex].length > 1);
    layerEl.appendChild(header);

    const taskContainer = document.createElement('div');
    taskContainer.className = 'orchestrate-task-grid';

    for (const task of buckets[layerIndex]) {
      const displayPrompt = stripDelegatedPromptRuntimeContext(task.prompt_preview || '');
      const row = document.createElement('div');
      row.className = 'orchestrate-task orchestrate-task-pending';
      row.dataset.orchestrateId = orchestrateId;
      row.dataset.taskId = task.id;
      row.dataset.taskStatus = 'pending';
      row.dataset.previewKind = 'waiting';
      row.dataset.previewText = displayPrompt;
      if (displayPrompt) row.dataset.promptPreview = displayPrompt;
      row.innerHTML = `
        <button
          type="button"
          class="orchestrate-task-summary"
          data-action="open-orchestrate-task-modal"
          aria-expanded="false"
          aria-haspopup="dialog"
        >
          <span class="orchestrate-task-icon">${iconMarkup(statusIcon('pending'))}</span>
          <span class="orchestrate-task-main">
            <span class="orchestrate-task-title">
              <span class="orchestrate-task-id">${escHtml(task.id)}</span>
              <span class="orchestrate-task-agent">${escHtml(task.agent)}</span>
            </span>
            <span class="orchestrate-task-preview">${escHtml(
              inlinePreview(displayPrompt || tr('execution.waitingToRun')),
            )}</span>
          </span>
          <span class="orchestrate-task-status">${statusText('pending')}</span>
          <span class="chevron">${iconMarkup('chevron-right')}</span>
        </button>
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

function syncSharedTaskPanel(
  entry,
  data,
  status: Exclude<TaskStatus, 'pending' | 'running'> | 'running',
) {
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
        summary_title_key: 'execution.skipReason',
        summary_tone: 'muted',
        summary_body: data.reason || '',
        summary_body_key: data.reason ? undefined : 'execution.taskSkipped',
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
  openSubagentPanelModal(panel, trigger);
}

function syncReusedTaskMetadata(
  existingRow: HTMLElement,
  existingPanel: HTMLElement,
  nextRow: HTMLElement,
  task,
) {
  const displayPrompt = stripDelegatedPromptRuntimeContext(task.prompt_preview || '');

  existingRow.dataset.orchestrateId =
    nextRow.dataset.orchestrateId || existingRow.dataset.orchestrateId || '';
  existingRow.dataset.taskId = nextRow.dataset.taskId || existingRow.dataset.taskId || '';
  if (displayPrompt) {
    existingRow.dataset.promptPreview = displayPrompt;
  } else {
    delete existingRow.dataset.promptPreview;
  }

  existingRow.dataset.previewText = displayPrompt;
  existingRow.dataset.previewKind = 'waiting';
  setTaskPreview(existingRow, displayPrompt || tr('execution.waitingToRun'));
  existingPanel.dataset.orchestrateId = existingRow.dataset.orchestrateId || '';
  existingPanel.dataset.orchestrateTaskId = existingRow.dataset.taskId || '';
  state.activeSubagentPanels.set(
    compositeTaskId(existingRow.dataset.orchestrateId || '', existingRow.dataset.taskId || ''),
    existingPanel,
  );
  updateSubagentPrompt(
    {
      task_id: compositeTaskId(
        existingRow.dataset.orchestrateId || '',
        existingRow.dataset.taskId || '',
      ),
      agent: existingPanel.dataset.agent || task.agent || '',
    },
    displayPrompt,
    { allowBlank: true },
  );
}

function reuseSyntheticTaskRows(existing, layout, nextTasks) {
  for (const task of nextTasks) {
    const existingRow = existing.taskRows.get(task.id);
    const existingPanel = existing.taskPanels.get(task.id);
    const nextRow = layout.taskRows.get(task.id);
    if (!existingRow || !existingPanel || !nextRow) continue;

    syncReusedTaskMetadata(existingRow, existingPanel, nextRow, task);
    nextRow.replaceWith(existingRow);
    layout.taskRows.set(task.id, existingRow);
    layout.taskPanels.set(task.id, existingPanel);
  }
}

function applySyntheticLayoutMerge(
  existing,
  layers: HTMLElement,
  nextLayers: HTMLElement,
  layout,
  layerCount: number,
) {
  layers.replaceChildren(...Array.from(nextLayers.children));
  existing.taskRows = layout.taskRows;
  existing.taskPanels = layout.taskPanels;
  existing.taskLayer = layout.taskLayer;
  existing.layerCount = layerCount;
  updateHeaderProgress(existing);
  return true;
}

function mergeSyntheticOrchestratePanel(existing, data) {
  const nextTasks = Array.isArray(data.tasks) ? data.tasks : [];
  const nextTaskIds = new Set(nextTasks.map((task) => task.id));
  const currentTaskIds = new Set(existing.taskRows.keys());
  const sameTasks =
    nextTaskIds.size === currentTaskIds.size &&
    Array.from(nextTaskIds).every((taskId) => currentTaskIds.has(taskId));

  existing.panel.dataset.synthetic = data.synthetic === true ? 'true' : 'false';
  const label = existing.panel.querySelector('.orchestrate-label') as HTMLElement | null;

  const layers = existing.panel.querySelector('.orchestrate-layers') as HTMLElement | null;
  if (!layers) return false;

  const nextLayers = document.createElement('div');
  const layout = buildDagLayout(nextLayers, nextTasks, data.orchestrate_id);
  const nextTaskCount = data.task_count || nextTasks.length;
  const nextLayerCount = data.layer_count || layout.layerCount || existing.layerCount;
  existing.panel.dataset.taskCount = String(nextTaskCount);
  existing.panel.dataset.layerCount = String(nextLayerCount);
  if (label) {
    label.textContent = orchestrationLabel(nextTaskCount, nextLayerCount);
  }

  reuseSyntheticTaskRows(existing, layout, nextTasks);
  return applySyntheticLayoutMerge(
    existing,
    layers,
    nextLayers,
    layout,
    sameTasks ? nextLayerCount : layout.layerCount,
  );
}

export function createOrchestratePanel(data) {
  const registry = ensureRegistry();
  if (!data?.orchestrate_id) return;

  const existing = registry.get(data.orchestrate_id);
  if (existing?.panel) {
    if (existing.panel.dataset.synthetic === 'true') {
      if (mergeSyntheticOrchestratePanel(existing, data)) {
        return;
      }
    }

    removeExecutionPanel(existing.panel);
    registry.delete(data.orchestrate_id);
  }

  const panel = document.createElement('div');
  panel.className = 'orchestrate-panel orchestrate-active';
  panel.dataset.orchestrateId = data.orchestrate_id;
  panel.dataset.taskCount = String(data.task_count || 0);
  panel.dataset.layerCount = String(data.layer_count || 0);
  if (data.synthetic === true) panel.dataset.synthetic = 'true';

  const header = document.createElement('button');
  header.type = 'button';
  header.className = 'orchestrate-header';
  header.dataset.action = 'toggle-tool';
  header.setAttribute('aria-expanded', 'false');
  header.innerHTML = `
    <span class="orchestrate-icon">${iconMarkup('workflow')}</span>
    <span class="orchestrate-label">${orchestrationLabel(data.task_count || 0, data.layer_count || 0)}</span>
    <span class="orchestrate-status">${tr('execution.running')}</span>
    <span class="chevron">${iconMarkup('chevron-right')}</span>
  `;

  const body = document.createElement('div');
  body.className = 'orchestrate-body';
  linkCollapsibleControl(header, body, 'orchestrate-body');

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
  panel.dataset.taskCount = String(tasks.length || data.task_count || 0);
  const layout = buildDagLayout(layers, tasks, data.orchestrate_id);

  if (!data.layer_count && layout.layerCount) {
    panel.dataset.layerCount = String(layout.layerCount);
    const label = panel.querySelector('.orchestrate-label') as HTMLElement | null;
    if (label) {
      label.textContent = orchestrationLabel(tasks.length, layout.layerCount);
    }
  }

  const currentRow = state.currentMsg ? state.currentMsg.closest('.msg-row') : null;
  mountExecutionPanel(panel, 'orchestrate', currentRow);

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
  row.dataset.taskStatus = status;
  row.dataset.statusDurationMs = data.duration_ms == null ? '' : String(data.duration_ms);
  row.dataset.statusInputTokens = data.input_tokens == null ? '' : String(data.input_tokens);
  row.dataset.statusOutputTokens = data.output_tokens == null ? '' : String(data.output_tokens);
  row.dataset.statusReason = data.reason == null ? '' : String(data.reason);
  setTaskStatusTone(row, status);

  const iconEl = row.querySelector('.orchestrate-task-icon') as HTMLElement | null;
  if (iconEl) iconEl.innerHTML = iconMarkup(statusIcon(status));

  const displayPrompt = stripDelegatedPromptRuntimeContext(data.prompt || '');
  if (data.prompt) {
    row.dataset.promptPreview = displayPrompt;
  }

  if (status === 'running') {
    row.dataset.previewText = displayPrompt || row.dataset.promptPreview || '';
    row.dataset.previewKind = 'running';
    row.removeAttribute('title');
    pulseFocus(row);
  } else if (status === 'completed') {
    row.dataset.previewText = data.result_excerpt || data.result_preview || '';
    row.dataset.previewKind = 'completed';
    row.removeAttribute('title');
  } else if (status === 'failed') {
    row.dataset.previewText = data.error || '';
    row.dataset.previewKind = 'failed';
    if (data.error) {
      row.title = String(data.error).slice(0, 200);
    } else {
      row.removeAttribute('title');
    }
    pulseFocus(row);
  } else if (status === 'skipped') {
    row.dataset.previewText = data.reason || '';
    row.dataset.previewKind = 'skipped';
    row.removeAttribute('title');
  }

  syncTaskRowPresentation(row);

  syncSharedTaskPanel(entry, data, status);
  updateHeaderProgress(entry);
}

function renderOrchestrationSummary(panel: HTMLElement): void {
  const summary = panel.querySelector('.orchestrate-summary') as HTMLElement | null;
  if (!summary) return;

  const completed = Number(panel.dataset.completedCount || 0);
  const failed = Number(panel.dataset.failedCount || 0);
  const skipped = Number(panel.dataset.skippedCount || 0);
  const metrics = [tr('execution.completedCount', { count: completed })];
  if (failed) metrics.push(tr('execution.failedCount', { count: failed }));
  if (skipped) metrics.push(tr('execution.skippedCount', { count: skipped }));
  if (panel.dataset.durationMs) {
    const duration = formatToolDuration(Number(panel.dataset.durationMs));
    if (duration) metrics.push(`${tr('execution.duration')} ${duration}`);
  }
  if (panel.dataset.inputTokens) {
    metrics.push(
      tr('execution.inputMetric', {
        count: formatTokenCount(Number(panel.dataset.inputTokens)),
      }),
    );
  }
  if (panel.dataset.outputTokens) {
    metrics.push(
      tr('execution.outputMetric', {
        count: formatTokenCount(Number(panel.dataset.outputTokens)),
      }),
    );
  }

  summary.innerHTML = `
    <div class="orchestrate-summary-head">
      <div class="orchestrate-summary-title">${escHtml(
        panel.dataset.orchestrateAborted === 'true'
          ? tr('execution.aborted')
          : tr('execution.summary'),
      )}</div>
      <div class="orchestrate-summary-metrics">
        ${metrics
          .map((metric) => `<span class="orchestrate-summary-chip">${escHtml(metric)}</span>`)
          .join('')}
      </div>
    </div>
  `;
  summary.classList.remove('hidden');
}

export function finishOrchestratePanel(data) {
  const entry = ensureRegistry().get(data?.orchestrate_id);
  if (!entry) return;

  const { panel } = entry;
  panel.classList.remove('orchestrate-active');
  panel.classList.add(data.aborted ? 'orchestrate-aborted' : 'orchestrate-done');
  panel.dataset.completedCount = String(data.completed || 0);
  panel.dataset.failedCount = String(data.failed || 0);
  panel.dataset.skippedCount = String(data.skipped || 0);
  panel.dataset.orchestrateAborted = String(data.aborted === true);
  panel.dataset.durationMs = data.duration_ms == null ? '' : String(data.duration_ms);
  panel.dataset.inputTokens = data.input_tokens == null ? '' : String(data.input_tokens);
  panel.dataset.outputTokens = data.output_tokens == null ? '' : String(data.output_tokens);

  panel.querySelectorAll('.orchestrate-layer-active').forEach((el) => {
    el.classList.remove('orchestrate-layer-active');
  });

  const status = panel.querySelector('.orchestrate-status') as HTMLElement | null;
  if (status) {
    const parts = [tr('execution.completedCount', { count: data.completed || 0 })];
    if (data.failed) parts.push(tr('execution.failedCount', { count: data.failed }));
    if (data.skipped) parts.push(tr('execution.skippedCount', { count: data.skipped }));
    if (data.duration_ms != null) {
      const duration = formatToolDuration(data.duration_ms);
      if (duration) parts.push(duration);
    }
    status.textContent = data.aborted
      ? `${tr('execution.failed')} (${parts.join(' / ')})`
      : `${tr('execution.completed')} (${parts.join(' / ')})`;
  }

  renderOrchestrationSummary(panel);

  const body = panel.querySelector('.orchestrate-body') as HTMLElement | null;
  const chevron = panel.querySelector('.orchestrate-header .chevron') as HTMLElement | null;
  if (body?.classList.contains('show')) {
    animateCollapsibleSection(body, false);
  }
  chevron?.classList.remove('open');

  syncProgressVisuals(entry);
  refreshExecutionStackForPanel(panel);
  entry.live = false;
}

export function refreshOrchestratePanelsLanguage(): void {
  document.querySelectorAll<HTMLElement>('.orchestrate-panel').forEach((panel) => {
    const label = panel.querySelector<HTMLElement>('.orchestrate-label');
    const taskCount = Number(
      panel.dataset.taskCount || panel.querySelectorAll('.orchestrate-task').length,
    );
    const layerCount = Number(
      panel.dataset.layerCount || panel.querySelectorAll('.orchestrate-layer').length,
    );
    if (label) label.textContent = orchestrationLabel(taskCount, layerCount);

    const entry = panel.dataset.orchestrateId
      ? state.activeOrchestrations.get(panel.dataset.orchestrateId)
      : null;
    panel.querySelectorAll<HTMLElement>('.orchestrate-layer-header').forEach((header) => {
      const layerIndex = Number(header.dataset.layerIndex || 0);
      header.textContent = layerLabel(layerIndex, header.dataset.layerParallel === 'true');
    });
    getTaskRows(panel).forEach(syncTaskRowPresentation);
    if (entry) syncProgressVisuals(entry);
    if (panel.classList.contains('orchestrate-active') && entry) {
      updateHeaderProgress(entry);
      return;
    }

    const status = panel.querySelector<HTMLElement>('.orchestrate-status');
    if (!status) return;
    const completed = Number(panel.dataset.completedCount || 0);
    const failed = Number(panel.dataset.failedCount || 0);
    const skipped = Number(panel.dataset.skippedCount || 0);
    const parts = [tr('execution.completedCount', { count: completed })];
    if (failed) parts.push(tr('execution.failedCount', { count: failed }));
    if (skipped) parts.push(tr('execution.skippedCount', { count: skipped }));
    const duration = panel.dataset.durationMs
      ? formatToolDuration(Number(panel.dataset.durationMs))
      : '';
    if (duration) parts.push(duration);
    status.textContent =
      panel.dataset.orchestrateAborted === 'true'
        ? `${tr('execution.failed')} (${parts.join(' / ')})`
        : `${tr('execution.completed')} (${parts.join(' / ')})`;
    renderOrchestrationSummary(panel);
  });
}
