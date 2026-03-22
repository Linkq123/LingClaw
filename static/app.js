// ── State ──
const chat = document.getElementById('chat');
const input = document.getElementById('input');
const inputArea = document.getElementById('input-area');
const sendBtn = document.getElementById('send');
const sendIcon = document.getElementById('send-icon');
const connDot = document.getElementById('conn-dot');
const connLabel = document.getElementById('conn-label');
const sessionNameEl = document.getElementById('session-name');
const sessionIdEl = document.getElementById('session-id');
const sessionList = document.getElementById('session-list');
const toggleToolsBtn = document.getElementById('toggle-tools-btn');
const toggleReasoningBtn = document.getElementById('toggle-reasoning-btn');
const toolDrawer = document.getElementById('tool-drawer');
const toolDrawerBackdrop = document.getElementById('tool-drawer-backdrop');
const toolDrawerTitle = document.getElementById('tool-drawer-title');
const toolDrawerMeta = document.getElementById('tool-drawer-meta');
const toolDrawerArgs = document.getElementById('tool-drawer-args');
const toolDrawerResult = document.getElementById('tool-drawer-result');
const toolDrawerResultSection = document.getElementById('tool-drawer-result-section');
const DEFAULT_BRAND_AVATAR = 'branding/avatar.png';
const DEFAULT_WELCOME_LOGO = 'branding/logo-wordmark.png';

let ws = null;
let currentMsg = null;
let busy = false;
let currentSessionId = '';
let sessions = [];
let reasoningPanel = null;
let reactStatusRow = null;
let reactStatusPhase = '';
let reactStatusCycle = null;
let reactStatusToolName = '';
let reactStatusElapsedMs = 0;
let reactPhaseShownAt = 0;
let reactPhaseTimer = 0;
let reactPhaseQueue = [];
let reactPendingClear = false;
let reconnectDelay = 1000;
let reconnectAttempts = 0;
const MAX_RECONNECT_ATTEMPTS = 50;
const MIN_REACT_ANALYZE_VISIBLE_MS = 180;
const MIN_REACT_ACT_VISIBLE_MS = 420;
const MIN_REACT_OBSERVE_VISIBLE_MS = 650;
const MAX_REACT_QUEUED_PHASES = 2;
let pendingAssistantText = '';
let pendingReasoningText = '';
let flushHandle = 0;
let _deferredHistory = [];
const HISTORY_RENDER_LIMIT = 50;
let activeToolPanel = null;
let showTools = true;
let showReasoning = true;
const markdownRenderQueue = [];
let markdownQueueHandle = 0;

function afterNextPaint(callback) {
  requestAnimationFrame(() => requestAnimationFrame(callback));
}

function animatePanelIn(panel) {
  if (!panel) return;
  panel.classList.add('panel-enter');
  afterNextPaint(() => {
    if (!panel.isConnected) return;
    panel.classList.add('panel-enter-active');
  });
}

function cancelScheduledMarkdownRender(el) {
  if (!el) return;
  if (el._markdownIdleHandle) {
    if (typeof cancelIdleCallback === 'function') {
      cancelIdleCallback(el._markdownIdleHandle);
    } else {
      clearTimeout(el._markdownIdleHandle);
    }
    el._markdownIdleHandle = 0;
  }
}

function scheduleBackgroundTask(callback, timeout = 180) {
  if (typeof requestIdleCallback === 'function') {
    return requestIdleCallback(callback, { timeout });
  }
  return setTimeout(callback, 16);
}

function formatToolDuration(durationMs) {
  if (durationMs == null) return '';
  if (durationMs < 1000) {
    return `${Math.max(1, Math.round(durationMs))}ms`;
  }
  return `${(durationMs / 1000).toFixed(durationMs < 10000 ? 1 : 0)}s`;
}

function cancelBackgroundTask(handle) {
  if (!handle) return;
  if (typeof cancelIdleCallback === 'function') {
    cancelIdleCallback(handle);
  } else {
    clearTimeout(handle);
  }
}

function shouldHighlightBlock(block, index, totalBlocks) {
  const code = block.textContent || '';
  if (code.length > 4000) return false;
  if (totalBlocks > 6 && index >= 4) return false;
  return true;
}

function scheduleCodeHighlight(blocks) {
  const codeBlocks = [...blocks];
  const highlightQueue = codeBlocks.filter((block, index) => {
    if (!block.isConnected || !shouldHighlightBlock(block, index, codeBlocks.length)) {
      block.classList.add('code-highlight-deferred');
      return false;
    }
    return true;
  });

  const highlightChunk = () => {
    let processed = 0;
    while (highlightQueue.length && processed < 2) {
      const block = highlightQueue.shift();
      if (block?.isConnected) {
        hljs.highlightElement(block);
      }
      processed += 1;
    }
    if (highlightQueue.length) {
      scheduleBackgroundTask(highlightChunk, 120);
    }
  };

  if (highlightQueue.length) {
    scheduleBackgroundTask(highlightChunk, 120);
  }
}

function scheduleMarkdownRender(el, options = {}) {
  if (!el) return;
  const { followScroll } = options;
  cancelScheduledMarkdownRender(el);
  const queuedIndex = markdownRenderQueue.indexOf(el);
  if (queuedIndex !== -1) {
    markdownRenderQueue.splice(queuedIndex, 1);
  }
  el.classList.add('markdown-pending');
  el._markdownShouldFollow = typeof followScroll === 'boolean'
    ? followScroll
    : chat.scrollHeight - chat.scrollTop - chat.clientHeight < 80;
  markdownRenderQueue.push(el);
  if (!markdownQueueHandle) {
    markdownQueueHandle = scheduleBackgroundTask(processMarkdownQueue);
  }
}

function processMarkdownQueue() {
  markdownQueueHandle = 0;
  const el = markdownRenderQueue.shift();
  if (!el) return;
  el._markdownIdleHandle = 0;
  if (el.isConnected) {
    renderMarkdown(el);
    el.classList.remove('markdown-pending');
    if (el._markdownShouldFollow) scrollDown();
  }
  el._markdownShouldFollow = false;
  if (markdownRenderQueue.length) {
    markdownQueueHandle = scheduleBackgroundTask(processMarkdownQueue);
  }
}

function animateCollapsibleSection(body, expand) {
  if (!body) return;

  const startHeight = body.getBoundingClientRect().height;
  body.classList.toggle('show', expand);
  const targetHeight = expand ? body.scrollHeight : 0;

  body.style.height = `${startHeight}px`;
  body.getBoundingClientRect();
  body.classList.toggle('is-open', expand);
  body.style.height = `${targetHeight}px`;

  const finalize = () => {
    body.style.height = expand ? 'auto' : '0px';
    body.removeEventListener('transitionend', finalize);
  };

  body.addEventListener('transitionend', finalize);
}

function syncToolDrawerBounds() {
  if (!inputArea) return;
  const viewport = window.visualViewport;
  const rect = inputArea.getBoundingClientRect();
  const viewportBottom = viewport
    ? viewport.offsetTop + viewport.height
    : window.innerHeight;
  const bottomInset = Math.max(16, Math.ceil(viewportBottom - rect.top + 8));
  document.documentElement.style.setProperty('--tool-drawer-bottom', `${bottomInset}px`);
}

function updateViewToggleButtons() {
  if (toggleToolsBtn) {
    toggleToolsBtn.textContent = `Tools: ${showTools ? 'On' : 'Off'}`;
    toggleToolsBtn.classList.toggle('is-active', showTools);
  }
  if (toggleReasoningBtn) {
    toggleReasoningBtn.textContent = `Reasoning: ${showReasoning ? 'On' : 'Off'}`;
    toggleReasoningBtn.classList.toggle('is-active', showReasoning);
  }
}

function applyViewState(viewState) {
  if (!viewState) return;

  if (typeof viewState.show_tools === 'boolean') {
    showTools = viewState.show_tools;
    if (!showTools) {
      closeToolDrawer();
      activeToolPanel = null;
      for (const panel of chat.querySelectorAll('.tool-panel')) {
        panel.remove();
      }
    }
  }

  if (typeof viewState.show_reasoning === 'boolean') {
    showReasoning = viewState.show_reasoning;
    if (!showReasoning) {
      finishReasoningStream();
      if (reasoningPanel) reasoningPanel.remove();
      reasoningPanel = null;
    }
  }

  updateViewToggleButtons();
}

function toggleToolsVisibility() {
  if (!ws || ws.readyState !== 1) return;
  const nextShowTools = !showTools;
  applyViewState({ show_tools: nextShowTools });
  sendCmd(`/tool ${nextShowTools ? 'on' : 'off'}`);
}

function toggleReasoningVisibility() {
  if (!ws || ws.readyState !== 1) return;
  const nextShowReasoning = !showReasoning;
  applyViewState({ show_reasoning: nextShowReasoning });
  sendCmd(`/reasoning ${nextShowReasoning ? 'on' : 'off'}`);
}

function canSendWhileBusy(cmd) {
  return /^\/(tool|reasoning)\b/i.test(cmd);
}

// ── Progressive segmented markdown ──

function findProgressiveSplitPoint(text) {
  let inFence = false;
  let lastSplit = -1;
  let i = 0;
  while (i < text.length) {
    const atLineStart = (i === 0 || text[i - 1] === '\n');
    if (atLineStart && i + 2 < text.length &&
        text[i] === '`' && text[i + 1] === '`' && text[i + 2] === '`') {
      const wasFenced = inFence;
      inFence = !inFence;
      let j = i + 3;
      while (j < text.length && text[j] !== '\n') j++;
      i = j < text.length ? j + 1 : text.length;
      if (wasFenced && !inFence && i < text.length) {
        lastSplit = i;
      }
      continue;
    }
    if (!inFence && text[i] === '\n' && i + 1 < text.length && text[i + 1] === '\n') {
      let j = i + 2;
      while (j < text.length && text[j] === '\n') j++;
      lastSplit = j;
      i = j;
      continue;
    }
    i++;
  }
  return lastSplit;
}

function decorateCodeBlocks(container) {
  container.querySelectorAll('pre').forEach(pre => {
    pre.style.position = 'relative';
    const codeEl = pre.querySelector('code');
    if (codeEl) {
      const cls = [...codeEl.classList].find(c => c.startsWith('language-'));
      if (cls) {
        const label = document.createElement('span');
        label.className = 'code-lang-label';
        label.textContent = cls.replace('language-', '');
        pre.appendChild(label);
      }
    }
    const btn = document.createElement('button');
    btn.className = 'copy-btn';
    btn.textContent = 'Copy';
    btn.onclick = () => {
      const code = pre.querySelector('code');
      const text = code?.textContent || pre.textContent;
      if (navigator.clipboard) {
        navigator.clipboard.writeText(text).catch(() => fallbackCopy(text));
      } else {
        fallbackCopy(text);
      }
      btn.textContent = 'Copied!';
      setTimeout(() => btn.textContent = 'Copy', 1500);
    };
    pre.appendChild(btn);
  });
}

function appendRenderedSegment(el, markdownText) {
  const html = marked.parse(markdownText);
  const sanitized = typeof DOMPurify !== 'undefined' ? DOMPurify.sanitize(html) : html;
  const temp = document.createElement('div');
  temp.innerHTML = sanitized;
  decorateCodeBlocks(temp);
  const codeBlocks = [...temp.querySelectorAll('pre code')];
  const tail = el._liveTail;
  while (temp.firstChild) {
    if (tail && tail.parentNode === el) {
      el.insertBefore(temp.firstChild, tail);
    } else {
      el.appendChild(temp.firstChild);
    }
  }
  scheduleCodeHighlight(codeBlocks);
}

function updateLiveTail(el, text) {
  if (!el._liveTail) {
    el._liveTail = document.createTextNode(text);
    el.appendChild(el._liveTail);
  } else {
    el._liveTail.nodeValue = text;
  }
}

function removeLiveTail(el) {
  if (el._liveTail) {
    if (el._liveTail.parentNode) el._liveTail.parentNode.removeChild(el._liveTail);
    el._liveTail = null;
  }
}

function flushAssistantText() {
  if (!currentMsg || !pendingAssistantText) return;
  currentMsg._rawText = (currentMsg._rawText || '') + pendingAssistantText;
  pendingAssistantText = '';

  const raw = currentMsg._rawText;
  const offset = currentMsg._renderedOffset || 0;
  const splitAt = findProgressiveSplitPoint(raw);

  if (splitAt > offset) {
    if (currentMsg._textNode) {
      currentMsg._textNode.remove();
      currentMsg._textNode = null;
    }
    appendRenderedSegment(currentMsg, raw.substring(offset, splitAt));
    currentMsg._renderedOffset = splitAt;
    updateLiveTail(currentMsg, raw.substring(splitAt));
  } else if (offset > 0) {
    updateLiveTail(currentMsg, raw.substring(offset));
  } else {
    if (!currentMsg._textNode) {
      currentMsg._textNode = document.createTextNode(raw);
      currentMsg.replaceChildren(currentMsg._textNode);
    } else {
      currentMsg._textNode.nodeValue = raw;
    }
  }
  revealCurrentAssistant();
}

function flushReasoningText() {
  if (!reasoningPanel || !pendingReasoningText) return;
  const body = reasoningPanel.querySelector('.reasoning-body');
  if (!body) { pendingReasoningText = ''; return; }
  if (!body._textNode) {
    body._textNode = document.createTextNode(pendingReasoningText);
    body.appendChild(body._textNode);
  } else {
    body._textNode.nodeValue += pendingReasoningText;
  }
  pendingReasoningText = '';
}

function flushStreaming() {
  flushHandle = 0;
  const follow = chat.scrollHeight - chat.scrollTop - chat.clientHeight < 80;
  flushAssistantText();
  flushReasoningText();
  if (follow) scrollDown();
}

function scheduleFlush() {
  if (!flushHandle) {
    flushHandle = requestAnimationFrame(flushStreaming);
  }
}

function cancelAssistantFlush() {
  pendingAssistantText = '';
  cancelFlushIfIdle();
}

function cancelReasoningFlush() {
  pendingReasoningText = '';
  cancelFlushIfIdle();
}

function cancelFlushIfIdle() {
  if (!pendingAssistantText && !pendingReasoningText && flushHandle) {
    cancelAnimationFrame(flushHandle);
    flushHandle = 0;
  }
}

function currentMsgRow() {
  return currentMsg ? currentMsg.closest('.msg-row') : null;
}

function beginAssistantStream() {
  cancelAssistantFlush();
  const message = addAssistant('');
  const row = message.closest('.msg-row');
  if (row) {
    row.hidden = true;
  }
  message.classList.add('typing');
  message._rawText = '';
  message._renderedOffset = 0;
  currentMsg = message;
}

function revealCurrentAssistant() {
  const row = currentMsgRow();
  if (row) {
    row.hidden = false;
  }
}

function finishAssistantStream({ discardIfEmpty = false } = {}) {
  flushAssistantText();
  if (!currentMsg) {
    return;
  }

  const row = currentMsgRow();
  const rawText = currentMsg._rawText || '';
  const raw = rawText.trim();
  currentMsg.classList.remove('typing');

  if (!raw && discardIfEmpty) {
    row?.remove();
    currentMsg = null;
    return;
  }

  if (!raw) {
    row?.removeAttribute('hidden');
    currentMsg = null;
    return;
  }

  revealCurrentAssistant();

  const offset = currentMsg._renderedOffset || 0;
  if (offset > 0) {
    removeLiveTail(currentMsg);
    const tail = rawText.substring(offset);
    if (tail) {
      appendRenderedSegment(currentMsg, tail);
    }
  } else {
    scheduleMarkdownRender(currentMsg);
  }
  currentMsg = null;
}

function finishReasoningStream() {
  flushReasoningText();
  cancelReasoningFlush();
  if (reasoningPanel) {
    const body = reasoningPanel.querySelector('.reasoning-body');
    if (body && body.classList.contains('show')) {
      body.style.height = 'auto';
    }
  }
  scrollDown();
}

function reactPhaseLabel(phase) {
  return {
    analyze: 'Analyze',
    act: 'Act',
    observe: 'Observe'
  }[phase] || phase || 'Analyze';
}

function renderReactStatus() {
  if (!reactStatusRow) return;
  const card = reactStatusRow.querySelector('.react-status-card');
  const phase = reactStatusRow.querySelector('.react-status-phase');
  const cycle = reactStatusRow.querySelector('.react-status-cycle');
  const detail = reactStatusRow.querySelector('.react-status-detail');
  if (!card || !phase || !cycle || !detail) return;
  card.dataset.phase = reactStatusPhase || 'analyze';
  phase.textContent = reactPhaseLabel(reactStatusPhase);
  cycle.textContent = Number.isInteger(reactStatusCycle) ? `cycle ${reactStatusCycle}` : '';
  if (reactStatusPhase === 'act' && reactStatusToolName) {
    const seconds = Math.max(1, Math.floor((reactStatusElapsedMs || 0) / 1000));
    detail.textContent = `${reactStatusToolName} · ${seconds}s`;
    detail.hidden = false;
  } else {
    detail.textContent = '';
    detail.hidden = true;
  }
}

function clearReactStatus() {
  if (reactPhaseTimer) {
    clearTimeout(reactPhaseTimer);
    reactPhaseTimer = 0;
  }
  reactPhaseQueue = [];
  reactPendingClear = false;
  reactStatusPhase = '';
  reactStatusCycle = null;
  reactStatusToolName = '';
  reactStatusElapsedMs = 0;
  reactPhaseShownAt = 0;
  if (reactStatusRow) {
    reactStatusRow.remove();
    reactStatusRow = null;
  }
}

function reactPhaseMinVisibleMs(phase) {
  switch (phase) {
    case 'act':
      return MIN_REACT_ACT_VISIBLE_MS;
    case 'observe':
      return MIN_REACT_OBSERVE_VISIBLE_MS;
    case 'analyze':
    default:
      return MIN_REACT_ANALYZE_VISIBLE_MS;
  }
}

function requestClearReactStatus() {
  if (!reactStatusPhase && reactPhaseQueue.length === 0) {
    clearReactStatus();
    return;
  }
  reactPendingClear = true;
  scheduleNextReactPhase();
}

function ensureReactStatusRow() {
  if (!reactStatusRow) {
    reactStatusRow = document.createElement('div');
    reactStatusRow.className = 'msg-row system react-status-row';
    reactStatusRow.innerHTML = `
      <div class="system-card system-inline react-status-card">
        <span class="react-status-tag">ReAct</span>
        <span class="react-status-phase"></span>
        <span class="react-status-cycle"></span>
        <span class="react-status-detail" hidden></span>
        <span class="react-status-dots" aria-hidden="true">
          <span></span>
          <span></span>
          <span></span>
        </span>
      </div>
    `;
    chat.appendChild(reactStatusRow);
    hideWelcome();
  }
}

function scheduleNextReactPhase() {
  if (reactPhaseTimer || !reactStatusPhase) {
    return;
  }

  const elapsed = performance.now() - reactPhaseShownAt;
  const delay = Math.max(0, reactPhaseMinVisibleMs(reactStatusPhase) - elapsed);
  reactPhaseTimer = setTimeout(() => {
    reactPhaseTimer = 0;
    const next = reactPhaseQueue.shift();
    if (next) {
      applyReactStatusNow(next.phase, next.cycle);
      return;
    }
    if (reactPendingClear) {
      clearReactStatus();
    }
  }, delay);
}

function applyReactStatusNow(phase, cycle = null) {
  ensureReactStatusRow();
  reactStatusPhase = phase;
  reactStatusCycle = Number.isInteger(cycle) ? cycle : null;
  if (phase !== 'act') {
    reactStatusToolName = '';
    reactStatusElapsedMs = 0;
  }
  reactPhaseShownAt = performance.now();
  renderReactStatus();
  scrollDown();
  scheduleNextReactPhase();
}

function setReactActTool(name, elapsedMs = 0) {
  if (!name) return;
  reactStatusToolName = name;
  reactStatusElapsedMs = elapsedMs;
  if (reactStatusPhase === 'act') {
    renderReactStatus();
  }
}

function showReactStatus(phase, cycle = null) {
  if (!phase) {
    requestClearReactStatus();
    return;
  }

  if (phase === 'finish') {
    requestClearReactStatus();
    return;
  }

  reactPendingClear = false;

  if (!reactStatusPhase && reactPhaseQueue.length === 0 && !reactPhaseTimer) {
    applyReactStatusNow(phase, cycle);
    return;
  }

  if (reactStatusPhase === phase && reactPhaseQueue.length === 0) {
    reactStatusCycle = Number.isInteger(cycle) ? cycle : null;
    renderReactStatus();
    return;
  }

  for (let index = reactPhaseQueue.length - 1; index >= 0; index -= 1) {
    if (reactPhaseQueue[index].phase === phase) {
      reactPhaseQueue[index].cycle = Number.isInteger(cycle) ? cycle : null;
      reactPhaseQueue.splice(index + 1);
      scheduleNextReactPhase();
      return;
    }
  }

  reactPhaseQueue.push({
    phase,
    cycle: Number.isInteger(cycle) ? cycle : null,
  });
  if (reactPhaseQueue.length > MAX_REACT_QUEUED_PHASES) {
    reactPhaseQueue.splice(0, reactPhaseQueue.length - MAX_REACT_QUEUED_PHASES);
  }
  scheduleNextReactPhase();
}

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

// ── WebSocket ──
function connect() {
  const proto = location.protocol === 'https:' ? 'wss' : 'ws';
  const stored = sessionStorage.getItem('lingclaw_session');
  const qs = stored ? `?session=${encodeURIComponent(stored)}` : '';
  ws = new WebSocket(`${proto}://${location.host}/ws${qs}`);

  ws.onopen = () => {
    reconnectDelay = 1000;
    reconnectAttempts = 0;
    connDot.className = 'conn-dot connected';
    connLabel.textContent = 'Online';
    addSystem('Connected.');
  };

  ws.onclose = () => {
    connDot.className = 'conn-dot disconnected';
    connLabel.textContent = 'Offline';
    finishAssistantStream({ discardIfEmpty: true });
    finishReasoningStream();
    closeToolDrawer();
    clearReactStatus();
    reasoningPanel = null;
    setBusy(false);
    if (reconnectAttempts < MAX_RECONNECT_ATTEMPTS) {
      addSystem('Disconnected. Reconnecting...');
      setTimeout(connect, reconnectDelay);
      reconnectDelay = Math.min(reconnectDelay * 2, 30000);
      reconnectAttempts++;
    } else {
      addSystem('Connection lost. Please refresh the page.', 'error');
    }
  };

  ws.onerror = () => ws.close();

  ws.onmessage = (e) => {
    let data;
    try { data = JSON.parse(e.data); } catch { console.warn('Invalid JSON from server:', e.data); return; }
    handleMessage(data);
  };
}

function handleMessage(data) {
  switch (data.type) {
    case 'session':
      currentSessionId = data.id;
      sessionNameEl.textContent = data.name || 'New Chat';
      sessionIdEl.textContent = data.id.slice(0, 12);
      sessionStorage.setItem('lingclaw_session', data.id);
      applyViewState(data);
      break;

    case 'session_switched':
      currentSessionId = data.id;
      sessionNameEl.textContent = data.name || 'New Chat';
      sessionIdEl.textContent = data.id.slice(0, 12);
      sessionStorage.setItem('lingclaw_session', data.id);
      applyViewState(data);
      finishAssistantStream({ discardIfEmpty: true });
      finishReasoningStream();
      closeToolDrawer();
      clearReactStatus();
      chat.innerHTML = '';
      reasoningPanel = null;
      setBusy(false);
      break;

    case 'history': {
      if (!showTools) {
        data.messages = (data.messages || []).filter(m => m.role !== 'tool_call' && m.role !== 'tool_result');
      }
      closeToolDrawer();
      clearReactStatus();
      chat.innerHTML = '';
      _deferredHistory = [];
      const msgs = data.messages || [];
      if (msgs.length === 0) {
        showWelcome();
      } else {
        chat.classList.add('no-animate');
        let startIdx = 0;
        if (msgs.length > HISTORY_RENDER_LIMIT) {
          startIdx = findHistoryRenderStart(msgs, msgs.length - HISTORY_RENDER_LIMIT);
          _deferredHistory = msgs.slice(0, startIdx);
          const loadMoreRow = document.createElement('div');
          loadMoreRow.className = 'msg-row system';
          loadMoreRow.id = 'load-more-row';
          loadMoreRow.innerHTML = `<button class="load-more-btn" onclick="loadEarlierMessages()">↑ 加载更早的消息 (${_deferredHistory.length} 条)</button>`;
          chat.appendChild(loadMoreRow);
        }
        for (let i = startIdx; i < msgs.length; i++) {
          renderHistoryMessage(msgs[i]);
        }
        requestAnimationFrame(() => chat.classList.remove('no-animate'));
      }
      break;
    }

    case 'view_state':
      applyViewState(data);
      break;

    case 'sessions_list':
      sessions = data.sessions || [];
      renderSessionList();
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
      if (currentMsg) {
        pendingAssistantText += data.content;
        scheduleFlush();
      }
      break;

    case 'done':
      finishAssistantStream({ discardIfEmpty: true });
      finishReasoningStream();
      requestClearReactStatus();
      reasoningPanel = null;
      setBusy(false);
      break;

    case 'react_phase': {
      showReactStatus(data.phase, data.cycle);
      break;
    }

    case 'thinking_start': {
      if (!showReasoning) break;
      const panel = document.createElement('div');
      panel.className = 'reasoning-panel reasoning-active';
      panel.innerHTML = `
        <div class="reasoning-header" onclick="toggleTool(this)">
          <span class="reasoning-icon">💭</span>
          <span class="reasoning-label">Reasoning</span>
          <span class="reasoning-status">推理中</span>
          <span class="chevron open">▸</span>
        </div>
        <div class="reasoning-body show"></div>
      `;
      const currentRow = currentMsg ? currentMsg.closest('.msg-row') : null;
      if (currentRow) {
        chat.insertBefore(panel, currentRow);
      } else {
        chat.appendChild(panel);
      }
      animatePanelIn(panel);
      reasoningPanel = panel;
      hideWelcome();
      scrollDown();
      break;
    }

    case 'thinking_delta':
      if (!showReasoning) break;
      if (reasoningPanel) {
        pendingReasoningText += data.content;
        scheduleFlush();
      }
      break;

    case 'thinking_done':
      if (!showReasoning) {
        finishReasoningStream();
        reasoningPanel = null;
        break;
      }
      if (reasoningPanel) {
        finishReasoningStream();
        reasoningPanel.classList.remove('reasoning-active');
        const status = reasoningPanel.querySelector('.reasoning-status');
        if (status) status.textContent = '完成';
        const body = reasoningPanel.querySelector('.reasoning-body');
        const chevron = reasoningPanel.querySelector('.chevron');
        setTimeout(() => {
          if (body) animateCollapsibleSection(body, false);
          if (chevron) chevron.classList.remove('open');
        }, 600);
        reasoningPanel = null;
      }
      break;

    case 'tool_call':
      setReactActTool(data.name, 0);
      if (!showTools) break;
      addToolCall(data.name, data.arguments, data.id);
      break;

    case 'tool_progress':
      setReactActTool(data.name, data.elapsed_ms || 0);
      if (!showTools) break;
      updateToolProgress(data.id, data.elapsed_ms || 0);
      break;

    case 'tool_result':
      if (reactStatusPhase === 'act' && reactStatusToolName === data.name) {
        reactStatusElapsedMs = data.duration_ms || reactStatusElapsedMs;
        renderReactStatus();
      }
      if (!showTools) break;
      addToolResult(data.name, data.result, data.id, data.duration_ms ?? null);
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
      reasoningPanel = null;
      setBusy(false);
      break;
  }
}

// ── Message rendering ──
function addMsg(cls, text, timestamp) {
  const isChat = (cls === 'user' || cls === 'assistant');
  const hasAvatar = cls === 'assistant';
  const row = document.createElement('div');
  row.className = `msg-row ${cls}`;

  if (hasAvatar) {
    const avatar = document.createElement('div');
    avatar.className = 'msg-avatar';
    setAssistantAvatar(avatar);
    row.appendChild(avatar);
  }

  const el = document.createElement('div');
  el.className = `msg ${cls}`;
  el.textContent = text;

  if (isChat) {
    const content = document.createElement('div');
    content.className = 'msg-content';
    content.appendChild(el);
    const time = document.createElement('div');
    time.className = 'msg-time';
    time.textContent = timestamp ? formatTime(new Date(timestamp * 1000)) : formatTime(new Date());
    content.appendChild(time);
    row.appendChild(content);
  } else {
    row.appendChild(el);
  }

  chat.appendChild(row);
  if (isChat) hideWelcome();
  scrollDown();
  return el;
}

function addAssistant(text) { return addMsg('assistant', text); }

function addSystem(t, kind = 'info') {
  const row = document.createElement('div');
  row.className = 'msg-row system';
  const card = document.createElement('div');
  card.className = 'system-card';
  const icon = kind === 'success' ? '✅' : 'ℹ️';
  if (kind === 'success') card.classList.add('success-card');
  const isBlock = t.includes('\n') || t.length > 80;
  if (isBlock) {
    card.innerHTML = `<div class="system-header"><span class="system-icon">📋</span> System</div><pre class="system-body">${escHtml(t)}</pre>`;
  } else {
    card.classList.add('system-inline');
    card.innerHTML = `<span class="system-icon">${icon}</span> <span>${escHtml(t)}</span>`;
  }
  row.appendChild(card);
  chat.appendChild(row);
  scrollDown();
}

function addError(t) {
  const row = document.createElement('div');
  row.className = 'msg-row error';
  const card = document.createElement('div');
  card.className = 'system-card system-inline error-card';
  card.innerHTML = `<span class="system-icon">⚠️</span> <span style="color:var(--accent-error)">${escHtml(t)}</span>`;
  row.appendChild(card);
  chat.appendChild(row);
  scrollDown();
}

function addToolCall(name, args, id) {
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
    <div class="tool-header" onclick="openToolDrawerFromHeader(this)">
      <span class="tool-icon">⚡</span>
      <span class="tool-name">${escHtml(name)}</span>
      <span class="tool-status">执行中</span>
      <span style="color:var(--dim);font-size:12px">${escHtml(truncateStr(args, 80))}</span>
    </div>
  `;
  chat.appendChild(panel);
  animatePanelIn(panel);
  hideWelcome();
  scrollDown();
}

function updateToolProgress(id, elapsedMs) {
  if (!id) return;
  const seconds = Math.max(1, Math.floor((elapsedMs || 0) / 1000));
  for (const panel of chat.querySelectorAll('.tool-panel')) {
    if (panel.dataset.toolId !== id || panel.dataset.toolHasResult === 'true') {
      continue;
    }
    const statusText = `执行中 ${seconds}s`;
    panel.dataset.toolStatus = statusText;
    const statusEl = panel.querySelector('.tool-status');
    if (statusEl) {
      statusEl.textContent = statusText;
    }
    if (activeToolPanel === panel) {
      syncToolDrawer(panel);
    }
    return;
  }
}

function addToolResult(name, result, id, durationMs = null) {
  const panels = chat.querySelectorAll('.tool-panel');
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
      if (activeToolPanel === p) {
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
    <div class="tool-header" onclick="openToolDrawerFromHeader(this)">
      <span class="tool-icon">📋</span>
      <span class="tool-name">${escHtml(name)} result</span>
      <span class="tool-status">${escHtml(el.dataset.toolStatus)}</span>
    </div>
  `;
  el.classList.add('tool-panel-ready');
  chat.appendChild(el);
  animatePanelIn(el);
  scrollDown();
}

function renderMarkdown(el) {
  const raw = el._rawText || el.textContent;
  const html = marked.parse(raw);
  el.innerHTML = typeof DOMPurify !== 'undefined' ? DOMPurify.sanitize(html) : html;
  el._markdownIdleHandle = 0;

  decorateCodeBlocks(el);
  scheduleCodeHighlight(el.querySelectorAll('pre code'));
}

// ── Session sidebar ──
function renderSessionList() {
  sessionList.innerHTML = '';
  sessions.sort((a, b) => (b.updated_at || b.created_at || 0) - (a.updated_at || a.created_at || 0));
  for (const s of sessions) {
    const item = document.createElement('div');
    item.className = `session-item${s.id === currentSessionId ? ' active' : ''}`;
    const ts = s.updated_at || s.created_at;
    item.innerHTML = `
      <div class="session-top">
        <span class="name">${escHtml(s.name)}</span>
        <span class="meta">${s.messages || 0}msg</span>
      </div>
      <span class="session-time">${ts ? timeAgo(ts) : ''}</span>
    `;
    item.onclick = () => {
      if (s.id !== currentSessionId) {
        sendCmd(`/switch ${s.id}`);
      }
    };
    sessionList.appendChild(item);
  }
  const active = sessions.find(s => s.id === currentSessionId);
  if (active) {
    sessionNameEl.textContent = active.name;
    sessionIdEl.textContent = active.id.slice(0, 12);
  }
}

function newSession() {
  if (confirm('开启新的对话？')) {
    sendCmd('/session_new');
  }
}

function toggleSidebar() {
  const sidebar = document.getElementById('sidebar');
  sidebar.classList.toggle('collapsed');
  const btn = document.querySelector('.toggle-sidebar');
  btn.setAttribute('aria-expanded', !sidebar.classList.contains('collapsed'));
}

// ── Helpers ──
function fallbackCopy(text) {
  const ta = document.createElement('textarea');
  ta.value = text;
  ta.style.cssText = 'position:fixed;left:-9999px';
  document.body.appendChild(ta);
  ta.select();
  document.execCommand('copy');
  document.body.removeChild(ta);
}

function escHtml(s) {
  return s.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;').replace(/'/g,'&#39;');
}
function truncateStr(s, max) {
  return s.length > max ? s.slice(0, max) + '…' : s;
}
function scrollDown() { chat.scrollTop = chat.scrollHeight; }

function setAssistantAvatar(node) {
  node.replaceChildren();
  const img = document.createElement('img');
  img.src = DEFAULT_BRAND_AVATAR;
  img.alt = 'LingClaw avatar';
  img.style.cssText = 'width:100%;height:100%;border-radius:50%;object-fit:cover';
  img.onerror = () => {
    node.replaceChildren();
    node.textContent = '🦀';
  };
  node.appendChild(img);
}

function formatTime(d) {
  return d.toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' });
}

function timeAgo(ts) {
  const diff = Date.now() - ts * 1000;
  const sec = Math.floor(diff / 1000);
  if (sec < 60) return '刚刚';
  const min = Math.floor(sec / 60);
  if (min < 60) return `${min}分钟前`;
  const hr = Math.floor(min / 60);
  if (hr < 24) return `${hr}小时前`;
  const day = Math.floor(hr / 24);
  if (day < 30) return `${day}天前`;
  return new Date(ts * 1000).toLocaleDateString('zh-CN');
}

function hideWelcome() {
  const w = document.getElementById('welcome');
  if (w) w.remove();
}

function showWelcome() {
  if (document.getElementById('welcome')) return;
  const w = document.createElement('div');
  w.className = 'welcome';
  w.id = 'welcome';
  w.innerHTML = `
    <div class="welcome-logo"><img src="${DEFAULT_WELCOME_LOGO}" alt="LingClaw"></div>
    <div class="welcome-title">LingClaw</div>
    <div class="welcome-hint">
      你的私人 AI 助手已就绪<br>
      输入消息开始对话，或使用 <strong>/</strong> 命令
    </div>
    <div class="welcome-shortcuts">
      <button onclick="sendCmd('/status')">📊 Status</button>
      <button onclick="sendCmd('/help')">❓ Help</button>
      <button onclick="newSession()">✨ New Chat</button>
    </div>
  `;
  chat.appendChild(w);
}

function setBusy(b) {
  busy = b;
  sendBtn.disabled = b;
  sendIcon.innerHTML = b ? '<span class="spinner"></span>' : '↑';
}

function syncToolDrawer(panel) {
  if (!panel || !toolDrawer) return;
  const toolName = panel.dataset.toolName || 'Tool';
  const toolArgs = panel.dataset.toolArgs || '';
  const toolResult = panel.dataset.toolResult || '';
  const hasResult = panel.dataset.toolHasResult === 'true';
  const statusText = panel.dataset.toolStatus || (hasResult ? '已返回结果' : '执行中');

  toolDrawerTitle.textContent = toolName;
  toolDrawerMeta.textContent = statusText;
  toolDrawerArgs.textContent = toolArgs || '(empty)';
  toolDrawerResult.textContent = toolResult;
  toolDrawerResultSection.hidden = !hasResult;
}

function openToolDrawer(panel) {
  if (!panel || !toolDrawer || !toolDrawerBackdrop) return;
  syncToolDrawerBounds();
  if (activeToolPanel && activeToolPanel !== panel) {
    activeToolPanel.classList.remove('tool-panel-active');
  }
  activeToolPanel = panel;
  activeToolPanel.classList.add('tool-panel-active');
  syncToolDrawer(panel);
  toolDrawer.classList.add('open');
  toolDrawerBackdrop.classList.add('open');
  toolDrawer.setAttribute('aria-hidden', 'false');
}

function openToolDrawerFromHeader(header) {
  openToolDrawer(header.closest('.tool-panel'));
}

function closeToolDrawer() {
  if (!toolDrawer || !toolDrawerBackdrop) return;
  toolDrawer.classList.remove('open');
  toolDrawerBackdrop.classList.remove('open');
  toolDrawer.setAttribute('aria-hidden', 'true');
  if (activeToolPanel) {
    activeToolPanel.classList.remove('tool-panel-active');
    activeToolPanel = null;
  }
}

function toggleTool(header) {
  const chevron = header.querySelector('.chevron');
  const body = header.nextElementSibling;
  const nextOpen = !body.classList.contains('show');
  if (chevron) chevron.classList.toggle('open', nextOpen);
  animateCollapsibleSection(body, nextOpen);
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
      }
    }
  }

  return startIdx;
}

function renderHistoryMessage(m, options = {}) {
  const { followMarkdown = true } = options;
  switch (m.role) {
    case 'user': addMsg('user', m.content, m.timestamp); break;
    case 'assistant': {
      const el = addMsg('assistant', m.content, m.timestamp);
      el._rawText = m.content;
      scheduleMarkdownRender(el, { followScroll: followMarkdown });
      break;
    }
    case 'tool_call': if (showTools) addToolCall(m.name, m.arguments, m.id); break;
    case 'tool_result': if (showTools) addToolResult('', m.result, m.id); break;
  }
}

function loadEarlierMessages() {
  const msgs = _deferredHistory;
  _deferredHistory = [];
  const loadMoreRow = document.getElementById('load-more-row');
  // The first child after load-more-row is the anchor we want to scroll to
  const anchor = loadMoreRow ? loadMoreRow.nextElementSibling : chat.firstElementChild;
  if (loadMoreRow) loadMoreRow.remove();
  const existing = [...chat.children];
  chat.replaceChildren();
  chat.classList.add('no-animate');
  for (const m of msgs) renderHistoryMessage(m, { followMarkdown: false });
  for (const el of existing) chat.appendChild(el);
  requestAnimationFrame(() => {
    chat.classList.remove('no-animate');
    if (anchor) anchor.scrollIntoView({ block: 'start' });
  });
}

// ── Input ──
function send() {
  const text = input.value.trim();
  if (!text || busy || !ws || ws.readyState !== 1) return;

  if (!text.startsWith('/')) {
    addMsg('user', text);
  }

  setBusy(true);
  ws.send(text);
  input.value = '';
  input.style.height = 'auto';
  syncToolDrawerBounds();
}

function sendCmd(cmd) {
  if ((!canSendWhileBusy(cmd) && busy) || !ws || ws.readyState !== 1) return;
  setBusy(true);
  ws.send(cmd);
}

updateViewToggleButtons();
syncToolDrawerBounds();

input.addEventListener('keydown', (e) => {
  if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); send(); }
});
document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') {
    closeToolDrawer();
  }
});
input.addEventListener('input', () => {
  input.style.height = 'auto';
  input.style.height = Math.min(input.scrollHeight, 120) + 'px';
  syncToolDrawerBounds();
});
window.addEventListener('resize', syncToolDrawerBounds);
if (window.visualViewport) {
  window.visualViewport.addEventListener('resize', syncToolDrawerBounds);
  window.visualViewport.addEventListener('scroll', syncToolDrawerBounds);
}
sendBtn.addEventListener('click', send);

connect();
