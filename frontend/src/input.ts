import { dom, state } from './state.js';
import { INPUT_HISTORY_MAX } from './constants.js';
import { canSendWhileBusy } from './utils.js';
import { syncToolDrawerBounds, scrollDown } from './scroll.js';
import { addMsg, addSystem, setBusy, renderUserImageThumbnails } from './renderers/chat.js';
import { renderImagePreviews, uploadLocalImages } from './images.js';
import {
  buildSlashCommandInput,
  getSlashCommandMenuState,
  isBusyAllowedSlashCommand,
  normalizeSlashCommandText,
  type SlashCommandSpec,
} from './slashCommands.js';
import { tr } from './i18n.js';

// Guard: prevent double-registration on Vite HMR re-execution of main.ts.
let _listenerInit = false;
let slashMenuSuggestions: SlashCommandSpec[] = [];
let slashMenuActiveIndex = 0;

function closeSlashCommandMenu() {
  const menu = dom.slashCommandMenu;
  if (!menu || menu.hidden) {
    slashMenuSuggestions = [];
    slashMenuActiveIndex = 0;
    return;
  }
  menu.hidden = true;
  menu.replaceChildren();
  slashMenuSuggestions = [];
  slashMenuActiveIndex = 0;
  syncToolDrawerBounds();
}

function applySlashCommandSuggestion(spec: SlashCommandSpec) {
  if (!dom.input) return;
  dom.input.value = buildSlashCommandInput(dom.input.value, spec);
  dom.input.focus();
  dom.input.setSelectionRange(dom.input.value.length, dom.input.value.length);
  dom.input.style.height = 'auto';
  dom.input.style.height = Math.min(dom.input.scrollHeight, 120) + 'px';
  closeSlashCommandMenu();
  syncToolDrawerBounds();
}

function scrollActiveSlashCommandIntoView(menu: HTMLElement) {
  const activeItem = menu.querySelector<HTMLElement>('.slash-command-item.is-active');
  if (activeItem && typeof activeItem.scrollIntoView === 'function') {
    activeItem.scrollIntoView({ block: 'nearest' });
  }
}

function syncSlashCommandMenuSelection(menu: HTMLElement) {
  const items = menu.querySelectorAll<HTMLElement>('.slash-command-item');
  items.forEach((item, index) => {
    const isActive = index === slashMenuActiveIndex;
    item.classList.toggle('is-active', isActive);
    item.setAttribute('aria-selected', isActive ? 'true' : 'false');
  });
  scrollActiveSlashCommandIntoView(menu);
}

function renderSlashCommandMenu() {
  const menu = dom.slashCommandMenu;
  if (!menu || !dom.input) return;

  const nextState = getSlashCommandMenuState(dom.input.value);
  if (!nextState) {
    closeSlashCommandMenu();
    return;
  }

  const previousActiveCommand = slashMenuSuggestions[slashMenuActiveIndex]?.command;
  slashMenuSuggestions = nextState.suggestions;

  if (slashMenuSuggestions.length === 0) {
    slashMenuActiveIndex = 0;
  } else {
    const previousIndex = previousActiveCommand
      ? slashMenuSuggestions.findIndex((spec) => spec.command === previousActiveCommand)
      : -1;
    slashMenuActiveIndex =
      previousIndex >= 0
        ? previousIndex
        : Math.min(slashMenuActiveIndex, slashMenuSuggestions.length - 1);
  }

  const fragment = document.createDocumentFragment();
  if (slashMenuSuggestions.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'slash-command-empty';
    empty.textContent = tr('slash.noMatches');
    fragment.appendChild(empty);
  } else {
    for (const [index, spec] of slashMenuSuggestions.entries()) {
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'slash-command-item';
      button.dataset.slashCommand = spec.command;
      button.dataset.slashIndex = String(index);
      button.addEventListener('mouseenter', () => {
        slashMenuActiveIndex = index;
        syncSlashCommandMenuSelection(menu);
      });
      button.addEventListener('mousedown', (event) => {
        event.preventDefault();
      });
      button.addEventListener('click', () => {
        applySlashCommandSuggestion(spec);
      });

      const commandRow = document.createElement('div');
      commandRow.className = 'slash-command-item-command';
      commandRow.textContent = spec.args ? `${spec.command} ${spec.args}` : spec.command;
      if (isBusyAllowedSlashCommand(spec)) {
        const badge = document.createElement('span');
        badge.className = 'slash-command-item-badge';
        badge.textContent = tr('common.live');
        commandRow.appendChild(badge);
      }

      const description = document.createElement('div');
      description.className = 'slash-command-item-description';
      description.textContent = spec.description();

      button.append(commandRow, description);
      fragment.appendChild(button);
    }
  }

  menu.hidden = false;
  menu.replaceChildren(fragment);
  syncSlashCommandMenuSelection(menu);
  syncToolDrawerBounds();
}

function moveSlashCommandSelection(direction: 1 | -1) {
  if (slashMenuSuggestions.length === 0) return;
  slashMenuActiveIndex =
    (slashMenuActiveIndex + direction + slashMenuSuggestions.length) % slashMenuSuggestions.length;
  if (dom.slashCommandMenu) {
    syncSlashCommandMenuSelection(dom.slashCommandMenu);
  }
}

function applyPendingSlashCommandSuggestion(text: string): boolean {
  const menuState = getSlashCommandMenuState(text);
  if (!menuState || menuState.exactMatch || menuState.suggestions.length === 0) {
    return false;
  }

  const activeCommand = slashMenuSuggestions[slashMenuActiveIndex]?.command;
  const suggestion =
    (activeCommand
      ? menuState.suggestions.find((spec) => spec.command === activeCommand)
      : undefined) ??
    menuState.suggestions[Math.min(slashMenuActiveIndex, menuState.suggestions.length - 1)];

  if (!suggestion) {
    return false;
  }

  applySlashCommandSuggestion(suggestion);
  return true;
}

function handleSlashCommandKeydown(e: KeyboardEvent): boolean {
  if (!dom.input || dom.slashCommandMenu?.hidden !== false) return false;
  const menuState = getSlashCommandMenuState(dom.input.value);
  if (!menuState) {
    closeSlashCommandMenu();
    return false;
  }

  if (e.key === 'ArrowDown') {
    if (slashMenuSuggestions.length === 0) {
      closeSlashCommandMenu();
      return false;
    }
    e.preventDefault();
    moveSlashCommandSelection(1);
    return true;
  }

  if (e.key === 'ArrowUp') {
    if (slashMenuSuggestions.length === 0) {
      closeSlashCommandMenu();
      return false;
    }
    e.preventDefault();
    moveSlashCommandSelection(-1);
    return true;
  }

  if (e.key === 'Tab') {
    if (slashMenuSuggestions.length === 0) return false;
    e.preventDefault();
    applySlashCommandSuggestion(slashMenuSuggestions[slashMenuActiveIndex]);
    return true;
  }

  if (e.key === 'Escape') {
    e.preventDefault();
    closeSlashCommandMenu();
    return true;
  }

  if (e.key === 'Enter' && !e.shiftKey && applyPendingSlashCommandSuggestion(dom.input.value)) {
    e.preventDefault();
    return true;
  }

  return false;
}

export function send() {
  if (!state.ws || state.ws.readyState !== 1) return;

  const text = dom.input.value.trim();
  if (!text && state.pendingImages.length === 0) return;

  if (state.activeGroupId) {
    if (text.startsWith('/') && state.pendingImages.length === 0) {
      addSystem(tr('group.slashUnsupported'));
      return;
    }
    if (state.pendingImages.length > 0) {
      addSystem(tr('group.imagesUnsupported'));
      return;
    }
    const targetMode = state.groupTargetMode || 'all';
    const activeMembers = new Set(state.activeGroupMembers);
    const targets =
      targetMode === 'selected'
        ? state.groupSelectedTargets.filter((target) => activeMembers.has(target))
        : [];
    if (targetMode === 'selected' && targets.length === 0) {
      addSystem(tr('group.selectMember'));
      return;
    }
    state.ws.send(
      JSON.stringify({
        type: 'group_message',
        text,
        targets,
        target_mode: targetMode,
        start_runs: true,
        run_mode: state.planModeEnabled ? 'plan_only' : 'execute',
      }),
    );
    if (!state.busy) {
      setBusy(true);
    }
    pushInputHistory(text);
    dom.input.value = '';
    dom.input.style.height = 'auto';
    closeSlashCommandMenu();
    syncToolDrawerBounds();
    return;
  }

  if (text.startsWith('/') && state.pendingImages.length === 0) {
    if (applyPendingSlashCommandSuggestion(text)) {
      return;
    }

    const commandText = normalizeSlashCommandText(text);
    if (state.busy && !canSendWhileBusy(commandText)) {
      addSystem(tr('slash.busyLimited'));
      return;
    }
    sendCmd(commandText);
    pushInputHistory(commandText);
    dom.input.value = '';
    dom.input.style.height = 'auto';
    closeSlashCommandMenu();
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

  const payload: { text: string; plan_mode: boolean; images?: typeof state.pendingImages } = {
    text: text || '',
    plan_mode: state.planModeEnabled,
  };
  if (hasImages) {
    payload.images = state.pendingImages;
    state.ws.send(JSON.stringify(payload));
    state.pendingImages = [];
    renderImagePreviews();
  } else {
    state.ws.send(JSON.stringify(payload));
  }
  pushInputHistory(text);
  dom.input.value = '';
  dom.input.style.height = 'auto';
  closeSlashCommandMenu();
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
  if (state.activeGroupId) {
    state.ws.send(JSON.stringify({ type: 'group_stop' }));
    return;
  }
  state.ws.send('/stop');
}

export function sendCmd(cmd) {
  const normalizedCmd = normalizeSlashCommandText(cmd.trim());
  if (state.activeGroupId) {
    addSystem(tr('group.slashUnsupported'));
    return;
  }
  if ((!canSendWhileBusy(normalizedCmd) && state.busy) || !state.ws || state.ws.readyState !== 1) {
    return;
  }
  setBusy(true);
  state.ws.send(normalizedCmd);
}

export function initInputListeners() {
  if (_listenerInit) return;
  _listenerInit = true;
  dom.input.addEventListener('keydown', (e) => {
    if (handleSlashCommandKeydown(e)) return;
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
    renderSlashCommandMenu();
    syncToolDrawerBounds();
  });
  dom.input.addEventListener('focus', () => {
    renderSlashCommandMenu();
  });
  dom.input.addEventListener('blur', () => {
    window.setTimeout(() => {
      if (dom.inputArea?.contains(document.activeElement)) return;
      closeSlashCommandMenu();
    }, 0);
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
