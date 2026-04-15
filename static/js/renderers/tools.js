import { dom, state } from '../state.js';
import { escHtml, truncateStr, formatToolDuration, hideWelcome } from '../utils.js';
import { scrollDown, syncToolDrawerBounds } from '../scroll.js';
import { wrapInTimeline, animatePanelIn, animateCollapsibleSection } from './timeline.js';
import { pinReactStatusToBottom } from './react-status.js';

export function addToolCall(name, args, id) {
  const panel = document.createElement('div');
  panel.className = 'tool-panel';
  panel.dataset.toolId = id;

  let argsDisplay = args;
  try { argsDisplay = JSON.stringify(JSON.parse(args), null, 2); } catch(e) {}
  panel.dataset.toolName = name;
  panel.dataset.toolArgs = argsDisplay;
  panel.dataset.toolResult = '';
  panel.dataset.toolHasResult = 'false';
  panel.dataset.toolStatus = '执行中';

  panel.innerHTML = `
    <div class="tool-header" data-action="open-tool-drawer">
      <span class="tool-icon">⚡</span>
      <span class="tool-name">${escHtml(name)}</span>
      <span class="tool-args-preview">${escHtml(truncateStr(args, 80))}</span>
      <span class="tool-status">执行中</span>
    </div>
  `;
  const wrapper = wrapInTimeline(panel, 'tool');
  const currentRow = state.currentMsg ? state.currentMsg.closest('.msg-row') : null;
  if (currentRow) {
    dom.chat.insertBefore(wrapper, currentRow);
  } else {
    dom.chat.appendChild(wrapper);
  }
  pinReactStatusToBottom();
  animatePanelIn(panel);
  hideWelcome();
  scrollDown();
}

export function updateToolProgress(id, elapsedMs) {
  if (!id) return;
  const seconds = Math.max(1, Math.floor((elapsedMs || 0) / 1000));
  for (const panel of dom.chat.querySelectorAll('.tool-panel')) {
    if (panel.dataset.toolId !== id || panel.dataset.toolHasResult === 'true') {
      continue;
    }
    const statusText = `执行中 ${seconds}s`;
    panel.dataset.toolStatus = statusText;
    const statusEl = panel.querySelector('.tool-status');
    if (statusEl) {
      statusEl.textContent = statusText;
    }
    if (state.activeToolPanel === panel) {
      syncToolDrawer(panel);
    }
    return;
  }
}

export function addToolResult(name, result, id, durationMs = null) {
  const panels = dom.chat.querySelectorAll('.tool-panel');
  for (const p of panels) {
    if (p.dataset.toolId === id) {
      p.dataset.toolResult = result;
      p.dataset.toolHasResult = 'true';
      const durationLabel = formatToolDuration(durationMs);
      p.dataset.toolStatus = durationLabel ? `已返回结果 (${durationLabel})` : '已返回结果';
      const statusEl = p.querySelector('.tool-status');
      if (statusEl) {
        statusEl.textContent = p.dataset.toolStatus;
      }
      p.classList.add('tool-panel-ready');
      if (state.activeToolPanel === p) {
        syncToolDrawer(p);
      }
      return;
    }
  }
  // Fallback: standalone result
  const el = document.createElement('div');
  el.className = 'tool-panel tool-result';
  el.dataset.toolId = id || '';
  el.dataset.toolName = name ? `${name} result` : 'Tool result';
  el.dataset.toolArgs = '';
  el.dataset.toolResult = result;
  el.dataset.toolHasResult = 'true';
  const durationLabel = formatToolDuration(durationMs);
  el.dataset.toolStatus = durationLabel ? `已返回结果 (${durationLabel})` : '已返回结果';
  el.innerHTML = `
    <div class="tool-header" data-action="open-tool-drawer">
      <span class="tool-icon">📋</span>
      <span class="tool-name">${escHtml(name)} result</span>
      <span class="tool-status">${escHtml(el.dataset.toolStatus)}</span>
    </div>
  `;
  el.classList.add('tool-panel-ready');
  const wrapper = wrapInTimeline(el, 'result');
  const currentRow = state.currentMsg ? state.currentMsg.closest('.msg-row') : null;
  if (currentRow) {
    dom.chat.insertBefore(wrapper, currentRow);
  } else {
    dom.chat.appendChild(wrapper);
  }
  pinReactStatusToBottom();
  animatePanelIn(el);
  scrollDown();
}

export function syncToolDrawer(panel) {
  if (!panel || !dom.toolDrawer) return;
  const toolName = panel.dataset.toolName || 'Tool';
  const toolArgs = panel.dataset.toolArgs || '';
  const toolResult = panel.dataset.toolResult || '';
  const hasResult = panel.dataset.toolHasResult === 'true';
  const statusText = panel.dataset.toolStatus || (hasResult ? '已返回结果' : '执行中');

  dom.toolDrawerTitle.textContent = toolName;
  dom.toolDrawerMeta.textContent = statusText;
  dom.toolDrawerArgs.textContent = toolArgs || '(empty)';
  dom.toolDrawerResult.textContent = toolResult;
  dom.toolDrawerResultSection.hidden = !hasResult;
}

export function openToolDrawer(panel) {
  if (!panel || !dom.toolDrawer || !dom.toolDrawerBackdrop) return;
  syncToolDrawerBounds();
  if (state.activeToolPanel && state.activeToolPanel !== panel) {
    state.activeToolPanel.classList.remove('tool-panel-active');
  }
  state.activeToolPanel = panel;
  state.activeToolPanel.classList.add('tool-panel-active');
  syncToolDrawer(panel);
  dom.toolDrawer.classList.add('open');
  dom.toolDrawerBackdrop.classList.add('open');
  dom.toolDrawer.setAttribute('aria-hidden', 'false');
}

export function openToolDrawerFromHeader(header) {
  openToolDrawer(header.closest('.tool-panel'));
}

export function closeToolDrawer() {
  if (!dom.toolDrawer || !dom.toolDrawerBackdrop) return;
  dom.toolDrawer.classList.remove('open');
  dom.toolDrawerBackdrop.classList.remove('open');
  dom.toolDrawer.setAttribute('aria-hidden', 'true');
  if (state.activeToolPanel) {
    state.activeToolPanel.classList.remove('tool-panel-active');
    state.activeToolPanel = null;
  }
}

export function toggleTool(header) {
  const chevron = header.querySelector('.chevron');
  const body = header.nextElementSibling;
  const nextOpen = !body.classList.contains('show');
  if (chevron) chevron.classList.toggle('open', nextOpen);
  animateCollapsibleSection(body, nextOpen);
}
