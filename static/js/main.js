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
  updateSubagentToolResult, finishSubagentPanel,
  startSubagentReasoning, appendSubagentReasoning, finishSubagentReasoning,
  focusSubagentTool,
  toggleSubagentTools, focusSubagentCurrent, copySubagentSummary
} from './renderers/subagent.js';
import {
  createOrchestratePanel, updateOrchestrateLayer, markOrchestrateTask,
  finishOrchestratePanel, toggleOrchestrateTasks,
  focusOrchestrateActive, copyOrchestrateSummary, focusOrchestrateTool,
  parseOrchestrateCompositeTaskId,
  startOrchestrateTaskReasoning, appendOrchestrateTaskReasoning, finishOrchestrateTaskReasoning,
  addOrchestrateTaskTool, updateOrchestrateTaskTool,
} from './renderers/orchestrate.js';
import { openSettingsPage, closeSettingsPage, initSettingsListeners } from './settings.js';
import { openUsagePage, closeUsagePage, initUsageListeners } from './usage.js';

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
    dom.chat.classList.toggle('hide-tools', !state.showTools);
    if (!state.showTools) {
      closeToolDrawer();
      state.activeToolPanel = null;
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
    const status = icon === '✅'
      ? 'completed'
      : icon === '❌'
        ? 'failed'
        : 'skipped';
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
      if (m.name === 'task') {
        try {
          const args = JSON.parse(m.arguments || '{}');
          const ref = { task_id: m.id, agent: args.agent || 'sub-agent' };
          createSubagentPanel(ref.agent, args.prompt || '', ref.task_id);
          if (!state._historyTaskIds) state._historyTaskIds = new Map();
          state._historyTaskIds.set(m.id, ref);
        } catch { addToolCall(m.name, m.arguments, m.id); }
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
            tasks: tasks.map(t => ({
              id: t.id,
              agent: t.agent,
              depends_on: t.depends_on || [],
              prompt_preview: t.prompt || '',
            })),
          });
          if (!state._historyOrchestrateIds) state._historyOrchestrateIds = new Map();
          state._historyOrchestrateIds.set(m.id, orchestrateId);
        } catch { addToolCall(m.name, m.arguments, m.id); }
        break;
      }
      addToolCall(m.name, m.arguments, m.id);
      break;
    }
    case 'tool_result': {
      if (state._historyTaskIds && state._historyTaskIds.has(m.id)) {
        const ref = state._historyTaskIds.get(m.id);
        state._historyTaskIds.delete(m.id);
        const r = (m.result || '').trimStart();
        const failed = m.is_error === true
          || r.startsWith('task error:')
          || r.startsWith('[rejected')
          || /^Sub-agent '.+' (failed|timed out)/.test(r);
        finishSubagentPanel(ref, !failed, {}, { immediate: true });
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
  const msgs = state.deferredHistory;
  state.deferredHistory = [];
  state._historyTaskIds = null;
  state._historyOrchestrateIds = null;
  const loadMoreRow = document.getElementById('load-more-row');
  const anchor = loadMoreRow ? loadMoreRow.nextElementSibling : dom.chat.firstElementChild;
  if (loadMoreRow) loadMoreRow.remove();
  const existing = [...dom.chat.children];
  dom.chat.replaceChildren();
  dom.chat.classList.add('no-animate');
  state.bulkRenderingChat = true;
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
      clearReactStatus();
      clearBufferedChatUpdates();
      setAutoFollowChat(true);
      dom.chat.innerHTML = '';
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
      if (!state.showReasoning) break;
      if (data.subagent) {
        const orchInfo = parseOrchestrateCompositeTaskId(data.task_id);
        if (orchInfo) {
          startOrchestrateTaskReasoning(orchInfo.orchestrateId, orchInfo.taskId, data.subagent);
          break;
        }
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
      pinReactStatusToBottom();
      animatePanelIn(panel);
      state.reasoningPanel = panel;
      hideWelcome();
      scrollDown();
      break;
    }

    case 'thinking_delta':
      if (!state.showReasoning) break;
      if (data.subagent) {
        const orchInfo = parseOrchestrateCompositeTaskId(data.task_id);
        if (orchInfo) {
          appendOrchestrateTaskReasoning(orchInfo.orchestrateId, orchInfo.taskId, data.content || '');
          break;
        }
        appendSubagentReasoning({ task_id: data.task_id, agent: data.subagent }, data.content || '');
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
        const orchInfo = parseOrchestrateCompositeTaskId(data.task_id);
        if (orchInfo) {
          finishOrchestrateTaskReasoning(orchInfo.orchestrateId, orchInfo.taskId);
          break;
        }
        finishSubagentReasoning({ task_id: data.task_id, agent: data.subagent });
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
      addToolCall(data.name, data.arguments, data.id);
      break;

    case 'tool_progress':
      if (data.subagent) break;
      setReactActTool(data.name, data.elapsed_ms || 0);
      updateToolProgress(data.id, data.elapsed_ms || 0);
      break;

    case 'tool_result':
      if (data.subagent) {
        const orchInfo = parseOrchestrateCompositeTaskId(data.task_id);
        if (orchInfo) {
          updateOrchestrateTaskTool(
            orchInfo.orchestrateId, orchInfo.taskId,
            data.id, data.duration_ms, data.result, data.is_error, data.name,
          );
          break;
        }
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
    case 'task_tool': {
      const orchInfo = parseOrchestrateCompositeTaskId(data.task_id);
      if (orchInfo) {
        addOrchestrateTaskTool(
          orchInfo.orchestrateId, orchInfo.taskId,
          data.tool, data.id, data.arguments,
        );
        break;
      }
      addSubagentTool({ task_id: data.task_id, agent: data.agent }, data.tool, data.id, data.arguments);
      break;
    }
    case 'task_completed':
      finishSubagentPanel(
        { task_id: data.task_id, agent: data.agent },
        true,
        {
          cycles: data.cycles,
          tool_calls: data.tool_calls,
          duration_ms: data.duration_ms,
          input_tokens: data.input_tokens,
          output_tokens: data.output_tokens,
          result_preview: data.result_preview,
          result_excerpt: data.result_excerpt,
        }
      );
      break;
    case 'task_failed':
      finishSubagentPanel(
        { task_id: data.task_id, agent: data.agent },
        false,
        {
          cycles: data.cycles,
          tool_calls: data.tool_calls,
          duration_ms: data.duration_ms,
          input_tokens: data.input_tokens,
          output_tokens: data.output_tokens,
          error: data.error,
        }
      );
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
        `Context auto-compressed: removed ${data.messages_removed || 0} messages, token estimate ${data.before_estimate || 0} -> ${data.after_estimate || 0}`
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
      setBusy(false);
      break;
  }
}

// ── Event delegation for data-action buttons ──

const actionHandlers = {
  'toggle-tools': () => toggleToolsVisibility(),
  'toggle-reasoning': () => toggleReasoningVisibility(),
  'nav-settings': () => { closeMobileMenu(); openSettingsPage(); },
  'nav-usage': () => { closeMobileMenu(); openUsagePage(); },
  'close-page': (el) => {
    const overlay = el.closest('.page-overlay');
    if (overlay) overlay.hidden = true;
  },
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
  'dismiss-system-card': (el) => {
    if (!el) return;
    const row = el.closest('.msg-row.system, .msg-row.error');
    if (row) row.remove();
  },
  'load-earlier': () => loadEarlierMessages(),
  'open-tool-drawer': (el) => openToolDrawerFromHeader(el),
  'toggle-tool': (el) => toggleTool(el),
  'subagent-toggle-all': (el) => toggleSubagentTools(el),
  'subagent-focus-current': (el) => focusSubagentCurrent(el),
  'subagent-focus-tool': (el) => focusSubagentTool(el),
  'subagent-copy-summary': (el) => copySubagentSummary(el),
  'orchestrate-toggle-all': (el) => toggleOrchestrateTasks(el),
  'orchestrate-focus-active': (el) => focusOrchestrateActive(el),
  'orchestrate-focus-tool': (el) => focusOrchestrateTool(el),
  'orchestrate-copy-summary': (el) => copyOrchestrateSummary(el),
};

document.addEventListener('click', (e) => {
  const el = e.target.closest('[data-action]');
  if (!el) {
    // Click on overlay backdrop to close
    if (e.target.classList.contains('page-overlay')) {
      e.target.hidden = true;
    }
    return;
  }
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
initSettingsListeners();
initUsageListeners();

document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') {
    closeToolDrawer();
    closeMobileMenu();
    closeSettingsPage();
    closeUsagePage();
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
