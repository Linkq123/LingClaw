import { dom, state } from '../state.js';
import { escHtml, formatToolDuration } from '../utils.js';
import { scrollDown } from '../scroll.js';
import { wrapInTimeline, animatePanelIn, animateCollapsibleSection } from './timeline.js';
import { pinReactStatusToBottom } from './react-status.js';

/**
 * Create a collapsible sub-agent panel and insert it into the chat timeline.
 * @param {string} agentName
 * @param {string} [prompt]
 */
export function createSubagentPanel(agentName, prompt) {
  const panel = document.createElement('div');
  panel.className = 'subagent-panel subagent-active';
  panel.dataset.agent = agentName;

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
    promptRow.textContent = prompt.length > 120 ? prompt.slice(0, 120) + '…' : prompt;
    body.appendChild(promptRow);
  }

  const toolList = document.createElement('div');
  toolList.className = 'subagent-tool-list';
  body.appendChild(toolList);

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

  state.activeSubagentPanels.set(agentName, panel);
}

/**
 * Append a mini tool row inside the sub-agent panel.
 * @param {string} agentName
 * @param {string} toolName
 * @param {string} [toolId]
 */
export function addSubagentTool(agentName, toolName, toolId) {
  const panel = state.activeSubagentPanels.get(agentName);
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
 * @param {string} agentName
 * @param {number} cycle
 */
export function updateSubagentProgress(agentName, cycle) {
  const panel = state.activeSubagentPanels.get(agentName);
  if (!panel) return;

  const status = panel.querySelector('.subagent-status');
  if (status) {
    status.textContent = `执行中 (cycle ${cycle})`;
  }
}

/**
 * Update a tool row inside the sub-agent panel when its result arrives.
 * @param {string} agentName
 * @param {string} toolId
 * @param {number} [durationMs]
 */
export function updateSubagentToolResult(agentName, toolId, durationMs) {
  const panel = state.activeSubagentPanels.get(agentName);
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
 * @param {string} agentName
 * @param {boolean} success
 * @param {{ cycles?: number, tool_calls?: number, duration_ms?: number, error?: string }} stats
 */
export function finishSubagentPanel(agentName, success, stats, { immediate = false } = {}) {
  const panel = state.activeSubagentPanels.get(agentName);
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

  state.activeSubagentPanels.delete(agentName);
}
