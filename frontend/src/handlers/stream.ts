import { state } from '../state.js';
import { addAssistant } from '../renderers/chat.js';
import { scrollDown, isChatNearBottom, invalidateChatScrollCache } from '../scroll.js';
import {
  findProgressiveSplitPoint,
  appendRenderedSegment,
  needsFinalMarkdownRender,
  updateLiveTail,
  removeLiveTail,
  renderMarkdown,
} from '../markdown.js';

function revealAssistantMessage(message) {
  const row = message ? message.closest('.msg-row') : null;
  if (row) {
    row.hidden = false;
  }
}

let flushInProgress = false;
let flushRequested = false;
let assistantRenderChain: Promise<void> = Promise.resolve();

function enqueueAssistantRender(work: () => Promise<void> | void): Promise<void> {
  const run = async () => {
    try {
      await work();
    } catch (error) {
      console.warn('Assistant render step failed:', error);
    }
  };
  assistantRenderChain = assistantRenderChain.then(run, run);
  return assistantRenderChain;
}

async function flushAssistantText() {
  if (!state.currentMsg || !state.pendingAssistantText) return;
  mergePendingAssistantText();

  const message = state.currentMsg;
  const raw = message._rawText;
  const offset = message._renderedOffset || 0;
  const splitAt = findProgressiveSplitPoint(raw, offset);

  if (splitAt > offset) {
    await enqueueAssistantRender(async () => {
      const renderedSegment = raw.substring(offset, splitAt);
      const liveTail = raw.substring(splitAt);
      try {
        await appendRenderedSegment(message, renderedSegment);
        message._renderedOffset = splitAt;
        updateLiveTail(message, liveTail);
      } catch (error) {
        message._renderedOffset = offset;
        updateLiveTail(message, raw.substring(offset));
        throw error;
      }
      revealAssistantMessage(message);
    });
  } else if (offset > 0) {
    await enqueueAssistantRender(() => {
      updateLiveTail(message, raw.substring(offset));
      revealAssistantMessage(message);
    });
  } else {
    await enqueueAssistantRender(() => {
      updateLiveTail(message, raw);
      revealAssistantMessage(message);
    });
  }
}

function mergePendingAssistantText() {
  if (!state.currentMsg || !state.pendingAssistantText) return;
  state.currentMsg._rawText = (state.currentMsg._rawText || '') + state.pendingAssistantText;
  state.pendingAssistantText = '';
}

function flushReasoningText() {
  if (!state.reasoningPanel || !state.pendingReasoningText) return;
  const body = state.reasoningPanel.querySelector('.reasoning-body');
  if (!body) {
    state.pendingReasoningText = '';
    return;
  }
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

async function flushStreaming() {
  if (flushInProgress) {
    flushRequested = true;
    return;
  }
  flushInProgress = true;
  state.flushHandle = 0;
  const follow = state.autoFollowChat || isChatNearBottom();
  try {
    await flushAssistantText();
    flushReasoningText();
    invalidateChatScrollCache();
    if (follow) scrollDown();
  } finally {
    flushInProgress = false;
    if (flushRequested || state.pendingAssistantText || state.pendingReasoningText) {
      flushRequested = false;
      scheduleFlush();
    }
  }
}

export function scheduleFlush() {
  if (flushInProgress) {
    flushRequested = true;
    return;
  }
  if (!state.flushHandle) {
    state.flushHandle = requestAnimationFrame(() => {
      void flushStreaming();
    });
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
  mergePendingAssistantText();
  if (!state.currentMsg) {
    return null;
  }

  const message = state.currentMsg;
  const row = message.closest('.msg-row');
  const rawText = message._rawText || '';
  const raw = rawText.trim();
  message.classList.remove('typing');

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

  revealAssistantMessage(message);

  void enqueueAssistantRender(async () => {
    const offset = message._renderedOffset || 0;
    if (offset > 0) {
      const tail = rawText.substring(offset);
      if (tail) {
        try {
          await appendRenderedSegment(message, tail);
        } catch (error) {
          updateLiveTail(message, tail);
          throw error;
        }
        removeLiveTail(message);
        invalidateChatScrollCache();
      } else {
        removeLiveTail(message);
      }
      if (needsFinalMarkdownRender(message, rawText)) {
        await renderMarkdown(message);
      } else {
        message._markdownRenderedRaw = rawText;
      }
      message._renderedOffset = rawText.length;
      return;
    }

    if (message._rawText === rawText) {
      await renderMarkdown(message);
      message._renderedOffset = rawText.length;
      invalidateChatScrollCache();
    }
  });
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
