import { dom, state } from '../state.js';
import { escHtml, truncateStr, formatToolDuration, hideWelcome } from '../utils.js';
import { scrollDown, syncToolDrawerBounds } from '../scroll.js';
import { wrapInTimeline, animatePanelIn, animateCollapsibleSection } from './timeline.js';
import { pinReactStatusToBottom } from './react-status.js';
import { tr } from '../i18n.js';

const TOOL_LIVE_OUTPUT_MAX_CHARS = 60000;
const TOOL_LIVE_OUTPUT_TRUNCATED_PREFIX = '[live output truncated]\n';

export function mergeToolLiveOutput(current, stream, chunk, maxChars = TOOL_LIVE_OUTPUT_MAX_CHARS) {
  const prefix = stream === 'stderr' ? '\n[stderr]\n' : '';
  let next = `${current || ''}${prefix}${chunk || ''}`;
  if (next.length <= maxChars) return next;
  next = next.slice(next.length - maxChars);
  return `${TOOL_LIVE_OUTPUT_TRUNCATED_PREFIX}${next}`;
}

function findToolPanel(id) {
  const panels = Array.from(dom.chat.querySelectorAll('.tool-panel'));
  let fallback = null;

  for (let idx = panels.length - 1; idx >= 0; idx -= 1) {
    const panel = panels[idx];
    if (id && panel.dataset.toolId !== id) {
      continue;
    }
    if (!fallback) {
      fallback = panel;
    }
    if (panel.dataset.toolHasResult !== 'true') {
      return panel;
    }
  }

  return fallback;
}

export function addToolCall(name, args, id) {
  let argsDisplay = args;
  try {
    argsDisplay = JSON.stringify(JSON.parse(args), null, 2);
  } catch {}

  const existing = findToolPanel(id);
  if (existing && existing.dataset.toolHasResult !== 'true') {
    existing.dataset.toolId = id;
    existing.dataset.toolName = name;
    existing.dataset.toolArgs = argsDisplay;
    const nameEl = existing.querySelector('.tool-name');
    if (nameEl) nameEl.textContent = name;
    const argsPreviewEl = existing.querySelector('.tool-args-preview');
    if (argsPreviewEl) argsPreviewEl.textContent = truncateStr(args, 80);
    return existing;
  }

  const panel = document.createElement('div');
  panel.className = 'tool-panel';
  panel.dataset.toolId = id;

  panel.dataset.toolName = name;
  panel.dataset.toolArgs = argsDisplay;
  panel.dataset.toolResult = '';
  panel.dataset.toolLiveOutput = '';
  panel.dataset.toolHasResult = 'false';
  panel.dataset.toolStatus = tr('tool.running');

  panel.innerHTML = `
    <div class="tool-header" data-action="open-tool-drawer">
      <span class="tool-icon">⚡</span>
      <span class="tool-name">${escHtml(name)}</span>
      <span class="tool-args-preview">${escHtml(truncateStr(args, 80))}</span>
      <span class="tool-status">${escHtml(tr('tool.running'))}</span>
    </div>
  `;
  const wrapper = wrapInTimeline(panel, 'tool');
  const currentRow = state.currentMsg ? state.currentMsg.closest('.msg-row') : null;
  if (currentRow) {
    // Tool calls are emitted after the assistant has finished streaming its text;
    // insert the card AFTER the current assistant row, not before it.
    currentRow.after(wrapper);
  } else {
    dom.chat.appendChild(wrapper);
  }
  pinReactStatusToBottom();
  animatePanelIn(panel);
  hideWelcome();
  scrollDown();
  return panel;
}

export function updateToolProgress(id, elapsedMs) {
  const panel = findToolPanel(id);
  if (!panel || panel.dataset.toolHasResult === 'true') return;
  const seconds = Math.max(1, Math.floor((elapsedMs || 0) / 1000));
  const statusText = tr('tool.runningWithSeconds', { seconds });
  panel.dataset.toolStatus = statusText;
  const statusEl = panel.querySelector('.tool-status');
  if (statusEl) {
    statusEl.textContent = statusText;
  }
  if (state.activeToolPanel === panel) {
    syncToolDrawer(panel);
  }
}

export function appendToolOutput(id, stream, chunk) {
  const panel = findToolPanel(id);
  if (!panel || panel.dataset.toolHasResult === 'true' || !chunk) return;
  panel.dataset.toolLiveOutput = mergeToolLiveOutput(
    panel.dataset.toolLiveOutput || '',
    stream,
    chunk,
  );
  if (state.activeToolPanel === panel) {
    syncToolDrawer(panel);
  }
}

export function addToolResult(name, result, id, durationMs = null) {
  const panel = findToolPanel(id);
  if (panel) {
    panel.dataset.toolResult = result;
    panel.dataset.toolLiveOutput = '';
    panel.dataset.toolHasResult = 'true';
    const durationLabel = formatToolDuration(durationMs);
    panel.dataset.toolStatus = durationLabel
      ? tr('tool.resultReturnedWithDuration', { duration: durationLabel })
      : tr('tool.resultReturned');
    const statusEl = panel.querySelector('.tool-status');
    if (statusEl) {
      statusEl.textContent = panel.dataset.toolStatus;
    }
    panel.classList.add('tool-panel-ready');
    if (state.activeToolPanel === panel) {
      syncToolDrawer(panel);
    }
    return;
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
  el.dataset.toolStatus = durationLabel
    ? tr('tool.resultReturnedWithDuration', { duration: durationLabel })
    : tr('tool.resultReturned');
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
    currentRow.after(wrapper);
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
  const toolLiveOutput = panel.dataset.toolLiveOutput || '';
  const hasResult = panel.dataset.toolHasResult === 'true';
  const detailText = hasResult ? toolResult : toolLiveOutput;
  const hasDetail = detailText.trim().length > 0;
  const statusText =
    panel.dataset.toolStatus || (hasResult ? tr('tool.resultReturned') : tr('tool.running'));

  if (dom.toolDrawerTitle) dom.toolDrawerTitle.textContent = toolName;
  if (dom.toolDrawerMeta) dom.toolDrawerMeta.textContent = statusText;
  if (dom.toolDrawerArgs) dom.toolDrawerArgs.textContent = toolArgs || tr('tool.argumentsEmpty');
  if (dom.toolDrawerResult) dom.toolDrawerResult.textContent = detailText;
  if (dom.toolDrawerResultSection) dom.toolDrawerResultSection.hidden = !hasDetail;
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
