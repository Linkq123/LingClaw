import { dom, state } from '../state.js';
import { escHtml, formatToolDuration, formatTokenCount, formatDetailText, inlinePreview, pulseFocus, copyButtonText } from '../utils.js';
import { scrollDown } from '../scroll.js';
import { wrapInTimeline, animatePanelIn, animateCollapsibleSection, removeTimelinePanel } from './timeline.js';
import { pinReactStatusToBottom } from './react-status.js';
import { openToolDrawer, syncToolDrawer } from './tools.js';

// Parse a composite task_id of the form `<orchestrate_id>:<task_id>` emitted by
// orchestrator-wrapped sub-agent runs. Returns { orchestrateId, taskId } when
// the first segment matches a known orchestration, else null.
export function parseOrchestrateCompositeTaskId(compositeId) {
  if (typeof compositeId !== 'string' || !compositeId.includes(':')) return null;
  const idx = compositeId.indexOf(':');
  const orchestrateId = compositeId.slice(0, idx);
  const taskId = compositeId.slice(idx + 1);
  if (!orchestrateId || !taskId) return null;
  if (!state.activeOrchestrations.has(orchestrateId)) return null;
  return { orchestrateId, taskId };
}

function getTaskRows(panel) {
  return Array.from(panel.querySelectorAll('.orchestrate-task'));
}

function summarizeTaskCounts(rows) {
  const total = rows.length;
  const completed = rows.filter((row) => row.classList.contains('orchestrate-task-completed')).length;
  const failed = rows.filter((row) => row.classList.contains('orchestrate-task-failed')).length;
  const running = rows.filter((row) => row.classList.contains('orchestrate-task-running')).length;
  const skipped = rows.filter((row) => row.classList.contains('orchestrate-task-skipped')).length;
  const pending = Math.max(0, total - completed - failed - running - skipped);

  return { total, completed, failed, running, skipped, pending };
}

function setTaskExpanded(row, expand) {
  const details = row?.querySelector('.orchestrate-task-details');
  const chevron = row?.querySelector('.orchestrate-task-summary .chevron');
  if (!details) return;
  animateCollapsibleSection(details, expand);
  if (chevron) chevron.classList.toggle('open', expand);
}

function findPriorityTaskRow(panel) {
  const rows = getTaskRows(panel);
  return rows.find((row) => row.classList.contains('orchestrate-task-running'))
    || rows.find((row) => row.classList.contains('orchestrate-task-failed'))
    || rows.slice().reverse().find((row) => row.classList.contains('orchestrate-task-completed'))
    || rows[rows.length - 1]
    || null;
}

function focusTaskRow(row) {
  if (!row) return;
  setTaskExpanded(row, true);
  pulseFocus(row);
  row.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
}

function orchestrationSummaryText(panel) {
  if (!panel) return '';

  const title = panel.querySelector('.orchestrate-label')?.textContent?.trim();
  const status = panel.querySelector('.orchestrate-status')?.textContent?.trim();
  const progress = panel.querySelector('[data-orchestrate-progress-label]')?.textContent?.trim();
  const tasks = getTaskRows(panel).map((row) => {
    const taskId = row.querySelector('.orchestrate-task-id')?.textContent?.trim() || 'task';
    const agent = row.querySelector('.orchestrate-task-agent')?.textContent?.trim() || '';
    const taskStatus = row.querySelector('.orchestrate-task-status')?.textContent?.trim() || '';
    const preview = row.querySelector('.orchestrate-task-preview')?.textContent?.trim() || '';
    return [`${taskId}${agent ? ` · ${agent}` : ''}`, taskStatus, preview].filter(Boolean).join('\n');
  }).filter(Boolean);

  return [title, status, progress, tasks.join('\n\n')].filter(Boolean).join('\n\n').trim();
}

function syncActionButtons(panel) {
  if (!panel) return;

  const rows = getTaskRows(panel);
  const toggleAllBtn = panel.querySelector('[data-action="orchestrate-toggle-all"]');
  const focusBtn = panel.querySelector('[data-action="orchestrate-focus-active"]');
  const copyBtn = panel.querySelector('[data-action="orchestrate-copy-summary"]');
  const allExpanded = rows.length > 0 && rows.every((row) => row.querySelector('.orchestrate-task-details')?.classList.contains('show'));

  if (toggleAllBtn) {
    toggleAllBtn.textContent = rows.length > 0 && allExpanded ? '收起全部' : '展开全部';
    toggleAllBtn.disabled = rows.length === 0;
  }
  if (focusBtn) {
    focusBtn.disabled = rows.length === 0;
  }
  if (copyBtn) {
    copyBtn.disabled = rows.length === 0;
  }
}

function syncTaskHighlights(panel) {
  if (!panel) return;
  const rows = getTaskRows(panel);
  const hasRunning = rows.some((row) => row.classList.contains('orchestrate-task-running'));
  rows.forEach((row) => {
    row.classList.toggle('orchestrate-task-current', hasRunning && row.classList.contains('orchestrate-task-running'));
  });
}

function syncProgressVisuals(entry) {
  if (!entry?.panel) return;

  const panel = entry.panel;
  const counts = summarizeTaskCounts(getTaskRows(panel));
  const total = Math.max(1, counts.total);
  const completionPercent = counts.total > 0
    ? Math.round((counts.completed / counts.total) * 100)
    : 0;
  const progressLabel = panel.querySelector('[data-orchestrate-progress-label]');
  if (progressLabel) {
    const parts = [`${counts.completed}/${counts.total} 完成`];
    parts.push(`${completionPercent}%`);
    if (counts.running) parts.push(`${counts.running} 执行中`);
    if (counts.failed) parts.push(`${counts.failed} 失败`);
    if (counts.skipped) parts.push(`${counts.skipped} 跳过`);
    if (counts.pending) parts.push(`${counts.pending} 等待`);
    progressLabel.textContent = parts.join(' · ');
  }

  const segmentCounts = {
    completed: counts.completed,
    running: counts.running,
    failed: counts.failed,
    skipped: counts.skipped,
    pending: counts.pending,
  };
  Object.entries(segmentCounts).forEach(([key, value]) => {
    const segment = panel.querySelector(`[data-orchestrate-progress="${key}"]`);
    if (!segment) return;
    segment.style.width = `${(value / total) * 100}%`;
    segment.hidden = value === 0;
    segment.title = `${({ completed: '完成', running: '执行中', failed: '失败', skipped: '跳过', pending: '等待' })[key] || key}: ${value}`;
  });

  syncTaskHighlights(panel);
  syncActionButtons(panel);
}

function ensureTaskSection(row, key, title, content, tone = '') {
  const details = row.querySelector('.orchestrate-task-details');
  if (!details) return;

  let section = row.querySelector(`[data-orchestrate-section="${key}"]`);
  const hasContent = typeof content === 'string' && content.trim().length > 0;
  if (!hasContent) {
    if (section) section.classList.add('hidden');
    return;
  }

  if (!section) {
    section = document.createElement('div');
    section.className = 'orchestrate-task-section';
    section.dataset.orchestrateSection = key;
    details.appendChild(section);
  }

  section.classList.remove('hidden');
  section.innerHTML = `
    <div class="orchestrate-task-section-title">${escHtml(title)}</div>
    <pre class="orchestrate-task-code">${escHtml(formatDetailText(content))}</pre>
  `;
  const codeEl = section.querySelector('.orchestrate-task-code');
  if (codeEl && tone) codeEl.classList.add(tone);
}

function ensureDependencySection(row, deps = []) {
  const details = row.querySelector('.orchestrate-task-details');
  if (!details) return;
  let section = row.querySelector('[data-orchestrate-section="deps"]');

  if (!Array.isArray(deps) || deps.length === 0) {
    if (section) section.classList.add('hidden');
    return;
  }

  if (!section) {
    section = document.createElement('div');
    section.className = 'orchestrate-task-section';
    section.dataset.orchestrateSection = 'deps';
    details.appendChild(section);
  }

  section.classList.remove('hidden');
  section.innerHTML = `
    <div class="orchestrate-task-section-title">依赖任务</div>
    <div class="orchestrate-task-deps">
      ${deps.map((dep) => `<span class="orchestrate-task-dep">${escHtml(dep)}</span>`).join('')}
    </div>
  `;
}

function setTaskPreview(row, text) {
  const previewEl = row.querySelector('.orchestrate-task-preview');
  if (!previewEl) return;
  previewEl.textContent = inlinePreview(text || '等待执行');
}

function reasoningPreview(rawText, fallback = '完成') {
  const summaryText = (rawText || '').trim().replace(/\n+/g, ' ');
  const preview = summaryText.slice(0, 60);
  return preview ? preview + (summaryText.length > 60 ? '…' : '') : fallback;
}

function setTaskStatusTone(row, status) {
  row.classList.toggle('orchestrate-task-has-error', status === 'failed');
  row.classList.toggle('orchestrate-task-has-result', status === 'completed');
}

function maybeOpenTaskDetails(row, shouldOpen) {
  setTaskExpanded(row, shouldOpen);
}

function getTaskReasoningBody(row) {
  return row?.querySelector('[data-orchestrate-reasoning-body]') || null;
}

function getTaskReasoningLabel(row) {
  return row?.querySelector('[data-orchestrate-reasoning-label]') || null;
}

function ensureTaskReasoningSection(row) {
  const details = row?.querySelector('.orchestrate-task-details');
  if (!details) return null;

  let section = row.querySelector('[data-orchestrate-section="reasoning"]');
  if (!section) {
    section = document.createElement('div');
    section.className = 'orchestrate-task-section orchestrate-task-reasoning';
    section.dataset.orchestrateSection = 'reasoning';
    section.innerHTML = `
      <div class="orchestrate-task-reasoning-header" data-action="toggle-tool">
        <span class="orchestrate-task-reasoning-icon">\ud83d\udcad</span>
        <span class="orchestrate-task-reasoning-label" data-orchestrate-reasoning-label>思考链</span>
        <span class="chevron">\u25b8</span>
      </div>
      <pre class="orchestrate-task-reasoning-body" data-orchestrate-reasoning-body></pre>
    `;
    const anchor = details.querySelector('[data-orchestrate-section="tools"], [data-orchestrate-section="result"]');
    details.insertBefore(section, anchor || null);
  }

  section.classList.remove('hidden');
  return section;
}

function updateHeaderProgress(entry) {
  syncProgressVisuals(entry);

  const rows = Array.from(entry.taskRows.values());
  const { total, completed, failed, running, skipped, pending } = summarizeTaskCounts(rows);

  const statusEl = entry.panel.querySelector('.orchestrate-status');
  if (!statusEl || !entry.panel.classList.contains('orchestrate-active')) return;

  const parts = [`${completed}/${total}`];
  if (running) parts.push(`${running} 执行中`);
  if (failed) parts.push(`${failed} 失败`);
  if (skipped) parts.push(`${skipped} 跳过`);
  if (!running && pending && completed < total) parts.push(`${pending} 等待`);
  statusEl.textContent = parts.join(' · ');
}

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
 * @param {Array<{id:string,agent:string,depends_on:string[],prompt_preview?:string}>} tasks
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
      if (t.prompt_preview) row.dataset.promptPreview = t.prompt_preview;
      row.innerHTML = `
        <div class="orchestrate-task-summary" data-action="toggle-tool">
          <span class="orchestrate-task-icon">${statusIcon('pending')}</span>
          <span class="orchestrate-task-main">
            <span class="orchestrate-task-title">
              <span class="orchestrate-task-id">${escHtml(t.id)}</span>
              <span class="orchestrate-task-agent">${escHtml(t.agent)}</span>
            </span>
            <span class="orchestrate-task-preview">${escHtml(inlinePreview(t.prompt_preview || '等待执行'))}</span>
          </span>
          <span class="orchestrate-task-status">${statusText('pending')}</span>
          <span class="chevron">▸</span>
        </div>
        <div class="orchestrate-task-details"></div>
      `;
      ensureDependencySection(row, t.depends_on || []);
      ensureTaskSection(row, 'prompt', '任务说明', t.prompt_preview || '');
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
    <span class="chevron">▸</span>
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
      <div class="orchestrate-progress-label" data-orchestrate-progress-label>0/${Array.isArray(data.tasks) ? data.tasks.length : 0} 完成</div>
    </div>
    <div class="panel-actions orchestrate-actions">
      <button type="button" class="panel-action-btn" data-action="orchestrate-toggle-all">展开全部</button>
      <button type="button" class="panel-action-btn" data-action="orchestrate-focus-active">定位当前</button>
      <button type="button" class="panel-action-btn" data-action="orchestrate-copy-summary">复制摘要</button>
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

  updateHeaderProgress(registry.get(data.orchestrate_id));
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
  setTaskStatusTone(row, status);

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

  if (data.prompt) {
    row.dataset.promptPreview = data.prompt;
    ensureTaskSection(row, 'prompt', '任务说明', data.prompt);
  }

  if (status === 'running') {
    setTaskPreview(row, data.prompt || row.dataset.promptPreview || '正在执行');
    pulseFocus(row);
  } else if (status === 'completed') {
    ensureTaskSection(row, 'result', '任务输出', data.result_excerpt || '', 'is-result');
    setTaskPreview(row, data.result_excerpt || '任务完成');
  } else if (status === 'failed') {
    ensureTaskSection(row, 'result', '失败详情', data.error || '', 'is-error');
    setTaskPreview(row, data.error || '任务失败');
    maybeOpenTaskDetails(row, true);
  } else if (status === 'skipped') {
    ensureTaskSection(row, 'result', '跳过原因', data.reason || '', 'is-muted');
    setTaskPreview(row, data.reason || '任务跳过');
  }

  if (status === 'failed' && data.error) {
    row.title = String(data.error).slice(0, 200);
  }

  updateHeaderProgress(entry);
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
    const metrics = [
      `${data.completed || 0} 完成`,
      data.failed ? `${data.failed} 失败` : '',
      data.skipped ? `${data.skipped} 跳过` : '',
      data.duration_ms != null ? `耗时 ${formatToolDuration(data.duration_ms)}` : '',
      data.input_tokens != null ? `In ${formatTokenCount(data.input_tokens)}` : '',
      data.output_tokens != null ? `Out ${formatTokenCount(data.output_tokens)}` : '',
    ].filter(Boolean);
    summary.innerHTML = `
      <div class="orchestrate-summary-head">
        <div class="orchestrate-summary-title">${data.aborted ? '执行状态' : '执行摘要'}</div>
        <div class="orchestrate-summary-metrics">
          ${metrics.map((metric) => `<span class="orchestrate-summary-chip">${escHtml(metric)}</span>`).join('')}
        </div>
      </div>
    `;
    summary.classList.remove('hidden');
  }

  const body = panel.querySelector('.orchestrate-body');
  const chevron = panel.querySelector('.orchestrate-header .chevron');
  if (body?.classList.contains('show')) {
    animateCollapsibleSection(body, false);
  }
  if (chevron) chevron.classList.remove('open');

  syncProgressVisuals(entry);

  registry.delete(data.orchestrate_id);
}

export function toggleOrchestrateTasks(button) {
  const panel = button.closest('.orchestrate-panel');
  if (!panel) return;

  const rows = getTaskRows(panel);
  if (rows.length === 0) return;
  const shouldExpand = rows.some((row) => !row.querySelector('.orchestrate-task-details')?.classList.contains('show'));
  rows.forEach((row) => setTaskExpanded(row, shouldExpand));
  syncActionButtons(panel);
}

export function focusOrchestrateActive(button) {
  const panel = button.closest('.orchestrate-panel');
  if (!panel) return;

  const target = findPriorityTaskRow(panel);
  if (!target) return;
  focusTaskRow(target);
}

export function copyOrchestrateSummary(button) {
  const panel = button.closest('.orchestrate-panel');
  if (!panel) return;
  void copyButtonText(button, orchestrationSummaryText(panel), '复制摘要');
}

export function focusOrchestrateTool(button) {
  // button is the pill element itself (it carries data-action).
  // Open the shared tool drawer using the pill's dataset.
  openToolDrawer(button);
}

export function startOrchestrateTaskReasoning(orchestrateId, taskId, agentName = '') {
  const entry = state.activeOrchestrations.get(orchestrateId);
  const row = getTaskRow(entry, taskId);
  if (!row) return;

  const section = ensureTaskReasoningSection(row);
  const body = getTaskReasoningBody(row);
  const label = getTaskReasoningLabel(row);
  if (!section || !body || !label) return;

  if (!body._textNode) {
    body.textContent = '';
    body._textNode = document.createTextNode('');
    body.appendChild(body._textNode);
  }

  if (body._textNode.nodeValue.trim()) body._textNode.nodeValue += '\n\n';
  if (agentName) body._textNode.nodeValue += `[${agentName}]\n`;

  label.textContent = agentName ? `思考链 · ${agentName} 推理中` : '思考链 · 推理中';
  label.title = label.textContent;
}

export function appendOrchestrateTaskReasoning(orchestrateId, taskId, content) {
  if (!content) return;

  const entry = state.activeOrchestrations.get(orchestrateId);
  const row = getTaskRow(entry, taskId);
  if (!row) return;

  const section = ensureTaskReasoningSection(row);
  const body = getTaskReasoningBody(row);
  if (!section || !body) return;

  if (!body._textNode) {
    body.textContent = '';
    body._textNode = document.createTextNode('');
    body.appendChild(body._textNode);
  }

  body._textNode.nodeValue += content;
}

export function finishOrchestrateTaskReasoning(orchestrateId, taskId) {
  const entry = state.activeOrchestrations.get(orchestrateId);
  const row = getTaskRow(entry, taskId);
  if (!row) return;

  const body = getTaskReasoningBody(row);
  const label = getTaskReasoningLabel(row);
  if (!body || !label) return;

  const rawText = body._textNode?.nodeValue || body.textContent || '';
  const preview = reasoningPreview(rawText);
  label.textContent = `思考链 · ${preview}`;
  label.title = rawText.trim() || '完成';
}

/* ──────────────────── Inner tool chain (per task row) ──────────────────── */

function getTaskRow(entry, taskId) {
  if (!entry || !taskId) return null;
  return entry.taskRows.get(taskId) || null;
}

function ensureToolChainSection(row) {
  const details = row?.querySelector('.orchestrate-task-details');
  if (!details) return null;
  let section = row.querySelector('[data-orchestrate-section="tools"]');
  if (!section) {
    section = document.createElement('div');
    section.className = 'orchestrate-task-section orchestrate-task-tools';
    section.dataset.orchestrateSection = 'tools';
    section.innerHTML = `
      <div class="orchestrate-task-section-title">工具链</div>
      <div class="orchestrate-tool-chain" data-orchestrate-tool-chain></div>
    `;
    details.appendChild(section);
  }
  section.classList.remove('hidden');
  return section;
}

function toolChainContainer(row) {
  const section = ensureToolChainSection(row);
  return section ? section.querySelector('[data-orchestrate-tool-chain]') : null;
}

function findOrchestrateToolPill(row, toolId) {
  if (!row || !toolId) return null;
  return row.querySelector(`.orchestrate-tool-pill[data-tool-id="${CSS.escape(toolId)}"]`) || null;
}

function setToolPillState(pill, stateLabel, tone) {
  if (!pill) return;
  pill.classList.remove('is-running', 'is-done', 'is-failed');
  if (tone) pill.classList.add(tone);
  const stateEl = pill.querySelector('.orchestrate-tool-pill-state');
  if (stateEl) stateEl.textContent = stateLabel;
}

function syncToolPillContent(pill, toolName, toolArgs = '') {
  if (!pill) return;

  const nameEl = pill.querySelector('.orchestrate-tool-pill-name');
  if (nameEl) nameEl.textContent = toolName || 'tool';

  const previewText = inlinePreview(formatDetailText(toolArgs || ''), 80);
  let previewEl = pill.querySelector('.orchestrate-tool-pill-preview');
  const stateEl = pill.querySelector('.orchestrate-tool-pill-state');
  if (previewText) {
    if (!previewEl) {
      previewEl = document.createElement('span');
      previewEl.className = 'orchestrate-tool-pill-preview';
      if (stateEl) pill.insertBefore(previewEl, stateEl);
      else pill.appendChild(previewEl);
    }
    previewEl.textContent = previewText;
  } else if (previewEl) {
    previewEl.remove();
  }
}

/**
 * Append a tool pill to the task row's inline tool chain.
 * No-op if the task/orchestration is unknown.
 */
export function addOrchestrateTaskTool(orchestrateId, taskId, toolName, toolId, toolArgs = '') {
  const entry = state.activeOrchestrations.get(orchestrateId);
  const row = getTaskRow(entry, taskId);
  if (!row) return;

  const container = toolChainContainer(row);
  if (!container) return;

  const formattedArgs = formatDetailText(toolArgs || '');
  const existingPill = toolId ? findOrchestrateToolPill(row, toolId) : null;
  if (existingPill) {
    syncToolPillContent(existingPill, toolName, toolArgs);
    existingPill.dataset.toolName = toolName || 'tool';
    existingPill.dataset.toolArgs = formattedArgs;
    return;
  }

  const pill = document.createElement('button');
  pill.type = 'button';
  pill.className = 'orchestrate-tool-pill is-running';
  pill.dataset.action = 'orchestrate-focus-tool';
  pill.dataset.taskId = taskId;
  pill.dataset.orchestrateId = orchestrateId;
  if (toolId) pill.dataset.toolId = toolId;

  // Drawer-compatible dataset for openToolDrawer / syncToolDrawer.
  pill.dataset.toolName = toolName || 'tool';
  pill.dataset.toolArgs = formattedArgs;
  pill.dataset.toolResult = '';
  pill.dataset.toolHasResult = 'false';
  pill.dataset.toolStatus = '执行中';

  pill.innerHTML = `
    <span class="orchestrate-tool-pill-index">${container.childElementCount + 1}</span>
    <span class="orchestrate-tool-pill-name">${escHtml(toolName || 'tool')}</span>
    <span class="orchestrate-tool-pill-state">执行中</span>
  `;
  syncToolPillContent(pill, toolName, toolArgs);
  container.appendChild(pill);
}

/**
 * Mark an existing tool pill as completed / failed.
 * Updates drawer-compatible dataset and syncs to the tool drawer if active.
 */
export function updateOrchestrateTaskTool(orchestrateId, taskId, toolId, durationMs, result, isError = false, toolName = '') {
  const entry = state.activeOrchestrations.get(orchestrateId);
  const row = getTaskRow(entry, taskId);
  if (!row) return;

  let pill = findOrchestrateToolPill(row, toolId);
  if (!pill) {
    addOrchestrateTaskTool(orchestrateId, taskId, toolName || 'tool', toolId, '');
    pill = findOrchestrateToolPill(row, toolId);
  }
  if (!pill) return;

  const durationLabel = formatToolDuration(durationMs);
  const stateText = `${isError ? '失败' : '完成'}${durationLabel ? ` · ${durationLabel}` : ''}`;
  setToolPillState(pill, stateText, isError ? 'is-failed' : 'is-done');

  // Update drawer-compatible dataset on the pill.
  const formattedResult = typeof result === 'string' ? formatDetailText(result) : '';
  pill.dataset.toolResult = formattedResult;
  pill.dataset.toolHasResult = typeof result === 'string' ? 'true' : 'false';
  pill.dataset.toolStatus = stateText;
  if (toolName) pill.dataset.toolName = toolName;

  // Sync to the shared tool drawer if this pill is the active panel.
  if (state.activeToolPanel === pill) {
    syncToolDrawer(pill);
  }

  if (isError) {
    maybeOpenTaskDetails(row, true);
    pulseFocus(pill);
  }
}
