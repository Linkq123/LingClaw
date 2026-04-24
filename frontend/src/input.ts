import { dom, state } from './state.js';
import { INPUT_HISTORY_MAX } from './constants.js';
import { canSendWhileBusy } from './utils.js';
import { syncToolDrawerBounds, scrollDown } from './scroll.js';
import { addMsg, addSystem, setBusy, renderUserImageThumbnails } from './renderers/chat.js';
import { renderImagePreviews, uploadLocalImages } from './images.js';

// Guard: prevent double-registration on Vite HMR re-execution of main.ts.
let _listenerInit = false;

export function send() {
  if (!state.ws || state.ws.readyState !== 1) return;

  const text = dom.input.value.trim();
  if (!text && state.pendingImages.length === 0) return;

  if (text.startsWith('/') && state.pendingImages.length === 0) {
    if (state.busy && !canSendWhileBusy(text)) {
      addSystem(
        'Agent \u8fd0\u884c\u4e2d\u65f6\uff0c\u53ea\u5141\u8bb8 /stop\u3001/tool \u548c /reasoning\u3002',
      );
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

  const el = addMsg('user', text || '(image)', undefined);
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
  if (_listenerInit) return;
  _listenerInit = true;
  dom.input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      send();
      return;
    }
    if (
      (e.key === 'ArrowUp' || e.key === 'ArrowDown') &&
      !e.shiftKey &&
      state.inputHistory.length > 0
    ) {
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
  dom.sendBtn.addEventListener('click', () => {
    send();
  });
  dom.stopBtn.addEventListener('click', () => {
    stopAgent();
  });

  // ── Clipboard paste: extract image blobs and route through the same
  //    upload path as the file picker. Text paste is left untouched so that
  //    mixed text+image clipboards (e.g. Markdown with a screenshot) still
  //    paste the text into the textarea. ──
  dom.input.addEventListener('paste', (e: ClipboardEvent) => {
    if (!state.imageCapable) return;
    const items = e.clipboardData?.items;
    if (!items || items.length === 0) return;
    const files: File[] = [];
    for (const item of items) {
      if (item.kind === 'file' && item.type.startsWith('image/')) {
        const f = item.getAsFile();
        if (f) files.push(f);
      }
    }
    if (files.length === 0) return;
    // Prevent the browser from also inserting the image as inline base64
    // HTML / file name into the textarea.
    e.preventDefault();
    void uploadLocalImages(files);
  });

  // ── Global drag-and-drop dropzone. We attach to document so the user can
  //    drop anywhere in the window; visual feedback is driven by a class on
  //    the chat container. `dragenter` uses a counter because `dragleave`
  //    fires when the pointer crosses any child boundary. ──
  initDropzone();
}

let dragCounter = 0;
function hasFileDrop(dt: DataTransfer | null): boolean {
  if (!dt) return false;
  if (dt.items && dt.items.length > 0) {
    for (const item of dt.items) {
      if (item.kind === 'file') return true;
    }
  }
  return Boolean(dt.types && Array.from(dt.types).includes('Files'));
}

function hasImageFiles(dt: DataTransfer | null): boolean {
  if (!dt) return false;
  // DataTransferItemList exposes types during dragover without revealing
  // file contents (per the HTML spec); we can still filter by MIME type.
  if (dt.items && dt.items.length > 0) {
    let sawFile = false;
    let sawTypedFile = false;
    for (const item of dt.items) {
      if (item.kind !== 'file') continue;
      sawFile = true;
      if (item.type) sawTypedFile = true;
      if (item.kind === 'file' && item.type.startsWith('image/')) return true;
    }
    return sawFile && !sawTypedFile && Boolean(dt.types && Array.from(dt.types).includes('Files'));
  }
  if (dt.types && Array.from(dt.types).includes('Files')) return true;
  return false;
}

function initDropzone(): void {
  if (!dom.chat) return;
  const target = document;

  target.addEventListener('dragenter', (e) => {
    if (!state.imageCapable) return;
    if (!hasImageFiles(e.dataTransfer)) return;
    dragCounter += 1;
    dom.chat.classList.add('dropzone-active');
  });

  target.addEventListener('dragover', (e) => {
    const isImageDrop = state.imageCapable && hasImageFiles(e.dataTransfer);
    if (!isImageDrop && !hasFileDrop(e.dataTransfer)) return;
    e.preventDefault();
    // Required to allow drop. Use 'copy' so the OS cursor shows a plus sign
    // regardless of whether the file originated from another app (move) or
    // a browser image (link).
    if (e.dataTransfer) e.dataTransfer.dropEffect = isImageDrop ? 'copy' : 'none';
  });

  target.addEventListener('dragleave', () => {
    if (dragCounter > 0) dragCounter -= 1;
    if (dragCounter === 0) dom.chat.classList.remove('dropzone-active');
  });

  target.addEventListener('drop', (e) => {
    const wasActive = dom.chat.classList.contains('dropzone-active');
    dragCounter = 0;
    dom.chat.classList.remove('dropzone-active');
    if (hasFileDrop(e.dataTransfer)) e.preventDefault();
    if (!state.imageCapable) return;
    if (!wasActive && !hasImageFiles(e.dataTransfer)) return;
    const files: File[] = [];
    const dt = e.dataTransfer;
    if (dt?.files) {
      for (const f of dt.files) {
        if (f.type.startsWith('image/')) files.push(f);
      }
    }
    if (files.length === 0) return;
    void uploadLocalImages(files);
  });
}
