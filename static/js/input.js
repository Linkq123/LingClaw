import { dom, state } from './state.js';
import { INPUT_HISTORY_MAX } from './constants.js';
import { canSendWhileBusy } from './utils.js';
import { syncToolDrawerBounds, scrollDown } from './scroll.js';
import { addMsg, addSystem, setBusy, renderUserImageThumbnails } from './renderers/chat.js';
import { scheduleMarkdownRender } from './markdown.js';
import { renderImagePreviews } from './images.js';

export function send() {
  if (!state.ws || state.ws.readyState !== 1) return;

  const text = dom.input.value.trim();
  if (!text && state.pendingImages.length === 0) return;

  if (text.startsWith('/') && state.pendingImages.length === 0) {
    if (state.busy && !canSendWhileBusy(text)) {
      addSystem('Agent \u8fd0\u884c\u4e2d\u65f6\uff0c\u53ea\u5141\u8bb8 /stop\u3001/tool \u548c /reasoning\u3002');
      return;
    }
    sendCmd(text);
    pushInputHistory(text);
    dom.input.value = '';
    dom.input.style.height = 'auto';
    syncToolDrawerBounds();
    return;
  }

  const hasImages = state.pendingImages.length > 0;
  const effectiveImages = state.busy ? [] : state.pendingImages.slice();

  const el = addMsg('user', text || '(image)');
  if (effectiveImages.length > 0) {
    renderUserImageThumbnails(el, effectiveImages);
  }
  scrollDown(true);

  if (!state.busy) {
    setBusy(true);
  }

  if (hasImages) {
    state.ws.send(JSON.stringify({ text: text || '', images: state.pendingImages }));
    state.pendingImages = [];
    renderImagePreviews();
  } else {
    state.ws.send(text);
  }
  pushInputHistory(text);
  dom.input.value = '';
  dom.input.style.height = 'auto';
  syncToolDrawerBounds();
}

export function pushInputHistory(text) {
  if (!text) return;
  if (state.inputHistory.length > 0 && state.inputHistory[state.inputHistory.length - 1] === text) {
    state.inputHistoryIndex = -1;
    return;
  }
  state.inputHistory.push(text);
  if (state.inputHistory.length > INPUT_HISTORY_MAX) state.inputHistory.shift();
  state.inputHistoryIndex = -1;
}

export function stopAgent() {
  if (!state.busy || !state.ws || state.ws.readyState !== 1) return;
  state.ws.send('/stop');
}

export function sendCmd(cmd) {
  if ((!canSendWhileBusy(cmd) && state.busy) || !state.ws || state.ws.readyState !== 1) return;
  setBusy(true);
  state.ws.send(cmd);
}

export function initInputListeners() {
  dom.input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); send(); return; }
    if ((e.key === 'ArrowUp' || e.key === 'ArrowDown') && !e.shiftKey && state.inputHistory.length > 0) {
      const val = dom.input.value;
      const pos = dom.input.selectionStart;
      if (e.key === 'ArrowUp') {
        const textBefore = val.slice(0, pos);
        if (textBefore.includes('\n')) return;
        e.preventDefault();
        if (state.inputHistoryIndex === -1) {
          state.inputHistoryDraft = val;
          state.inputHistoryIndex = state.inputHistory.length - 1;
        } else if (state.inputHistoryIndex > 0) {
          state.inputHistoryIndex--;
        }
        dom.input.value = state.inputHistory[state.inputHistoryIndex];
        dom.input.setSelectionRange(dom.input.value.length, dom.input.value.length);
      } else {
        const textAfter = val.slice(pos);
        if (textAfter.includes('\n')) return;
        e.preventDefault();
        if (state.inputHistoryIndex === -1) return;
        if (state.inputHistoryIndex < state.inputHistory.length - 1) {
          state.inputHistoryIndex++;
          dom.input.value = state.inputHistory[state.inputHistoryIndex];
        } else {
          state.inputHistoryIndex = -1;
          dom.input.value = state.inputHistoryDraft;
        }
        dom.input.setSelectionRange(dom.input.value.length, dom.input.value.length);
      }
      dom.input.style.height = 'auto';
      dom.input.style.height = Math.min(dom.input.scrollHeight, 120) + 'px';
    }
  });
  dom.input.addEventListener('input', () => {
    dom.input.style.height = 'auto';
    dom.input.style.height = Math.min(dom.input.scrollHeight, 120) + 'px';
    syncToolDrawerBounds();
  });
  dom.sendBtn.addEventListener('click', () => { send(); });
  dom.stopBtn.addEventListener('click', () => { stopAgent(); });
}
