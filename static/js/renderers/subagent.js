import { dom, state } from '../state.js';
import { escHtml, formatToolDuration, formatTokenCount } from '../utils.js';
import { scrollDown } from '../scroll.js';
import { wrapInTimeline, animatePanelIn, animateCollapsibleSection } from './timeline.js';
import { pinReactStatusToBottom } from './react-status.js';

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
    <span class="subagent-icon">🤖</span>
    <span class="subagent-label">${escHtml(agentName)}</span>
    <span class="subagent-status">执行中</span>
    <span class="chevron open">▸</span>
  `;

  const body = document.createElement('div');
  body.className = 'subagent-body show';

  if (prompt) {
    const promptRow = document.createElement('div');
    promptRow.className = 'subagent-prompt';
    promptRow.textContent = prompt.length > 200 ? prompt.slice(0, 200) + '…' : prompt;
    body.appendChild(promptRow);
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
}

/**
 * Append a mini tool row inside the sub-agent panel.
 * @param {{ task_id?: string, agent: string }} ref
 * @param {string} toolName
 * @param {string} [toolId]
 */
export function addSubagentTool(ref, toolName, toolId) {
  const panel = resolvePanel(ref);
  if (!panel) return;

  const toolList = panel.querySelector('.subagent-tool-list');
  if (!toolList) return;

  const row = document.createElement('div');
  row.className = 'subagent-tool-row';
  if (toolId) row.dataset.toolId = toolId;
  row.innerHTML = `
    <span class="subagent-tool-icon">⚡</span>
    <span class="subagent-tool-name">${escHtml(toolName)}</span>
    <span class="subagent-tool-status">执行中</span>
  `;
  toolList.appendChild(row);
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
}

/**
 * Update a tool row inside the sub-agent panel when its result arrives.
 * @param {{ task_id?: string, agent: string }} ref
 * @param {string} toolId
 * @param {number} [durationMs]
 */
export function updateSubagentToolResult(ref, toolId, durationMs) {
  const panel = resolvePanel(ref);
  if (!panel) return;

  const rows = panel.querySelectorAll('.subagent-tool-row');
  for (const row of rows) {
    if (row.dataset.toolId === toolId) {
      row.classList.add('subagent-tool-done');
      const statusEl = row.querySelector('.subagent-tool-status');
      if (statusEl) {
        const label = formatToolDuration(durationMs);
        statusEl.textContent = label || '✓';
      }
      return;
    }
  }
}

/**
 * Mark a sub-agent panel as completed or failed and auto-collapse after delay.
 * @param {{ task_id?: string, agent: string }} ref
 * @param {boolean} success
 * @param {{ cycles?: number, tool_calls?: number, duration_ms?: number, input_tokens?: number, output_tokens?: number, error?: string, result_preview?: string }} stats
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

  // Populate the summary block with tokens / preview / error so users can
  // see at a glance what each sub-agent produced and what it cost.
  const summary = panel.querySelector('.subagent-summary');
  if (summary) {
    const lines = [];
    const tokenParts = [];
    if (stats.input_tokens != null) {
      tokenParts.push(`in ${formatTokenCount(stats.input_tokens)}`);
    }
    if (stats.output_tokens != null) {
      tokenParts.push(`out ${formatTokenCount(stats.output_tokens)}`);
    }
    if (tokenParts.length) {
      lines.push(`<div class="subagent-tokens">Tokens: ${escHtml(tokenParts.join(' · '))}</div>`);
    }
    if (!success && stats.error) {
      lines.push(`<div class="subagent-error">${escHtml(stats.error)}</div>`);
    } else if (success && stats.result_preview) {
      lines.push(`<div class="subagent-preview">${escHtml(stats.result_preview)}</div>`);
    }
    if (lines.length) {
      summary.innerHTML = lines.join('');
      summary.classList.remove('hidden');
    }
  }

  const collapsePanel = () => {
    const body = panel.querySelector('.subagent-body');
    const chevron = panel.querySelector('.chevron');
    if (body) animateCollapsibleSection(body, false);
    if (chevron) chevron.classList.remove('open');
  };

  if (immediate) {
    collapsePanel();
  } else {
    setTimeout(collapsePanel, 600);
  }

  state.activeSubagentPanels.delete(panelKey(ref));
  // Also drop the legacy agent-name key if it was used as a fallback, so
  // parallel later tasks for the same agent start with a clean slate.
  if (ref && ref.agent) state.activeSubagentPanels.delete(ref.agent);
}
