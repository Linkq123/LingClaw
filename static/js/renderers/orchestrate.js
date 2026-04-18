import { dom, state } from '../state.js';
import { escHtml, formatToolDuration, formatTokenCount } from '../utils.js';
import { scrollDown } from '../scroll.js';
import { wrapInTimeline, animatePanelIn, animateCollapsibleSection, removeTimelinePanel } from './timeline.js';
import { pinReactStatusToBottom } from './react-status.js';

/**
 * Return the shared orchestration registry from global state.
 * Keyed by orchestrate_id. Each entry holds:
 *   { panel, taskRows: Map<taskId, HTMLElement>, taskLayer: Map<taskId, layerIdx>, layerCount }
 */
function ensureRegistry() {
  return state.activeOrchestrations;
}

function statusText(status) {
  switch (status) {
    case 'running': return '执行中';
    case 'completed': return '完成';
    case 'failed': return '失败';
    case 'skipped': return '跳过';
    case 'pending':
    default: return '等待';
  }
}

function statusIcon(status) {
  switch (status) {
    case 'running': return '⏳';
    case 'completed': return '✅';
    case 'failed': return '❌';
    case 'skipped': return '⏭️';
    case 'pending':
    default: return '⚪';
  }
}

/**
 * Build the DAG layout (layers × tasks) into the given container.
 * @param {HTMLElement} layersContainer
 * @param {Array<{id:string,agent:string,depends_on:string[]}>} tasks
 * @returns {{ taskRows: Map<string,HTMLElement>, taskLayer: Map<string,number>, layerCount: number }}
 */
function buildDagLayout(layersContainer, tasks) {
  // Compute layers: a task belongs to layer = 1 + max(layer(dep)).
  const layer = new Map();
  const deps = new Map();
  for (const t of tasks) deps.set(t.id, t.depends_on || []);
  // Iterative Kahn-ish assignment; supports up to tasks.length passes.
  let changed = true;
  while (changed) {
    changed = false;
    for (const t of tasks) {
      if (layer.has(t.id)) continue;
      const depLayers = (deps.get(t.id) || []).map((d) => layer.get(d));
      if (depLayers.some((x) => x === undefined)) continue;
      const max = depLayers.length ? Math.max(...depLayers) : -1;
      layer.set(t.id, max + 1);
      changed = true;
    }
  }
  // Anything still unassigned (impossible if plan is a DAG) → layer 0.
  for (const t of tasks) {
    if (!layer.has(t.id)) layer.set(t.id, 0);
  }

  const layerCount = Math.max(0, ...Array.from(layer.values())) + 1;
  const buckets = Array.from({ length: layerCount }, () => []);
  for (const t of tasks) buckets[layer.get(t.id)].push(t);

  const taskRows = new Map();
  const taskLayer = new Map();
  for (let li = 0; li < buckets.length; li++) {
    const layerEl = document.createElement('div');
    layerEl.className = 'orchestrate-layer';
    layerEl.dataset.layerIndex = String(li);

    const header = document.createElement('div');
    header.className = 'orchestrate-layer-header';
    header.textContent = `Layer ${li + 1}${buckets[li].length > 1 ? ' (parallel)' : ''}`;
    layerEl.appendChild(header);

    const taskContainer = document.createElement('div');
    taskContainer.className = 'orchestrate-task-grid';
    for (const t of buckets[li]) {
      const row = document.createElement('div');
      row.className = 'orchestrate-task orchestrate-task-pending';
      row.dataset.taskId = t.id;
      row.innerHTML = `
        <span class="orchestrate-task-icon">${statusIcon('pending')}</span>
        <span class="orchestrate-task-id">${escHtml(t.id)}</span>
        <span class="orchestrate-task-agent">${escHtml(t.agent)}</span>
        <span class="orchestrate-task-status">${statusText('pending')}</span>
      `;
      taskContainer.appendChild(row);
      taskRows.set(t.id, row);
      taskLayer.set(t.id, li);
    }
    layerEl.appendChild(taskContainer);
    layersContainer.appendChild(layerEl);
  }

  return { taskRows, taskLayer, layerCount };
}

/**
 * Handle `orchestrate_started` event.
 * @param {object} data
 */
export function createOrchestratePanel(data) {
  const registry = ensureRegistry();
  if (!data || !data.orchestrate_id) return;

  const existing = registry.get(data.orchestrate_id);
  if (existing && existing.panel) {
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
    <span class="orchestrate-icon">🗺️</span>
    <span class="orchestrate-label">Orchestrate · ${data.task_count || 0} tasks · ${data.layer_count || 0} layers</span>
    <span class="orchestrate-status">执行中</span>
    <span class="chevron open">▸</span>
  `;

  const body = document.createElement('div');
  body.className = 'orchestrate-body show';

  const layers = document.createElement('div');
  layers.className = 'orchestrate-layers';
  body.appendChild(layers);

  const summary = document.createElement('div');
  summary.className = 'orchestrate-summary hidden';
  body.appendChild(summary);

  panel.appendChild(header);
  panel.appendChild(body);

  const tasks = Array.isArray(data.tasks) ? data.tasks : [];
  const layout = buildDagLayout(layers, tasks);

  // Update header with computed layer count when the provided value is 0 or
  // missing (e.g. during history replay where layer_count isn't available).
  if (!data.layer_count && layout.layerCount) {
    const label = panel.querySelector('.orchestrate-label');
    if (label) label.textContent = `Orchestrate · ${tasks.length} tasks · ${layout.layerCount} layers`;
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
    taskLayer: layout.taskLayer,
    layerCount: layout.layerCount,
  });
}

/**
 * Handle `orchestrate_layer` event — highlight the layer currently running.
 */
export function updateOrchestrateLayer(data) {
  const registry = ensureRegistry();
  const entry = registry.get(data && data.orchestrate_id);
  if (!entry) return;
  const layerIdx = (data.layer || 1) - 1;
  const layers = entry.panel.querySelectorAll('.orchestrate-layer');
  layers.forEach((layerEl, idx) => {
    layerEl.classList.toggle('orchestrate-layer-active', idx === layerIdx);
  });
}

/**
 * Handle all per-task status events.
 * @param {object} data
 * @param {'running'|'completed'|'failed'|'skipped'} status
 */
export function markOrchestrateTask(data, status) {
  const registry = ensureRegistry();
  const entry = registry.get(data && data.orchestrate_id);
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

  const iconEl = row.querySelector('.orchestrate-task-icon');
  if (iconEl) iconEl.textContent = statusIcon(status);
  const statusEl = row.querySelector('.orchestrate-task-status');
  if (statusEl) {
    const parts = [statusText(status)];
    if (status === 'completed' || status === 'failed') {
      if (data.duration_ms != null) {
        const dur = formatToolDuration(data.duration_ms);
        if (dur) parts.push(dur);
      }
      if (data.input_tokens != null || data.output_tokens != null) {
        const tokens = [];
        if (data.input_tokens != null) tokens.push(formatTokenCount(data.input_tokens));
        if (data.output_tokens != null) tokens.push(formatTokenCount(data.output_tokens));
        if (tokens.length) parts.push(`${tokens.join('/')} tok`);
      }
    } else if (status === 'skipped' && data.reason) {
      parts.push(String(data.reason).slice(0, 60));
    }
    statusEl.textContent = parts.join(' · ');
  }
  if (status === 'failed' && data.error) {
    row.title = String(data.error).slice(0, 200);
  }
}

/**
 * Handle `orchestrate_completed` event — finalize the panel and collapse.
 */
export function finishOrchestratePanel(data) {
  const registry = ensureRegistry();
  const entry = registry.get(data && data.orchestrate_id);
  if (!entry) return;
  const { panel } = entry;

  panel.classList.remove('orchestrate-active');
  panel.classList.add(data.aborted ? 'orchestrate-aborted' : 'orchestrate-done');

  // Clear any layer highlight.
  panel.querySelectorAll('.orchestrate-layer-active').forEach((el) => {
    el.classList.remove('orchestrate-layer-active');
  });

  const status = panel.querySelector('.orchestrate-status');
  if (status) {
    const parts = [];
    parts.push(`${data.completed || 0} ✓`);
    if (data.failed) parts.push(`${data.failed} ✗`);
    if (data.skipped) parts.push(`${data.skipped} ⏭`);
    if (data.duration_ms != null) {
      const dur = formatToolDuration(data.duration_ms);
      if (dur) parts.push(dur);
    }
    status.textContent = data.aborted ? `中断 (${parts.join(' · ')})` : `完成 (${parts.join(' · ')})`;
  }

  const summary = panel.querySelector('.orchestrate-summary');
  if (summary) {
    const lines = [];
    const tokens = [];
    if (data.input_tokens != null) tokens.push(`in ${formatTokenCount(data.input_tokens)}`);
    if (data.output_tokens != null) tokens.push(`out ${formatTokenCount(data.output_tokens)}`);
    if (tokens.length) {
      lines.push(`<div class="orchestrate-tokens">Tokens: ${escHtml(tokens.join(' · '))}</div>`);
    }
    if (lines.length) {
      summary.innerHTML = lines.join('');
      summary.classList.remove('hidden');
    }
  }

  // Collapse after a short delay so users can see the final state first.
  setTimeout(() => {
    const body = panel.querySelector('.orchestrate-body');
    const chevron = panel.querySelector('.chevron');
    if (body) animateCollapsibleSection(body, false);
    if (chevron) chevron.classList.remove('open');
  }, 800);

  registry.delete(data.orchestrate_id);
}
