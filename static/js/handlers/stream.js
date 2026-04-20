import { state } from '../state.js';
import { addAssistant } from '../renderers/chat.js';
import { scrollDown, isChatNearBottom } from '../scroll.js';
import {
  findProgressiveSplitPoint, appendRenderedSegment, updateLiveTail,
  removeLiveTail, scheduleMarkdownRender
} from '../markdown.js';

function currentMsgRow() {
  return state.currentMsg ? state.currentMsg.closest('.msg-row') : null;
}

function flushAssistantText() {
  if (!state.currentMsg || !state.pendingAssistantText) return;
  state.currentMsg._rawText = (state.currentMsg._rawText || '') + state.pendingAssistantText;
  state.pendingAssistantText = '';

  const raw = state.currentMsg._rawText;
  const offset = state.currentMsg._renderedOffset || 0;
  const splitAt = findProgressiveSplitPoint(raw);

  if (splitAt > offset) {
    appendRenderedSegment(state.currentMsg, raw.substring(offset, splitAt));
    state.currentMsg._renderedOffset = splitAt;
    updateLiveTail(state.currentMsg, raw.substring(splitAt));
  } else if (offset > 0) {
    updateLiveTail(state.currentMsg, raw.substring(offset));
  } else {
    updateLiveTail(state.currentMsg, raw);
  }
  revealCurrentAssistant();
}

function flushReasoningText() {
  if (!state.reasoningPanel || !state.pendingReasoningText) return;
  const body = state.reasoningPanel.querySelector('.reasoning-body');
  if (!body) { state.pendingReasoningText = ''; return; }
  if (!body._textNode) {
    body._textNode = document.createTextNode(state.pendingReasoningText);
    body.appendChild(body._textNode);
  } else {
    body._textNode.nodeValue += state.pendingReasoningText;
  }
  state.pendingReasoningText = '';
}

function cancelFlushIfIdle() {
  if (!state.pendingAssistantText && !state.pendingReasoningText && state.flushHandle) {
    cancelAnimationFrame(state.flushHandle);
    state.flushHandle = 0;
  }
}

function flushStreaming() {
  state.flushHandle = 0;
  const follow = state.autoFollowChat || isChatNearBottom();
  flushAssistantText();
  flushReasoningText();
  if (follow) scrollDown();
}

export function scheduleFlush() {
  if (!state.flushHandle) {
    state.flushHandle = requestAnimationFrame(flushStreaming);
  }
}

function cancelAssistantFlush() {
  state.pendingAssistantText = '';
  cancelFlushIfIdle();
}

function cancelReasoningFlush() {
  state.pendingReasoningText = '';
  cancelFlushIfIdle();
}

function revealCurrentAssistant() {
  const row = currentMsgRow();
  if (row) {
    row.hidden = false;
  }
}

export function beginAssistantStream() {
  cancelAssistantFlush();
  const message = addAssistant('', { trackUnread: false });
  const row = message.closest('.msg-row');
  if (row) {
    row.hidden = true;
  }
  message.classList.add('typing');
  message._rawText = '';
  message._renderedOffset = 0;
  state.currentMsg = message;
}

export function finishAssistantStream({ discardIfEmpty = false } = {}) {
  flushAssistantText();
  if (!state.currentMsg) {
    return null;
  }

  const message = state.currentMsg;
  const row = currentMsgRow();
  const rawText = state.currentMsg._rawText || '';
  const raw = rawText.trim();
  state.currentMsg.classList.remove('typing');

  if (!raw && discardIfEmpty) {
    row?.remove();
    state.currentMsg = null;
    return null;
  }

  if (!raw) {
    row?.removeAttribute('hidden');
    state.currentMsg = null;
    return null;
  }

  revealCurrentAssistant();

  const offset = state.currentMsg._renderedOffset || 0;
  if (offset > 0) {
    removeLiveTail(state.currentMsg);
    const tail = rawText.substring(offset);
    if (tail) {
      appendRenderedSegment(state.currentMsg, tail);
    }
  } else {
    scheduleMarkdownRender(state.currentMsg);
  }
  state.currentMsg = null;
  return message;
}

export function finishReasoningStream() {
  flushReasoningText();
  cancelReasoningFlush();
  if (state.reasoningPanel) {
    const body = state.reasoningPanel.querySelector('.reasoning-body');
    if (body && body.classList.contains('show')) {
      body.style.height = 'auto';
    }
  }
  scrollDown();
}
