import { dom, state, initDomRefs } from './state.js';
import { HISTORY_RENDER_LIMIT } from './constants.js';
import { escHtml, formatToolDuration, formatTokenCount, hideWelcome } from './utils.js';
import {
  syncToolDrawerBounds, clearBufferedChatUpdates, setAutoFollowChat,
  scrollDown, syncChatScrollState, jumpToLatest, updateJumpToLatestVisibility
} from './scroll.js';
import { wrapInTimeline, animatePanelIn, removeTimelinePanel, animateCollapsibleSection } from './renderers/timeline.js';
import {
  addMsg, addSystem, addError, renderUserImageThumbnails,
  showWelcome, setBusy, loadAppVersion
} from './renderers/chat.js';
import {
  pinReactStatusToBottom, clearReactStatus, showReactStatus,
  setReactActTool, requestClearReactStatus, renderReactStatus
} from './renderers/react-status.js';
import {
  addToolCall, updateToolProgress, addToolResult,
  openToolDrawerFromHeader, closeToolDrawer, toggleTool
} from './renderers/tools.js';
import { scheduleMarkdownRender } from './markdown.js';
import {
  beginAssistantStream, finishAssistantStream, finishReasoningStream, scheduleFlush
} from './handlers/stream.js';
import { connect } from './socket.js';
import {
  ensureUploadTokenInternal, updateAttachButton,
  dropUnavailablePendingUploads, renderImagePreviews, initImageListeners
} from './images.js';
import { sendCmd, send, stopAgent, initInputListeners } from './input.js';
import { toggleMobileMenu, closeMobileMenu, initMobileListeners } from './mobile.js';
import {
  createSubagentPanel, addSubagentTool, updateSubagentProgress,
  updateSubagentToolResult, finishSubagentPanel
} from './renderers/subagent.js';

// ── Initialize DOM ──
initDomRefs();

// ── Markdown setup ──
marked.setOptions({
  highlight: (code, lang) => {
    if (lang && hljs.getLanguage(lang)) {
      return hljs.highlight(code, { language: lang }).value;
    }
    return hljs.highlightAuto(code).value;
  },
  breaks: true,
});

// ── View toggles ──

function updateViewToggleButtons() {
  if (dom.toggleToolsBtn) {
    dom.toggleToolsBtn.textContent = `Tools: ${state.showTools ? 'On' : 'Off'}`;
    dom.toggleToolsBtn.classList.toggle('is-active', state.showTools);
  }
  if (dom.toggleReasoningBtn) {
    dom.toggleReasoningBtn.textContent = `Reasoning: ${state.showReasoning ? 'On' : 'Off'}`;
    dom.toggleReasoningBtn.classList.toggle('is-active', state.showReasoning);
  }
}

function applyViewState(viewState) {
  if (!viewState) return;

  if (typeof viewState.show_tools === 'boolean') {
    state.showTools = viewState.show_tools;
    if (!state.showTools) {
      closeToolDrawer();
      state.activeToolPanel = null;
      for (const panel of dom.chat.querySelectorAll('.tool-panel, .subagent-panel')) {
        removeTimelinePanel(panel);
      }
      state.activeSubagentPanels.clear();
    }
  }

  if (typeof viewState.show_reasoning === 'boolean') {
    state.showReasoning = viewState.show_reasoning;
    if (!state.showReasoning) {
      finishReasoningStream();
      if (state.reasoningPanel) removeTimelinePanel(state.reasoningPanel);
      state.reasoningPanel = null;
    }
  }

  updateViewToggleButtons();
}

function toggleToolsVisibility() {
  if (!state.ws || state.ws.readyState !== 1) return;
  const nextShowTools = !state.showTools;
  applyViewState({ show_tools: nextShowTools });
  sendCmd(`/tool ${nextShowTools ? 'on' : 'off'}`);
}

function toggleReasoningVisibility() {
  if (!state.ws || state.ws.readyState !== 1) return;
  const nextShowReasoning = !state.showReasoning;
  applyViewState({ show_reasoning: nextShowReasoning });
  sendCmd(`/reasoning ${nextShowReasoning ? 'on' : 'off'}`);
}

// ── Usage badge ──

function updateUsageBadge() {
  if (!dom.usageBadge) return;
  const inp = state.dailyInputTokens;
  const out = state.dailyOutputTokens;
  if (inp === 0 && out === 0) {
    dom.usageBadge.textContent = '';
    return;
  }
  dom.usageBadge.textContent = `📊 ${formatTokenCount(inp)} in / ${formatTokenCount(out)} out`;
  dom.usageBadge.title = `今日: ${formatTokenCount(inp)} input, ${formatTokenCount(out)} output\n累计: ${formatTokenCount(state.totalInputTokens)} input, ${formatTokenCount(state.totalOutputTokens)} output`;
}

function appendRoundUsage(messageEl, inputTokens, outputTokens) {
  const lastAssistantRow = messageEl ? messageEl.closest('.msg-row') : null;
  if (!lastAssistantRow) return;
  const content = lastAssistantRow.querySelector('.msg-content');
  if (!content) return;
  if (content.querySelector('.msg-usage')) return;
  const label = document.createElement('div');
  label.className = 'msg-usage';
  label.textContent = `${formatTokenCount(inputTokens)} in / ${formatTokenCount(outputTokens)} out`;
  label.title = `Input: ${inputTokens.toLocaleString()} tokens, Output: ${outputTokens.toLocaleString()} tokens`;
  content.appendChild(label);
}

// ── History lazy-load ──

function findHistoryRenderStart(messages, preferredStart) {
  let startIdx = Math.max(0, preferredStart);
  if (startIdx === 0) {
    return 0;
  }

  const toolCallById = new Map();
  for (let i = 0; i < messages.length; i++) {
    const message = messages[i];
    if (message.role === 'tool_call' && message.id) {
      toolCallById.set(message.id, i);
    }
  }

  let expanded = true;
  while (expanded) {
    expanded = false;
    for (let i = startIdx; i < messages.length; i++) {
      const message = messages[i];
      if (message.role !== 'tool_result' || !message.id) {
        continue;
      }

      const callIdx = toolCallById.get(message.id);
      if (callIdx !== undefined && callIdx < startIdx) {
        startIdx = callIdx;
        expanded = true;
        break;
      }
    }
  }

  return startIdx;
}

function renderHistoryMessage(m, options = {}) {
  const { followMarkdown = true } = options;
  switch (m.role) {
    case 'user': {
      const el = addMsg('user', m.content, m.timestamp);
      if (m.images && m.images.length > 0) renderUserImageThumbnails(el, m.images);
      break;
    }
    case 'assistant': {
      const el = addMsg('assistant', m.content, m.timestamp);
      el._rawText = m.content;
      scheduleMarkdownRender(el, { followScroll: followMarkdown });
      break;
    }
    case 'tool_call': {
      if (!state.showTools) break;
      if (m.name === 'task') {
        try {
          const args = JSON.parse(m.arguments || '{}');
          createSubagentPanel(args.agent || 'sub-agent', args.prompt || '');
          if (!state._historyTaskIds) state._historyTaskIds = new Map();
          state._historyTaskIds.set(m.id, args.agent || 'sub-agent');
        } catch { addToolCall(m.name, m.arguments, m.id); }
        break;
      }
      addToolCall(m.name, m.arguments, m.id);
      break;
    }
    case 'tool_result': {
      if (!state.showTools) break;
      if (state._historyTaskIds && state._historyTaskIds.has(m.id)) {
        const agentName = state._historyTaskIds.get(m.id);
        state._historyTaskIds.delete(m.id);
        const r = (m.result || '').trimStart();
        const failed = m.is_error === true
          || r.startsWith('task error:')
          || r.startsWith('[rejected')
          || /^Sub-agent '.+' (failed|timed out)/.test(r);
        finishSubagentPanel(agentName, !failed, {}, { immediate: true });
        break;
      }
      addToolResult('', m.result, m.id);
      break;
    }
  }
}

function loadEarlierMessages() {
  const msgs = state.deferredHistory;
  state.deferredHistory = [];
  const loadMoreRow = document.getElementById('load-more-row');
  const anchor = loadMoreRow ? loadMoreRow.nextElementSibling : dom.chat.firstElementChild;
  if (loadMoreRow) loadMoreRow.remove();
  const existing = [...dom.chat.children];
  dom.chat.replaceChildren();
  dom.chat.classList.add('no-animate');
  state.bulkRenderingChat = true;
  for (const m of msgs) renderHistoryMessage(m, { followMarkdown: false });
  for (const el of existing) dom.chat.appendChild(el);
  requestAnimationFrame(() => {
    state.bulkRenderingChat = false;
    dom.chat.classList.remove('no-animate');
    if (anchor) anchor.scrollIntoView({ block: 'start' });
    requestAnimationFrame(syncChatScrollState);
  });
}

// ── handleMessage ──

function handleMessage(data) {
  switch (data.type) {
    case 'session':
      state.currentSessionId = data.id;
      dom.sessionNameEl.textContent = data.name || 'Main';
      dom.sessionIdEl.textContent = data.id.slice(0, 12);
      if (data.capabilities && typeof data.capabilities.image === 'boolean') {
        state.imageCapable = data.capabilities.image;
        updateAttachButton();
      }
      if (data.capabilities && typeof data.capabilities.s3 === 'boolean') {
        const previousS3Capable = state.s3Capable;
        state.s3Capable = data.capabilities.s3;
        if (state.s3Capable) {
          void ensureUploadTokenInternal(true).catch(() => {});
        } else {
          state.uploadToken = '';
          state.uploadTokenPromise = null;
          dropUnavailablePendingUploads(previousS3Capable);
        }
      }
      if (data.usage) {
        state.dailyInputTokens = data.usage.daily_input ?? 0;
        state.dailyOutputTokens = data.usage.daily_output ?? 0;
        state.totalInputTokens = data.usage.total_input ?? 0;
        state.totalOutputTokens = data.usage.total_output ?? 0;
        updateUsageBadge();
      }
      applyViewState(data);
      break;

    case 'history': {
      if (!state.showTools) {
        data.messages = (data.messages || []).filter(m => m.role !== 'tool_call' && m.role !== 'tool_result');
      }
      closeToolDrawer();
      clearReactStatus();
      clearBufferedChatUpdates();
      setAutoFollowChat(true);
      dom.chat.innerHTML = '';
      state.deferredHistory = [];
      state._historyTaskIds = null;
      const msgs = data.messages || [];
      if (msgs.length === 0) {
        showWelcome();
      } else {
        dom.chat.classList.add('no-animate');
        state.bulkRenderingChat = true;
        let startIdx = 0;
        if (msgs.length > HISTORY_RENDER_LIMIT) {
          startIdx = findHistoryRenderStart(msgs, msgs.length - HISTORY_RENDER_LIMIT);
          state.deferredHistory = msgs.slice(0, startIdx);
          const loadMoreRow = document.createElement('div');
          loadMoreRow.className = 'msg-row system';
          loadMoreRow.id = 'load-more-row';
          const btn = document.createElement('button');
          btn.className = 'load-more-btn';
          btn.dataset.action = 'load-earlier';
          btn.textContent = `\u2191 \u52a0\u8f7d\u66f4\u65e9\u7684\u6d88\u606f (${state.deferredHistory.length} \u6761)`;
          loadMoreRow.appendChild(btn);
          dom.chat.appendChild(loadMoreRow);
        }
        for (let i = startIdx; i < msgs.length; i++) {
          renderHistoryMessage(msgs[i]);
        }
        if (state._historyTaskIds && state._historyTaskIds.size > 0) {
          for (const name of state._historyTaskIds.values()) {
            finishSubagentPanel(name, false, {}, { immediate: true });
          }
          state._historyTaskIds = null;
        }
        requestAnimationFrame(() => {
          state.bulkRenderingChat = false;
          dom.chat.classList.remove('no-animate');
          scrollDown(true);
        });
      }
      break;
    }

    case 'view_state':
      applyViewState(data);
      break;

    case 'start':
      setBusy(true);
      finishAssistantStream({ discardIfEmpty: true });
      beginAssistantStream();
      if (data.react_visible && data.phase) {
        showReactStatus(data.phase, data.cycle);
      }
      break;

    case 'delta':
      if (data.subagent) break;
      if (state.currentMsg) {
        state.pendingAssistantText += data.content;
        scheduleFlush();
      }
      break;

    case 'done': {
      const finishedAssistantMsg = finishAssistantStream({ discardIfEmpty: true });
      finishReasoningStream();
      requestClearReactStatus();
      state.reasoningPanel = null;
      if (data.daily_input_tokens != null) {
        state.dailyInputTokens = data.daily_input_tokens;
        state.dailyOutputTokens = data.daily_output_tokens ?? 0;
        state.totalInputTokens = data.total_input_tokens ?? 0;
        state.totalOutputTokens = data.total_output_tokens ?? 0;
        updateUsageBadge();
      }
      if (data.round_input_tokens != null || data.round_output_tokens != null) {
        appendRoundUsage(
          finishedAssistantMsg,
          data.round_input_tokens ?? 0,
          data.round_output_tokens ?? 0,
        );
      }
      setBusy(false);
      break;
    }

    case 'react_phase':
      showReactStatus(data.phase, data.cycle);
      break;

    case 'thinking_start': {
      if (data.subagent) break;
      if (!state.showReasoning) break;
      const panel = document.createElement('div');
      panel.className = 'reasoning-panel reasoning-active';
      const header = document.createElement('div');
      header.className = 'reasoning-header';
      header.dataset.action = 'toggle-tool';
      header.innerHTML = `
          <span class="reasoning-icon">\ud83d\udcad</span>
          <span class="reasoning-label">Reasoning</span>
          <span class="reasoning-status">\u63a8\u7406\u4e2d</span>
          <span class="chevron open">\u25b8</span>
      `;
      const body = document.createElement('div');
      body.className = 'reasoning-body show';
      panel.appendChild(header);
      panel.appendChild(body);
      const currentRow = state.currentMsg ? state.currentMsg.closest('.msg-row') : null;
      const wrapper = wrapInTimeline(panel, 'reasoning');
      if (currentRow) {
        dom.chat.insertBefore(wrapper, currentRow);
      } else {
        dom.chat.appendChild(wrapper);
      }
      pinReactStatusToBottom();
      animatePanelIn(panel);
      state.reasoningPanel = panel;
      hideWelcome();
      scrollDown();
      break;
    }

    case 'thinking_delta':
      if (data.subagent) break;
      if (!state.showReasoning) break;
      if (state.reasoningPanel) {
        state.pendingReasoningText += data.content;
        scheduleFlush();
      }
      break;

    case 'thinking_done':
      if (data.subagent) break;
      if (!state.showReasoning) {
        finishReasoningStream();
        state.reasoningPanel = null;
        break;
      }
      if (state.reasoningPanel) {
        finishReasoningStream();
        state.reasoningPanel.classList.remove('reasoning-active');
        const status = state.reasoningPanel.querySelector('.reasoning-status');
        const body = state.reasoningPanel.querySelector('.reasoning-body');
        const chevron = state.reasoningPanel.querySelector('.chevron');
        const rawText = body?._textNode?.nodeValue || body?.textContent || '';
        const summaryText = rawText.trim().replace(/\n+/g, ' ');
        const preview = summaryText.substring(0, 60);
        if (status) {
          status.textContent = preview ? preview + (summaryText.length > 60 ? '\u2026' : '') : '\u5b8c\u6210';
          status.title = summaryText || '\u5b8c\u6210';
        }
        setTimeout(() => {
          if (body) animateCollapsibleSection(body, false);
          if (chevron) chevron.classList.remove('open');
        }, 600);
        state.reasoningPanel = null;
      }
      break;

    case 'tool_call':
      if (data.subagent) break;
      setReactActTool(data.name, 0);
      if (!state.showTools) break;
      addToolCall(data.name, data.arguments, data.id);
      break;

    case 'tool_progress':
      if (data.subagent) break;
      setReactActTool(data.name, data.elapsed_ms || 0);
      if (!state.showTools) break;
      updateToolProgress(data.id, data.elapsed_ms || 0);
      break;

    case 'tool_result':
      if (data.subagent) {
        if (state.showTools) updateSubagentToolResult(data.subagent, data.id, data.duration_ms);
        break;
      }
      if (state.reactStatusPhase === 'act' && state.reactStatusToolName === data.name) {
        state.reactStatusElapsedMs = data.duration_ms || state.reactStatusElapsedMs;
        renderReactStatus();
      }
      if (!state.showTools) break;
      addToolResult(data.name, data.result, data.id, data.duration_ms ?? null);
      break;

    case 'task_started':
      if (state.showTools) createSubagentPanel(data.agent, data.prompt);
      break;
    case 'task_progress':
      if (state.showTools) updateSubagentProgress(data.agent, data.cycle);
      break;
    case 'task_tool':
      if (state.showTools) addSubagentTool(data.agent, data.tool, data.id);
      break;
    case 'task_completed':
      if (state.showTools) finishSubagentPanel(data.agent, true, { cycles: data.cycles, tool_calls: data.tool_calls, duration_ms: data.duration_ms });
      break;
    case 'task_failed':
      if (state.showTools) finishSubagentPanel(data.agent, false, { cycles: data.cycles, tool_calls: data.tool_calls, duration_ms: data.duration_ms, error: data.error });
      break;

    case 'context_compressed':
      addSystem(
        `Context auto-compressed: removed ${data.messages_removed || 0} messages, token estimate ${data.before_estimate || 0} -> ${data.after_estimate || 0}`
      );
      break;

    case 'progress':
      addSystem(data.content);
      break;

    case 'success':
      clearReactStatus();
      addSystem(data.content, 'success');
      setBusy(false);
      break;

    case 'system':
      clearReactStatus();
      addSystem(data.content);
      setBusy(false);
      break;

    case 'error':
      finishAssistantStream({ discardIfEmpty: true });
      finishReasoningStream();
      clearReactStatus();
      addError(data.content);
      state.reasoningPanel = null;
      setBusy(false);
      break;
  }
}

// ── Event delegation for data-action buttons ──

const actionHandlers = {
  'toggle-tools': () => toggleToolsVisibility(),
  'toggle-reasoning': () => toggleReasoningVisibility(),
  'cmd': (el) => {
    const cmd = el.dataset.cmd;
    if (cmd) sendCmd(cmd);
  },
  'cmd-close-menu': (el) => {
    const cmd = el.dataset.cmd;
    if (cmd) sendCmd(cmd);
    closeMobileMenu();
  },
  'toggle-mobile-menu': () => toggleMobileMenu(),
  'close-tool-drawer': () => closeToolDrawer(),
  'load-earlier': () => loadEarlierMessages(),
  'open-tool-drawer': (el) => openToolDrawerFromHeader(el),
  'toggle-tool': (el) => toggleTool(el),
};

document.addEventListener('click', (e) => {
  const el = e.target.closest('[data-action]');
  if (!el) return;
  const handler = actionHandlers[el.dataset.action];
  if (handler) handler(el);
});

// ── Init ──
updateViewToggleButtons();
syncToolDrawerBounds();
updateJumpToLatestVisibility();

initImageListeners();
initInputListeners();
initMobileListeners();

document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') {
    closeToolDrawer();
    closeMobileMenu();
  }
});
dom.chat.addEventListener('scroll', () => {
  syncChatScrollState();
}, { passive: true });
window.addEventListener('resize', syncToolDrawerBounds);
window.addEventListener('resize', () => {
  if (window.innerWidth > 768) closeMobileMenu();
});
if (window.visualViewport) {
  window.visualViewport.addEventListener('resize', syncToolDrawerBounds);
  window.visualViewport.addEventListener('scroll', syncToolDrawerBounds);
}
if (dom.jumpToLatestBtn) {
  dom.jumpToLatestBtn.addEventListener('click', () => {
    jumpToLatest();
  });
}

void loadAppVersion();
connect(handleMessage);
