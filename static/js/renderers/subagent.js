import { dom, state } from '../state.js';
import { escHtml, formatToolDuration, formatTokenCount, formatDetailText, inlinePreview, pulseFocus, copyButtonText } from '../utils.js';
import { scrollDown } from '../scroll.js';
import { wrapInTimeline, animatePanelIn, animateCollapsibleSection } from './timeline.js';
import { pinReactStatusToBottom } from './react-status.js';

function setChipText(panel, key, value, extraClass = '') {
  const chip = panel.querySelector(`[data-subagent-chip="${key}"]`);
  if (!chip) return;
  chip.textContent = value;
  chip.className = 'subagent-chip';
  if (extraClass) chip.classList.add(extraClass);
}

function getToolRows(panel) {
  return Array.from(panel.querySelectorAll('.subagent-tool-row'));
}

function setToolRowExpanded(row, expand) {
  const details = row?.querySelector('.subagent-tool-details');
  const chevron = row?.querySelector('.subagent-tool-summary .chevron');
  if (!details) return;
  animateCollapsibleSection(details, expand);
  if (chevron) chevron.classList.toggle('open', expand);
}

function findPriorityToolRow(panel) {
  const rows = getToolRows(panel);
  return rows.find((row) => row.classList.contains('subagent-tool-running'))
    || rows.find((row) => row.classList.contains('subagent-tool-failed'))
    || rows[rows.length - 1]
    || null;
}

function focusToolRow(row) {
  if (!row) return;
  setToolRowExpanded(row, true);
  pulseFocus(row);
  row.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
}

function summaryCopyText(panel) {
  if (!panel) return '';

  const parts = [];
  const label = panel.querySelector('.subagent-label')?.textContent?.trim();
  const status = panel.querySelector('.subagent-status')?.textContent?.trim();
  const prompt = panel.querySelector('.subagent-prompt')?.textContent?.trim();
  const metrics = Array.from(panel.querySelectorAll('.subagent-summary-chip'))
    .map((chip) => chip.textContent.trim())
    .filter(Boolean)
    .join(' · ');
  const summaryBody = panel.querySelector('.subagent-summary:not(.hidden) .subagent-preview, .subagent-summary:not(.hidden) .subagent-error')
    ?.textContent?.trim();
  const latestOutput = Array.from(panel.querySelectorAll('.subagent-tool-output-code'))
    .map((node) => node.textContent.trim())
    .filter(Boolean)
    .slice(-1)[0];

  if (label) parts.push(label);
  if (status) parts.push(status);
  if (prompt) parts.push(`委托任务\n${prompt}`);
  if (metrics) parts.push(metrics);
  if (summaryBody) parts.push(summaryBody);
  if (!summaryBody && latestOutput) parts.push(latestOutput);

  return parts.join('\n\n').trim();
}

function syncPanelActions(panel) {
  if (!panel) return;

  const rows = getToolRows(panel);
  const toggleAllBtn = panel.querySelector('[data-action="subagent-toggle-all"]');
  const focusBtn = panel.querySelector('[data-action="subagent-focus-current"]');
  const copyBtn = panel.querySelector('[data-action="subagent-copy-summary"]');
  const allExpanded = rows.length > 0 && rows.every((row) => row.querySelector('.subagent-tool-details')?.classList.contains('show'));

  if (toggleAllBtn) {
    toggleAllBtn.textContent = rows.length > 0 && allExpanded ? '收起全部' : '展开全部';
    toggleAllBtn.disabled = rows.length === 0;
  }
  if (focusBtn) {
    focusBtn.disabled = rows.length === 0;
  }
  if (copyBtn) {
    copyBtn.disabled = !summaryCopyText(panel);
  }
}

function syncToolCount(panel) {
  const rows = getToolRows(panel);
  const total = rows.length;
  const done = rows.filter((row) => row.classList.contains('subagent-tool-done')).length;
  setChipText(panel, 'tools', total ? `${done}/${total} tools` : '0 tools');
}

function renderSummary(panel, success, stats = {}) {
  const summary = panel.querySelector('.subagent-summary');
  if (!summary) return;

  const metrics = [];
  if (stats.cycles != null) metrics.push(`Cycles ${stats.cycles}`);
  if (stats.tool_calls != null) metrics.push(`Tools ${stats.tool_calls}`);
  if (stats.duration_ms != null) {
    const duration = formatToolDuration(stats.duration_ms);
    if (duration) metrics.push(`耗时 ${duration}`);
  }
  if (stats.input_tokens != null || stats.output_tokens != null) {
    const tokens = [];
    if (stats.input_tokens != null) tokens.push(`In ${formatTokenCount(stats.input_tokens)}`);
    if (stats.output_tokens != null) tokens.push(`Out ${formatTokenCount(stats.output_tokens)}`);
    if (tokens.length) metrics.push(tokens.join(' · '));
  }

  const bodyText = success
    ? (stats.result_excerpt || stats.result_preview || '').trim()
    : (stats.error || '').trim();

  const metricHtml = metrics
    .map((metric) => `<span class="subagent-summary-chip">${escHtml(metric)}</span>`)
    .join('');
  const contentHtml = bodyText
    ? `<pre class="${success ? 'subagent-preview' : 'subagent-error'}">${escHtml(bodyText)}</pre>`
    : '';

  if (!metricHtml && !contentHtml) {
    summary.classList.add('hidden');
    summary.innerHTML = '';
    return;
  }

  summary.innerHTML = `
    <div class="subagent-summary-head">
      <div class="subagent-summary-title">${success ? '最终输出' : '失败信息'}</div>
      <div class="subagent-summary-metrics">${metricHtml}</div>
    </div>
    ${contentHtml}
  `;
  summary.classList.remove('hidden');
}

/**
 * Resolve an active sub-agent panel by task_id (preferred) or agent name
 * (legacy fallback). Before panels were keyed by agent name alone, which
 * collided when the same agent ran in parallel. The backend now emits a
 * unique `task_id` for every delegated task; old sessions / older backends
 * fall back to agent name so the UI keeps working.
 * @param {{ task_id?: string, agent?: string }} ref
 */
function resolvePanel(ref) {
  if (ref && ref.task_id && state.activeSubagentPanels.has(ref.task_id)) {
    return state.activeSubagentPanels.get(ref.task_id);
  }
  if (ref && ref.agent && state.activeSubagentPanels.has(ref.agent)) {
    return state.activeSubagentPanels.get(ref.agent);
  }
  return null;
}

function panelKey(ref) {
  if (ref && ref.task_id) return ref.task_id;
  return (ref && ref.agent) || '';
}

/**
 * Create a collapsible sub-agent panel and insert it into the chat timeline.
 * @param {string} agentName
 * @param {string} [prompt]
 * @param {string} [taskId]
 */
export function createSubagentPanel(agentName, prompt, taskId) {
  const panel = document.createElement('div');
  panel.className = 'subagent-panel subagent-active';
  panel.dataset.agent = agentName;
  if (taskId) panel.dataset.taskId = taskId;

  const header = document.createElement('div');
  header.className = 'subagent-header';
  header.dataset.action = 'toggle-tool';
  header.innerHTML = `
    <span class="subagent-icon">✦</span>
    <span class="subagent-head-copy">
      <span class="subagent-kicker">Sub-agent</span>
      <span class="subagent-label">${escHtml(agentName)}</span>
    </span>
    <span class="subagent-status">执行中</span>
    <span class="chevron open">▸</span>
  `;

  const body = document.createElement('div');
  body.className = 'subagent-body show';

  const meta = document.createElement('div');
  meta.className = 'subagent-meta';
  meta.innerHTML = `
    <span class="subagent-chip is-live" data-subagent-chip="state">运行中</span>
    <span class="subagent-chip" data-subagent-chip="cycle">Cycle 1</span>
    <span class="subagent-chip" data-subagent-chip="tools">0 tools</span>
  `;
  body.appendChild(meta);

  const actions = document.createElement('div');
  actions.className = 'panel-actions subagent-actions';
  actions.innerHTML = `
    <button type="button" class="panel-action-btn" data-action="subagent-toggle-all">展开全部</button>
    <button type="button" class="panel-action-btn" data-action="subagent-focus-current">定位当前</button>
    <button type="button" class="panel-action-btn" data-action="subagent-copy-summary" disabled>复制摘要</button>
  `;
  body.appendChild(actions);

  if (prompt) {
    const promptCard = document.createElement('div');
    promptCard.className = 'subagent-section-card';
    promptCard.innerHTML = `
      <div class="subagent-section-title">委托任务</div>
      <div class="subagent-prompt">${escHtml(prompt)}</div>
    `;
    body.appendChild(promptCard);
  }

  const toolList = document.createElement('div');
  toolList.className = 'subagent-tool-list';
  body.appendChild(toolList);

  // Result / stats block — filled on terminal event.
  const summary = document.createElement('div');
  summary.className = 'subagent-summary hidden';
  body.appendChild(summary);

  panel.appendChild(header);
  panel.appendChild(body);

  const currentRow = state.currentMsg ? state.currentMsg.closest('.msg-row') : null;
  const wrapper = wrapInTimeline(panel, 'subagent');
  if (currentRow) {
    dom.chat.insertBefore(wrapper, currentRow);
  } else {
    dom.chat.appendChild(wrapper);
  }
  pinReactStatusToBottom();
  animatePanelIn(panel);
  scrollDown();

  state.activeSubagentPanels.set(panelKey({ task_id: taskId, agent: agentName }), panel);
  syncPanelActions(panel);
}

/**
 * Append a mini tool row inside the sub-agent panel.
 * @param {{ task_id?: string, agent: string }} ref
 * @param {string} toolName
 * @param {string} [toolId]
 * @param {string} [toolArgs]
 */
export function addSubagentTool(ref, toolName, toolId, toolArgs = '') {
  const panel = resolvePanel(ref);
  if (!panel) return;

  const toolList = panel.querySelector('.subagent-tool-list');
  if (!toolList) return;

  const formattedArgs = formatDetailText(toolArgs);
  const row = document.createElement('div');
  row.className = 'subagent-tool-row subagent-tool-running';
  if (toolId) row.dataset.toolId = toolId;
  row.dataset.toolName = toolName;
  row.innerHTML = `
    <div class="subagent-tool-summary" data-action="toggle-tool">
      <span class="subagent-tool-icon">⚡</span>
      <span class="subagent-tool-main">
        <span class="subagent-tool-name">${escHtml(toolName)}</span>
        <span class="subagent-tool-preview">${escHtml(inlinePreview(formattedArgs || '无参数'))}</span>
      </span>
      <span class="subagent-tool-status">执行中</span>
      <span class="chevron">▸</span>
    </div>
    <div class="subagent-tool-details">
      <div class="subagent-tool-section">
        <div class="subagent-tool-section-title">参数</div>
        <pre class="subagent-tool-code">${escHtml(formattedArgs || '无参数')}</pre>
      </div>
      <div class="subagent-tool-section subagent-tool-output hidden">
        <div class="subagent-tool-section-title">输出</div>
        <pre class="subagent-tool-code subagent-tool-output-code"></pre>
      </div>
    </div>
  `;
  toolList.appendChild(row);
  syncToolCount(panel);
  syncPanelActions(panel);
  scrollDown();
}

/**
 * Update the sub-agent panel header with current cycle number.
 * @param {{ task_id?: string, agent: string }} ref
 * @param {number} cycle
 */
export function updateSubagentProgress(ref, cycle) {
  const panel = resolvePanel(ref);
  if (!panel) return;

  const status = panel.querySelector('.subagent-status');
  if (status) {
    status.textContent = `执行中 (cycle ${cycle})`;
  }
  setChipText(panel, 'cycle', `Cycle ${cycle}`);
}

/**
 * Update a tool row inside the sub-agent panel when its result arrives.
 * @param {{ task_id?: string, agent: string }} ref
 * @param {string} toolId
 * @param {number} [durationMs]
 * @param {string} [result]
 * @param {boolean} [isError]
 * @param {string} [toolName]
 */
export function updateSubagentToolResult(ref, toolId, durationMs, result, isError = false, toolName = '') {
  const panel = resolvePanel(ref);
  if (!panel) return;

  let row = null;
  for (const candidate of panel.querySelectorAll('.subagent-tool-row')) {
    if (candidate.dataset.toolId === toolId) {
      row = candidate;
      break;
    }
  }

  if (!row) {
    addSubagentTool(ref, toolName || 'tool', toolId);
    row = Array.from(panel.querySelectorAll('.subagent-tool-row')).find((candidate) => candidate.dataset.toolId === toolId) || null;
  }
  if (!row) return;

  row.classList.remove('subagent-tool-running');
  row.classList.add('subagent-tool-done');
  row.classList.toggle('subagent-tool-failed', isError);

  const statusEl = row.querySelector('.subagent-tool-status');
  if (statusEl) {
    const label = formatToolDuration(durationMs);
    statusEl.textContent = `${isError ? '失败' : '完成'}${label ? ` · ${label}` : ''}`;
  }

  const hasResult = typeof result === 'string';
  const formattedResult = hasResult ? formatDetailText(result) : '';
  const previewEl = row.querySelector('.subagent-tool-preview');
  if (previewEl && formattedResult) {
    previewEl.textContent = inlinePreview(formattedResult, 120);
  }

  const outputSection = row.querySelector('.subagent-tool-output');
  const outputCode = row.querySelector('.subagent-tool-output-code');
  if (outputSection && outputCode && (hasResult || isError)) {
    outputCode.textContent = formattedResult || (isError ? '工具执行失败，未返回可展示的输出。' : '无输出');
    outputSection.classList.remove('hidden');
  }

  syncToolCount(panel);
  syncPanelActions(panel);

  if (isError) {
    setToolRowExpanded(row, true);
    pulseFocus(row);
  }

  scrollDown();
}

/**
 * Mark a sub-agent panel as completed or failed and auto-collapse after delay.
 * @param {{ task_id?: string, agent: string }} ref
 * @param {boolean} success
 * @param {{ cycles?: number, tool_calls?: number, duration_ms?: number, input_tokens?: number, output_tokens?: number, error?: string, result_preview?: string, result_excerpt?: string }} stats
 */
export function finishSubagentPanel(ref, success, stats, { immediate = false } = {}) {
  const panel = resolvePanel(ref);
  if (!panel) return;

  panel.classList.remove('subagent-active');
  panel.classList.add(success ? 'subagent-done' : 'subagent-failed');

  const status = panel.querySelector('.subagent-status');
  if (status) {
    if (success) {
      const parts = [];
      if (stats.cycles != null) parts.push(`${stats.cycles} cycles`);
      if (stats.tool_calls != null) parts.push(`${stats.tool_calls} tools`);
      if (stats.duration_ms != null) {
        const dur = formatToolDuration(stats.duration_ms);
        if (dur) parts.push(dur);
      }
      status.textContent = parts.length ? `完成 (${parts.join(', ')})` : '完成';
    } else {
      status.textContent = stats.error ? `失败: ${stats.error.slice(0, 60)}` : '失败';
    }
  }

  setChipText(panel, 'state', success ? '已完成' : '失败', success ? 'is-success' : 'is-error');
  if (stats.cycles != null) setChipText(panel, 'cycle', `Cycle ${stats.cycles}`);
  if (stats.tool_calls != null) setChipText(panel, 'tools', `${stats.tool_calls} tools`);

  renderSummary(panel, success, stats);
  syncPanelActions(panel);

  const collapsePanel = () => {
    const body = panel.querySelector('.subagent-body');
    const chevron = panel.querySelector('.chevron');
    if (body) animateCollapsibleSection(body, false);
    if (chevron) chevron.classList.remove('open');
  };

  if (immediate) {
    collapsePanel();
  }

  state.activeSubagentPanels.delete(panelKey(ref));
  // Also drop the legacy agent-name key if it was used as a fallback, so
  // parallel later tasks for the same agent start with a clean slate.
  if (ref && ref.agent) state.activeSubagentPanels.delete(ref.agent);
}

export function toggleSubagentTools(button) {
  const panel = button.closest('.subagent-panel');
  if (!panel) return;

  const rows = getToolRows(panel);
  if (rows.length === 0) return;
  const shouldExpand = rows.some((row) => !row.querySelector('.subagent-tool-details')?.classList.contains('show'));
  rows.forEach((row) => setToolRowExpanded(row, shouldExpand));
  syncPanelActions(panel);
}

export function focusSubagentCurrent(button) {
  const panel = button.closest('.subagent-panel');
  if (!panel) return;

  const target = findPriorityToolRow(panel);
  if (!target) return;
  focusToolRow(target);
}

export function copySubagentSummary(button) {
  const panel = button.closest('.subagent-panel');
  if (!panel) return;
  void copyButtonText(button, summaryCopyText(panel), '复制摘要');
}
