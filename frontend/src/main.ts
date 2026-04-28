// CSS imports. The highlight.js stylesheet is injected dynamically by
// `theme.ts` so that an explicit user theme choice overrides the system
// setting; we no longer import it statically here.
import './css/base.css';
import './css/layout.css';
import './css/chat.css';
import './css/panels.css';
import './css/pages.css';
import './css/responsive.css';

import { initTheme, cycleTheme, disposeTheme } from './theme.js';
import { dom, initDomRefs, state } from './state.js';
import { HISTORY_LOAD_CHUNK_SIZE, HISTORY_RENDER_LIMIT } from './constants.js';
import { findHistoryRenderStart, splitHistoryLoadChunk } from './historyWindow.js';
import {
  formatTokenCount,
  formatToolDuration,
  hideWelcome,
  scheduleBackgroundTask,
} from './utils.js';
import {
  syncToolDrawerBounds,
  cancelToolDrawerBoundsSync,
  invalidateChatScrollCache,
  clearBufferedChatUpdates,
  setAutoFollowChat,
  scrollDown,
  syncChatScrollState,
  jumpToLatest,
  updateJumpToLatestVisibility,
} from './scroll.js';
import {
  wrapInTimeline,
  animatePanelIn,
  removeTimelinePanel,
  animateCollapsibleSection,
} from './renderers/timeline.js';
import {
  addMsg,
  addSystem,
  addError,
  renderUserImageThumbnails,
  showWelcome,
  setBusy,
  loadAppVersion,
} from './renderers/chat.js';
import {
  pinReactStatusToBottom,
  clearReactStatus,
  showReactStatus,
  setReactActTool,
  requestClearReactStatus,
  renderReactStatus,
} from './renderers/react-status.js';
import {
  addToolCall,
  updateToolProgress,
  addToolResult,
  openToolDrawerFromHeader,
  closeToolDrawer,
  toggleTool,
} from './renderers/tools.js';
import { preloadMarkdownEngine, scheduleMarkdownRender } from './markdown.js';
import {
  beginAssistantStream,
  finishAssistantStream,
  finishReasoningStream,
  scheduleFlush,
} from './handlers/stream.js';
import { connect, cancelReconnect } from './socket.js';
import {
  ensureUploadTokenInternal,
  updateAttachButton,
  dropUnavailablePendingUploads,
  initImageListeners,
} from './images.js';
import { sendCmd, initInputListeners } from './input.js';
import { toggleMobileMenu, closeMobileMenu, initMobileListeners } from './mobile.js';
import { applyToolsVisibility } from './viewState.js';
import {
  createSubagentPanel,
  addSubagentTool,
  updateSubagentProgress,
  updateSubagentToolResult,
  finishSubagentPanel,
  startSubagentReasoning,
  appendSubagentReasoning,
  finishSubagentReasoning,
  restoreSubagentHistorySnapshot,
  copySubagentSummary,
  openSubagentModal,
  closeSubagentModal,
  openSubagentToolDrawer,
} from './renderers/subagent.js';
import {
  createOrchestratePanel,
  updateOrchestrateLayer,
  markOrchestrateTask,
  finishOrchestratePanel,
  openOrchestrateTaskModal,
  closeOrchestrateTaskModal,
} from './renderers/orchestrate.js';
import {
  openSettingsPage,
  closeSettingsPage,
  openUsagePage,
  closeUsagePage,
  prefetchPageChunks,
} from './pages/lazy.js';
import { closeOverlayById } from './pages/overlay.js';
import {
  buildHistoryReasoningPanel,
  finalizeOrDiscardLiveReasoningPanel,
} from './renderers/reasoning.js';

// ── Initialize DOM ──
initDomRefs();

// React islands (Settings & Usage) are now code-split and mounted lazily on
// first `openSettingsPage()` / `openUsagePage()` call. We also prefetch them
// during idle time so the first open is instant.

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
    applyToolsVisibility(viewState.show_tools, {
      state,
      chat: dom.chat,
      closeToolDrawer,
      closeSubagentModal,
      closeOrchestrateTaskModal,
    });
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

function appendRoundUsage(messageEl, inputTokens, outputTokens, firstTokenMs = null) {
  const lastAssistantRow = messageEl ? messageEl.closest('.msg-row') : null;
  if (!lastAssistantRow) return;
  const content = lastAssistantRow.querySelector('.msg-content');
  if (!content) return;
  if (content.querySelector('.msg-usage')) return;
  const label = document.createElement('div');
  label.className = 'msg-usage';
  const parts = [`${formatTokenCount(inputTokens)} in / ${formatTokenCount(outputTokens)} out`];
  if (firstTokenMs != null) parts.push(`首 token ${formatToolDuration(firstTokenMs)}`);
  label.replaceChildren(
    ...parts.map((part) => {
      const item = document.createElement('span');
      item.textContent = part;
      return item;
    }),
  );
  label.title = [
    `Input: ${inputTokens.toLocaleString()} tokens`,
    `Output: ${outputTokens.toLocaleString()} tokens`,
    firstTokenMs != null ? `First token latency: ${formatToolDuration(firstTokenMs)}` : '',
  ]
    .filter(Boolean)
    .join('\n');
  content.appendChild(label);
}

function markCurrentRoundFirstTokenAt() {
  if (!state.currentRoundStartedAt || state.currentRoundFirstTokenAt) return;
  state.currentRoundFirstTokenAt = performance.now();
}

function resetRoundTimers() {
  state.currentRoundStartedAt = 0;
  state.currentRoundFirstTokenAt = 0;
}

// ── History lazy-load ──

function createLoadMoreRow(count: number): HTMLElement {
  const loadMoreRow = document.createElement('div');
  loadMoreRow.className = 'msg-row system';
  loadMoreRow.id = 'load-more-row';
  const btn = document.createElement('button');
  btn.className = 'load-more-btn';
  btn.dataset.action = 'load-earlier';
  btn.type = 'button';
  btn.textContent = `\u2191 \u52a0\u8f7d\u66f4\u65e9\u7684\u6d88\u606f (${count} \u6761)`;
  btn.setAttribute('aria-label', `Load ${count} earlier messages`);
  loadMoreRow.appendChild(btn);
  return loadMoreRow;
}

function parseOrchestrationHistoryResult(resultText) {
  const text = (resultText || '').trimStart();
  const aborted = /^## Orchestration Aborted\b/m.test(text);
  const completedMatch = text.match(/(\d+) completed/);
  const failedMatch = text.match(/(\d+) failed/);
  const skippedMatch = text.match(/(\d+) skipped/);
  const taskStatuses = new Map();
  const taskHeaderRe = /^### \[\d+\] (.+?) \((.+?)\) — (✅|❌|⏭️)/gm;

  let match;
  while ((match = taskHeaderRe.exec(text)) !== null) {
    const [, taskId, _agent, icon] = match;
    const status = icon === '✅' ? 'completed' : icon === '❌' ? 'failed' : 'skipped';
    taskStatuses.set(taskId, status);
  }

  return {
    aborted,
    completed: completedMatch ? parseInt(completedMatch[1], 10) : 0,
    failed: failedMatch ? parseInt(failedMatch[1], 10) : 0,
    skipped: skippedMatch ? parseInt(skippedMatch[1], 10) : 0,
    taskStatuses,
  };
}

function renderHistoryMessage(m, options: { followMarkdown?: boolean } = {}) {
  const { followMarkdown = true } = options;
  switch (m.role) {
    case 'user': {
      const el = addMsg('user', m.content, m.timestamp);
      if (m.images && m.images.length > 0) renderUserImageThumbnails(el, m.images);
      break;
    }
    case 'assistant': {
      if (m.thinking && m.thinking.trim() && state.showReasoning) {
        const panel = buildHistoryReasoningPanel(m.thinking);
        dom.chat.appendChild(wrapInTimeline(panel, 'reasoning'));
        invalidateChatScrollCache();
      }
      // Thinking-only cycles (no text, tool call follows) have empty content.
      // Only create a bubble when there is actual message text.
      if (m.content) {
        const el = addMsg('assistant', m.content, m.timestamp);
        el._rawText = m.content;
        scheduleMarkdownRender(el, { followScroll: followMarkdown });
      }
      break;
    }
    case 'tool_call': {
      if (m.name === 'task') {
        try {
          const args = JSON.parse(m.arguments || '{}');
          const ref = { task_id: m.id, agent: args.agent || 'sub-agent' };
          createSubagentPanel(ref.agent, args.prompt || '', ref.task_id);
          if (!state._historyTaskIds) state._historyTaskIds = new Map();
          state._historyTaskIds.set(m.id, ref);
        } catch {
          addToolCall(m.name, m.arguments, m.id);
        }
        break;
      }
      if (m.name === 'orchestrate') {
        try {
          const args = JSON.parse(m.arguments || '{}');
          const tasks = Array.isArray(args.tasks) ? args.tasks : [];
          const orchestrateId = `hist-${m.id || Date.now()}`;
          createOrchestratePanel({
            orchestrate_id: orchestrateId,
            task_count: tasks.length,
            layer_count: 0,
            tasks: tasks.map((t) => ({
              id: t.id,
              agent: t.agent,
              depends_on: t.depends_on || [],
              prompt_preview: t.prompt || '',
            })),
          });
          if (!state._historyOrchestrateIds) state._historyOrchestrateIds = new Map();
          state._historyOrchestrateIds.set(m.id, orchestrateId);
        } catch {
          addToolCall(m.name, m.arguments, m.id);
        }
        break;
      }
      addToolCall(m.name, m.arguments, m.id);
      break;
    }
    case 'tool_result': {
      if (state._historyTaskIds && state._historyTaskIds.has(m.id)) {
        const ref = state._historyTaskIds.get(m.id);
        state._historyTaskIds.delete(m.id);
        if (ref && m.subagent_snapshot) {
          restoreSubagentHistorySnapshot(ref, m.subagent_snapshot);
        } else {
          const r = (m.result || '').trimStart();
          const failed =
            m.is_error === true ||
            r.startsWith('task error:') ||
            r.startsWith('[rejected') ||
            /^Sub-agent '.+' (failed|timed out)/.test(r);
          finishSubagentPanel(ref, !failed, {}, { immediate: true });
        }
        break;
      }
      if (state._historyOrchestrateIds && state._historyOrchestrateIds.has(m.id)) {
        const orchestrateId = state._historyOrchestrateIds.get(m.id);
        state._historyOrchestrateIds.delete(m.id);
        const r = (m.result || '').trimStart();
        const summary = parseOrchestrationHistoryResult(r);
        const registry = state.activeOrchestrations;
        const entry = registry && registry.get(orchestrateId);
        if (entry) {
          for (const [taskId, status] of summary.taskStatuses.entries()) {
            if (entry.taskRows.has(taskId)) {
              markOrchestrateTask({ orchestrate_id: orchestrateId, id: taskId }, status);
            }
          }
        }
        finishOrchestratePanel({
          orchestrate_id: orchestrateId,
          aborted: summary.aborted,
          completed: summary.completed,
          failed: summary.failed,
          skipped: summary.skipped,
        });
        break;
      }
      addToolResult('', m.result, m.id);
      break;
    }
  }
}

function loadEarlierMessages() {
  const { remaining, chunk: msgs } = splitHistoryLoadChunk(
    state.deferredHistory,
    HISTORY_LOAD_CHUNK_SIZE,
  );
  if (msgs.length === 0) return;
  state.deferredHistory = remaining;
  state._historyTaskIds = null;
  state._historyOrchestrateIds = null;
  const loadMoreRow = document.getElementById('load-more-row');
  const anchor = loadMoreRow ? loadMoreRow.nextElementSibling : dom.chat.firstElementChild;
  if (loadMoreRow) loadMoreRow.remove();
  const existing = [...dom.chat.children];
  dom.chat.replaceChildren();
  invalidateChatScrollCache();
  dom.chat.classList.add('no-animate');
  state.bulkRenderingChat = true;
  if (state.deferredHistory.length > 0) {
    dom.chat.appendChild(createLoadMoreRow(state.deferredHistory.length));
    invalidateChatScrollCache();
  }
  for (const m of msgs) renderHistoryMessage(m, { followMarkdown: false });
  // Finalize orphaned panels from deferred history.
  if (state._historyTaskIds && state._historyTaskIds.size > 0) {
    for (const ref of state._historyTaskIds.values()) {
      finishSubagentPanel(ref, false, {}, { immediate: true });
    }
    state._historyTaskIds = null;
  }
  if (state._historyOrchestrateIds && state._historyOrchestrateIds.size > 0) {
    for (const orchestrateId of state._historyOrchestrateIds.values()) {
      finishOrchestratePanel({ orchestrate_id: orchestrateId, aborted: true });
    }
    state._historyOrchestrateIds = null;
  }
  for (const el of existing) dom.chat.appendChild(el);
  invalidateChatScrollCache();
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
      closeToolDrawer();
      closeSubagentModal();
      closeOrchestrateTaskModal();
      clearReactStatus();
      clearBufferedChatUpdates();
      setAutoFollowChat(true);
      // replaceChildren() avoids the extra HTML parser invocation of
      // `innerHTML = ''` and is slightly friendlier to GC on large chats.
      dom.chat.replaceChildren();
      invalidateChatScrollCache();
      state.deferredHistory = [];
      state.activeSubagentPanels.clear();
      state.activeOrchestrations.clear();
      state._historyTaskIds = null;
      state._historyOrchestrateIds = null;
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
          dom.chat.appendChild(createLoadMoreRow(state.deferredHistory.length));
          invalidateChatScrollCache();
        }
        for (let i = startIdx; i < msgs.length; i++) {
          renderHistoryMessage(msgs[i]);
        }
        if (state._historyTaskIds && state._historyTaskIds.size > 0) {
          for (const ref of state._historyTaskIds.values()) {
            finishSubagentPanel(ref, false, {}, { immediate: true });
          }
          state._historyTaskIds = null;
        }
        // Finalize orphaned orchestrate panels that never got a tool_result.
        if (state._historyOrchestrateIds && state._historyOrchestrateIds.size > 0) {
          for (const orchestrateId of state._historyOrchestrateIds.values()) {
            finishOrchestratePanel({ orchestrate_id: orchestrateId, aborted: true });
          }
          state._historyOrchestrateIds = null;
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

    case 'start': {
      const isNewTurn = !state.busy || state.currentRoundStartedAt === 0;
      setBusy(true);
      if (isNewTurn) {
        state.currentRoundStartedAt = performance.now();
        state.currentRoundFirstTokenAt = 0;
      }
      finishAssistantStream({ discardIfEmpty: true });
      beginAssistantStream();
      if (data.react_visible && data.phase) {
        showReactStatus(data.phase, data.cycle);
      }
      break;
    }

    case 'delta':
      if (data.subagent) break;
      if (data.content) markCurrentRoundFirstTokenAt();
      if (state.currentMsg) {
        state.pendingAssistantText += data.content;
        scheduleFlush();
      }
      break;

    case 'done': {
      const finishedAssistantMsg = finishAssistantStream({ discardIfEmpty: true });
      const activeReasoningPanel = state.reasoningPanel;
      finishReasoningStream();
      if (activeReasoningPanel) {
        activeReasoningPanel.classList.remove('reasoning-active');
        const body = activeReasoningPanel.querySelector('.reasoning-body') as Element | null;
        const chevron = activeReasoningPanel.querySelector('.chevron') as Element | null;
        if (finalizeOrDiscardLiveReasoningPanel(activeReasoningPanel)) {
          setTimeout(() => {
            if (body) animateCollapsibleSection(body, false);
            if (chevron) chevron.classList.remove('open');
          }, 600);
        }
      }
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
        const firstTokenMs = state.currentRoundFirstTokenAt
          ? Math.max(0, state.currentRoundFirstTokenAt - state.currentRoundStartedAt)
          : null;
        appendRoundUsage(
          finishedAssistantMsg,
          data.round_input_tokens ?? 0,
          data.round_output_tokens ?? 0,
          firstTokenMs,
        );
      }
      resetRoundTimers();
      setBusy(false);
      break;
    }

    case 'react_phase':
      showReactStatus(data.phase, data.cycle);
      break;

    case 'thinking_start': {
      if (!state.showReasoning) break;
      if (data.subagent) {
        startSubagentReasoning({ task_id: data.task_id, agent: data.subagent });
        break;
      }
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
      invalidateChatScrollCache();
      pinReactStatusToBottom();
      animatePanelIn(panel);
      state.reasoningPanel = panel;
      hideWelcome();
      scrollDown();
      break;
    }

    case 'thinking_delta':
      if (data.content && !data.subagent) {
        markCurrentRoundFirstTokenAt();
      }
      if (!state.showReasoning) break;
      if (data.subagent) {
        appendSubagentReasoning(
          { task_id: data.task_id, agent: data.subagent },
          data.content || '',
        );
        break;
      }
      if (state.reasoningPanel) {
        state.pendingReasoningText += data.content;
        scheduleFlush();
      }
      break;

    case 'thinking_done':
      if (!state.showReasoning) {
        finishReasoningStream();
        state.reasoningPanel = null;
        break;
      }
      if (data.subagent) {
        finishSubagentReasoning({ task_id: data.task_id, agent: data.subagent });
        break;
      }
      if (state.reasoningPanel) {
        finishReasoningStream();
        const reasoningPanel = state.reasoningPanel;
        reasoningPanel.classList.remove('reasoning-active');
        const body = reasoningPanel.querySelector('.reasoning-body') as Element | null;
        const chevron = reasoningPanel.querySelector('.chevron') as Element | null;
        if (!finalizeOrDiscardLiveReasoningPanel(reasoningPanel)) {
          state.reasoningPanel = null;
          break;
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
      markCurrentRoundFirstTokenAt();
      setReactActTool(data.name, 0);
      addToolCall(data.name, data.arguments, data.id);
      break;

    case 'tool_progress':
      if (data.subagent) break;
      setReactActTool(data.name, data.elapsed_ms || 0);
      updateToolProgress(data.id, data.elapsed_ms || 0);
      break;

    case 'tool_result':
      if (data.subagent) {
        updateSubagentToolResult(
          { task_id: data.task_id, agent: data.subagent },
          data.id,
          data.duration_ms,
          data.result,
          data.is_error,
          data.name,
        );
        break;
      }
      if (state.reactStatusPhase === 'act' && state.reactStatusToolName === data.name) {
        state.reactStatusElapsedMs = data.duration_ms || state.reactStatusElapsedMs;
        renderReactStatus();
      }
      addToolResult(data.name, data.result, data.id, data.duration_ms ?? null);
      break;

    case 'task_started':
      createSubagentPanel(data.agent, data.prompt, data.task_id);
      break;
    case 'task_progress':
      updateSubagentProgress({ task_id: data.task_id, agent: data.agent }, data.cycle);
      break;
    case 'task_tool':
      addSubagentTool(
        { task_id: data.task_id, agent: data.agent },
        data.tool,
        data.id,
        data.arguments,
      );
      break;
    case 'task_completed':
      finishSubagentPanel({ task_id: data.task_id, agent: data.agent }, true, {
        cycles: data.cycles,
        tool_calls: data.tool_calls,
        duration_ms: data.duration_ms,
        input_tokens: data.input_tokens,
        output_tokens: data.output_tokens,
        result_preview: data.result_preview,
        result_excerpt: data.result_excerpt,
      });
      break;
    case 'task_failed':
      finishSubagentPanel({ task_id: data.task_id, agent: data.agent }, false, {
        cycles: data.cycles,
        tool_calls: data.tool_calls,
        duration_ms: data.duration_ms,
        input_tokens: data.input_tokens,
        output_tokens: data.output_tokens,
        error: data.error,
      });
      break;

    case 'orchestrate_started':
      createOrchestratePanel(data);
      break;
    case 'orchestrate_layer':
      updateOrchestrateLayer(data);
      break;
    case 'orchestrate_task_started':
      markOrchestrateTask(data, 'running');
      break;
    case 'orchestrate_task_completed':
      markOrchestrateTask(data, 'completed');
      break;
    case 'orchestrate_task_failed':
      markOrchestrateTask(data, 'failed');
      break;
    case 'orchestrate_task_skipped':
      markOrchestrateTask(data, 'skipped');
      break;
    case 'orchestrate_completed':
      finishOrchestratePanel(data);
      break;

    case 'context_compressed':
      addSystem(
        `Context auto-compressed: removed ${data.messages_removed || 0} messages, token estimate ${data.before_estimate || 0} -> ${data.after_estimate || 0}`,
      );
      break;

    case 'context_compress_failed':
      addError(`Context auto-compress failed: ${data.error || 'unknown error'}`);
      break;

    case 'progress':
      addSystem(data.content);
      break;

    case 'success':
      clearReactStatus();
      addSystem(data.content, 'success', { dismissible: data.dismissible === true });
      setBusy(false);
      break;

    case 'system':
      clearReactStatus();
      addSystem(data.content, 'info', { dismissible: data.dismissible === true });
      setBusy(false);
      break;

    case 'error':
      finishAssistantStream({ discardIfEmpty: true });
      finishReasoningStream();
      clearReactStatus();
      addError(data.content, { dismissible: data.dismissible === true });
      state.reasoningPanel = null;
      resetRoundTimers();
      setBusy(false);
      break;
  }
}

// ── Event delegation for data-action buttons ──

const actionHandlers = {
  'toggle-tools': () => toggleToolsVisibility(),
  'toggle-reasoning': () => toggleReasoningVisibility(),
  'nav-settings': () => {
    closeMobileMenu();
    openSettingsPage();
  },
  'nav-usage': () => {
    closeMobileMenu();
    openUsagePage();
  },
  'close-page': (el) => {
    const overlay = el.closest('.page-overlay');
    if (!(overlay instanceof HTMLElement)) return;
    if (!closeOverlayById(overlay.id, closeSettingsPage, closeUsagePage)) {
      overlay.hidden = true;
    }
  },
  cmd: (el) => {
    const cmd = el.dataset.cmd;
    if (cmd) sendCmd(cmd);
  },
  'cmd-close-menu': (el) => {
    const cmd = el.dataset.cmd;
    if (cmd) sendCmd(cmd);
    closeMobileMenu();
  },
  'toggle-mobile-menu': () => toggleMobileMenu(),
  'toggle-theme': () => cycleTheme(),
  'show-shortcuts': () => {
    closeMobileMenu();
    toggleShortcutsOverlay();
  },
  'close-tool-drawer': () => closeToolDrawer(),
  'dismiss-system-card': (el) => {
    if (!el) return;
    const row = el.closest('.msg-row.system, .msg-row.error');
    if (row) row.remove();
  },
  'load-earlier': () => loadEarlierMessages(),
  'open-tool-drawer': (el) => openToolDrawerFromHeader(el),
  'toggle-tool': (el) => toggleTool(el),
  'subagent-copy-summary': (el) => copySubagentSummary(el),
  'subagent-open-tool-drawer': (el) => openSubagentToolDrawer(el),
  'open-subagent-modal': (el) => {
    closeOrchestrateTaskModal();
    openSubagentModal(el);
  },
  'close-subagent-modal': () => closeSubagentModal(),
  'open-orchestrate-task-modal': (el) => {
    closeSubagentModal();
    openOrchestrateTaskModal(el);
  },
  'close-orchestrate-task-modal': () => closeOrchestrateTaskModal(),
};

// ── Named global listeners ───────────────────────────────────────────────────
// Named so we can remove them in HMR `dispose` hooks and keep the set of
// active listeners bounded across hot reloads. (In production the page owns
// them for its entire lifetime.)

function handleDocumentClick(e: MouseEvent) {
  const target = e.target;
  if (!(target instanceof Element)) return;

  const el = target.closest('[data-action]');
  if (!el) {
    // Click on overlay backdrop to close
    if (target instanceof HTMLElement && target.classList.contains('page-overlay')) {
      if (!closeOverlayById(target.id, closeSettingsPage, closeUsagePage)) {
        target.hidden = true;
      }
    }
    return;
  }
  const action = (el as HTMLElement).dataset.action;
  const handler = action ? actionHandlers[action] : null;
  if (handler) handler(el);
}

function handleDocumentKeydown(e: KeyboardEvent) {
  if (e.key === 'Escape') {
    closeToolDrawer();
    closeMobileMenu();
    closeSubagentModal();
    closeOrchestrateTaskModal();
    closeSettingsPage();
    closeUsagePage();
    closeShortcutsOverlay();
    return;
  }

  if (e.key === 'Enter' || e.key === ' ') {
    const target = e.target;
    const el = target instanceof Element ? target.closest('[data-action]') : null;
    if (el instanceof HTMLElement && el.getAttribute('role') === 'button') {
      const action = el.dataset.action;
      const handler = action ? actionHandlers[action] : null;
      if (handler) {
        e.preventDefault();
        handler(el);
        return;
      }
    }
  }

  if (trapShortcutsFocus(e)) {
    return;
  }

  if (shortcutsOverlay && !shortcutsOverlay.hidden) {
    if ((e.ctrlKey || e.metaKey) && (e.key === '/' || e.key === 'k' || e.key === 'K')) {
      e.preventDefault();
    }
    return;
  }

  // Avoid stealing keys while the user types. We only treat shortcuts as
  // global when the active element is not an editable field, except for the
  // meta-combo variants which are still safe to intercept (Ctrl/Cmd is rarely
  // part of literal text input).
  const active = document.activeElement;
  const inField =
    active instanceof HTMLElement &&
    (active.tagName === 'INPUT' || active.tagName === 'TEXTAREA' || active.isContentEditable);
  const modKey = e.ctrlKey || e.metaKey;

  // Ctrl/Cmd + / → cycle theme. Matches the shortcut shown in the help
  // overlay; avoids the `?` key which conflicts with text entry.
  if (modKey && e.key === '/') {
    e.preventDefault();
    cycleTheme();
    return;
  }

  // Ctrl/Cmd + K → focus composer. Familiar pattern from Slack/Discord.
  if (modKey && (e.key === 'k' || e.key === 'K')) {
    e.preventDefault();
    if (dom.input) {
      dom.input.focus();
      dom.input.setSelectionRange(dom.input.value.length, dom.input.value.length);
    }
    return;
  }

  // Shift + / (i.e. the `?` key on US layouts) opens the shortcuts overlay.
  // We skip this when inside an editable field so typing a literal `?`
  // into a message still works.
  if (!inField && !modKey && e.key === '?') {
    e.preventDefault();
    toggleShortcutsOverlay();
  }
}

let shortcutsOverlay: HTMLElement | null = null;
let lastFocusBeforeShortcuts: Element | null = null;

function ensureShortcutsOverlay(): HTMLElement {
  if (shortcutsOverlay) return shortcutsOverlay;
  const el = document.createElement('div');
  el.className = 'shortcuts-overlay';
  el.hidden = true;
  el.setAttribute('role', 'dialog');
  el.setAttribute('aria-modal', 'true');
  el.setAttribute('aria-label', 'Keyboard shortcuts');
  el.innerHTML = `
    <div class="shortcuts-panel">
      <div class="shortcuts-header">
        <h2>Keyboard shortcuts</h2>
        <button type="button" class="shortcuts-close" aria-label="Close">×</button>
      </div>
      <dl class="shortcuts-list">
        <dt><kbd>Enter</kbd></dt><dd>Send message</dd>
        <dt><kbd>Shift</kbd>+<kbd>Enter</kbd></dt><dd>Newline in composer</dd>
        <dt><kbd>↑</kbd> / <kbd>↓</kbd></dt><dd>Browse input history</dd>
        <dt><kbd>Ctrl</kbd>+<kbd>K</kbd></dt><dd>Focus the composer</dd>
        <dt><kbd>Ctrl</kbd>+<kbd>/</kbd></dt><dd>Cycle theme (auto / light / dark)</dd>
        <dt><kbd>?</kbd></dt><dd>Show this help</dd>
        <dt><kbd>Esc</kbd></dt><dd>Close panels & menus</dd>
      </dl>
      <p class="shortcuts-hint">Press <kbd>Esc</kbd> to close.</p>
    </div>
  `;
  el.addEventListener('click', (ev) => {
    const t = ev.target;
    if (!(t instanceof Element)) return;
    if (t === el || t.classList.contains('shortcuts-close')) {
      closeShortcutsOverlay();
    }
  });
  document.body.appendChild(el);
  shortcutsOverlay = el;
  return el;
}

function getShortcutsFocusableElements(): HTMLElement[] {
  if (!shortcutsOverlay) return [];
  const selector = [
    'button:not([disabled])',
    '[href]',
    'input:not([disabled])',
    'select:not([disabled])',
    'textarea:not([disabled])',
    '[tabindex]:not([tabindex="-1"])',
  ].join(',');
  return Array.from(shortcutsOverlay.querySelectorAll<HTMLElement>(selector)).filter(
    (el) => !el.hasAttribute('hidden') && el.getAttribute('aria-hidden') !== 'true',
  );
}

function trapShortcutsFocus(e: KeyboardEvent): boolean {
  if (e.key !== 'Tab' || !shortcutsOverlay || shortcutsOverlay.hidden) return false;
  const focusable = getShortcutsFocusableElements();
  if (focusable.length === 0) {
    e.preventDefault();
    shortcutsOverlay.focus();
    return true;
  }

  const first = focusable[0];
  const last = focusable[focusable.length - 1];
  const active = document.activeElement;
  if (!(active instanceof HTMLElement) || !shortcutsOverlay.contains(active)) {
    e.preventDefault();
    first.focus();
    return true;
  }
  if (e.shiftKey && active === first) {
    e.preventDefault();
    last.focus();
    return true;
  }
  if (!e.shiftKey && active === last) {
    e.preventDefault();
    first.focus();
    return true;
  }
  return false;
}

function toggleShortcutsOverlay(): void {
  const el = ensureShortcutsOverlay();
  if (el.hidden) {
    lastFocusBeforeShortcuts = document.activeElement;
    el.hidden = false;
    const close = el.querySelector('.shortcuts-close');
    if (close instanceof HTMLElement) close.focus();
  } else {
    closeShortcutsOverlay();
  }
}

function closeShortcutsOverlay(): void {
  if (shortcutsOverlay && !shortcutsOverlay.hidden) {
    shortcutsOverlay.hidden = true;
    // Restore focus to whatever the user had active before opening; falling
    // back to the composer is nicer than leaving focus on <body>.
    if (lastFocusBeforeShortcuts instanceof HTMLElement && lastFocusBeforeShortcuts.isConnected) {
      lastFocusBeforeShortcuts.focus();
    } else if (dom.input) {
      dom.input.focus();
    }
    lastFocusBeforeShortcuts = null;
  }
}

function handleWindowResizeMenu() {
  if (window.innerWidth > 768) closeMobileMenu();
}

function handleJumpToLatestClick() {
  jumpToLatest();
}

// Throttle the chat scroll handler to one invocation per animation frame.
// `scroll` fires at device refresh rate on fast wheels/touchpads; running
// `syncChatScrollState` every single event produced redundant state writes
// and jump-to-latest button re-renders. rAF collapses bursts into a single
// update per frame without adding perceptible latency.
let scrollSyncRafId = 0;
function handleChatScroll() {
  if (scrollSyncRafId) return;
  scrollSyncRafId = requestAnimationFrame(() => {
    scrollSyncRafId = 0;
    // User-driven scroll means any cached scroll-distance read is stale.
    invalidateChatScrollCache();
    syncChatScrollState();
  });
}

// ResizeObserver handles the input composer growing as the user types (auto
// resizing textarea), panels opening/closing inside `#chat`, and the chat
// scroll container itself being resized. We used to pile three `window.resize`
// and two `visualViewport` listeners on top of each other for this, firing
// `getBoundingClientRect` on every burst; a single RO keeps the work O(frame).
let chatResizeObserver: ResizeObserver | null = null;
function installChatResizeObserver(): void {
  if (typeof ResizeObserver !== 'function') return;
  chatResizeObserver = new ResizeObserver(() => {
    invalidateChatScrollCache();
    syncToolDrawerBounds();
  });
  if (dom.chat) chatResizeObserver.observe(dom.chat);
  if (dom.inputArea) chatResizeObserver.observe(dom.inputArea);
}

document.addEventListener('click', handleDocumentClick);

// ── Init ──
initTheme();
scheduleBackgroundTask(() => {
  void preloadMarkdownEngine();
});
updateViewToggleButtons();
syncToolDrawerBounds();
updateJumpToLatestVisibility();

initImageListeners();
initInputListeners();
initMobileListeners();

document.addEventListener('keydown', handleDocumentKeydown);
dom.chat.addEventListener('scroll', handleChatScroll, { passive: true });
window.addEventListener('resize', syncToolDrawerBounds);
window.addEventListener('resize', handleWindowResizeMenu);
if (window.visualViewport) {
  window.visualViewport.addEventListener('resize', syncToolDrawerBounds);
  window.visualViewport.addEventListener('scroll', syncToolDrawerBounds);
}
installChatResizeObserver();
if (dom.jumpToLatestBtn) {
  dom.jumpToLatestBtn.addEventListener('click', handleJumpToLatestClick);
}

// Vite HMR: remove global listeners on module dispose so hot reloads don't
// accumulate duplicate handlers in the dev build. No-op in production.
if (import.meta.hot) {
  import.meta.hot.dispose(() => {
    if (scrollSyncRafId) {
      cancelAnimationFrame(scrollSyncRafId);
      scrollSyncRafId = 0;
    }
    cancelToolDrawerBoundsSync();
    if (chatResizeObserver) {
      chatResizeObserver.disconnect();
      chatResizeObserver = null;
    }
    disposeTheme();
    cancelReconnect();
    document.removeEventListener('click', handleDocumentClick);
    document.removeEventListener('keydown', handleDocumentKeydown);
    dom.chat.removeEventListener('scroll', handleChatScroll);
    window.removeEventListener('resize', syncToolDrawerBounds);
    window.removeEventListener('resize', handleWindowResizeMenu);
    if (window.visualViewport) {
      window.visualViewport.removeEventListener('resize', syncToolDrawerBounds);
      window.visualViewport.removeEventListener('scroll', syncToolDrawerBounds);
    }
    if (dom.jumpToLatestBtn) {
      dom.jumpToLatestBtn.removeEventListener('click', handleJumpToLatestClick);
    }
  });
}

void loadAppVersion();
connect(handleMessage);
prefetchPageChunks();
