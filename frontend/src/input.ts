import { dom, state } from './state.js';
import { INPUT_HISTORY_MAX } from './constants.js';
import { canSendWhileBusy } from './utils.js';
import { syncToolDrawerBounds, scrollDown } from './scroll.js';
import { addMsg, addSystem, setBusy, renderUserImageThumbnails } from './renderers/chat.js';
import { renderImagePreviews, setPlanMode, uploadLocalImages } from './images.js';
import {
  buildSlashCommandInput,
  getSlashCommandMenuState,
  isBusyAllowedSlashCommand,
  normalizeSlashCommandText,
  type SlashCommandSpec,
} from './slashCommands.js';
import { tr } from './i18n.js';
import {
  filterGroupMentionMembers,
  findGroupMentionQuery,
  insertGroupMention,
  mentionedGroupTargets,
  type GroupMentionMember,
  type GroupMentionQuery,
} from './groupMentions.js';
import { renderSessionDrawer } from './renderers/sessions.js';
import {
  areGroupMessageTargetsModelReady,
  beginComposerSessionTransition,
  canBypassComposerModelGate,
  canSendWhileStorageProtected,
  isComposerModelReady,
  syncComposerAvailability,
} from './composerAvailability.js';

// Guard: prevent double-registration on Vite HMR re-execution of main.ts.
let _listenerInit = false;
let slashMenuSuggestions: SlashCommandSpec[] = [];
let slashMenuActiveIndex = 0;
let mentionMenuSuggestions: GroupMentionMember[] = [];
let mentionMenuActiveIndex = 0;
let activeMentionQuery: GroupMentionQuery | null = null;
let onGroupMentionTargetModeActivated: (() => void) | null = null;

export type InputListenerOptions = {
  onGroupMentionTargetModeActivated?: () => void;
};

export function activateGroupMentionTargetMode(): void {
  if (!state.activeGroupId) return;
  state.groupTargetMode = 'mentions';
  state.groupTargetPickerOpen = false;
  state.groupTargetSearchQuery = '';
  onGroupMentionTargetModeActivated?.();
}

function closeSlashCommandMenu() {
  const menu = dom.slashCommandMenu;
  slashMenuSuggestions = [];
  slashMenuActiveIndex = 0;
  mentionMenuSuggestions = [];
  mentionMenuActiveIndex = 0;
  activeMentionQuery = null;
  if (dom.input) {
    dom.input.removeAttribute('aria-activedescendant');
    dom.input.setAttribute('aria-expanded', 'false');
  }
  if (!menu || menu.hidden) return;
  menu.hidden = true;
  menu.replaceChildren();
  menu.classList.remove('mention-menu');
  menu.setAttribute('role', 'listbox');
  menu.setAttribute('aria-label', tr('slash.suggestions'));
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
  syncComposerAvailability();
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
  const activeItem = items[slashMenuActiveIndex];
  if (activeItem) dom.input?.setAttribute('aria-activedescendant', activeItem.id);
  else dom.input?.removeAttribute('aria-activedescendant');
  scrollActiveSlashCommandIntoView(menu);
}

function groupMentionRoleLabel(role?: string): string {
  if (role === 'admin') return tr('common.admin');
  if (role === 'owner') return tr('common.main');
  if (role === 'all') return tr('group.mentionEveryone');
  return tr('common.member');
}

function currentMentionMembers(): GroupMentionMember[] {
  const details = new Map(
    state.activeGroupMemberDetails.map((member) => [member.id, member] as const),
  );
  return state.activeGroupMembers.map((id) => {
    const detail = details.get(id);
    return {
      id,
      name: detail?.name || id,
      role: detail?.role || 'member',
    };
  });
}

function syncGroupMentionMenuSelection(menu: HTMLElement): void {
  const items = menu.querySelectorAll<HTMLElement>('.mention-menu-item');
  items.forEach((item, index) => {
    const active = index === mentionMenuActiveIndex;
    item.classList.toggle('is-active', active);
    item.setAttribute('aria-selected', active ? 'true' : 'false');
  });
  const activeItem = items[mentionMenuActiveIndex];
  if (activeItem) {
    dom.input?.setAttribute('aria-activedescendant', activeItem.id);
    if (typeof activeItem.scrollIntoView === 'function') {
      activeItem.scrollIntoView({ block: 'nearest' });
    }
  } else {
    dom.input?.removeAttribute('aria-activedescendant');
  }
}

function applyGroupMentionSuggestion(candidate: GroupMentionMember): void {
  if (!dom.input || !activeMentionQuery) return;
  const currentQuery = findGroupMentionQuery(
    dom.input.value,
    dom.input.selectionStart ?? dom.input.value.length,
  );
  if (!currentQuery) {
    closeSlashCommandMenu();
    return;
  }
  const replacement = insertGroupMention(dom.input.value, currentQuery, candidate.id);
  dom.input.value = replacement.value;
  activateGroupMentionTargetMode();
  dom.input.focus();
  dom.input.setSelectionRange(replacement.cursor, replacement.cursor);
  dom.input.style.height = 'auto';
  dom.input.style.height = Math.min(dom.input.scrollHeight, 120) + 'px';
  closeSlashCommandMenu();
  syncComposerAvailability();
  syncToolDrawerBounds();
}

function renderGroupMentionMenu(): boolean {
  const menu = dom.slashCommandMenu;
  const input = dom.input;
  if (!menu || !input || !state.activeGroupId) return false;

  const query = findGroupMentionQuery(input.value, input.selectionStart ?? input.value.length);
  if (!query) return false;
  activeMentionQuery = query;
  const previousId = mentionMenuSuggestions[mentionMenuActiveIndex]?.id;
  mentionMenuSuggestions = filterGroupMentionMembers(
    query.query,
    state.activeGroupMembers,
    currentMentionMembers(),
    tr('common.all'),
  );
  const previousIndex = previousId
    ? mentionMenuSuggestions.findIndex((candidate) => candidate.id === previousId)
    : -1;
  mentionMenuActiveIndex =
    previousIndex >= 0
      ? previousIndex
      : Math.min(mentionMenuActiveIndex, Math.max(0, mentionMenuSuggestions.length - 1));

  const fragment = document.createDocumentFragment();
  if (mentionMenuSuggestions.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'slash-command-empty mention-menu-empty';
    empty.textContent = tr('group.mentionNoMatches');
    fragment.appendChild(empty);
  } else {
    mentionMenuSuggestions.forEach((candidate, index) => {
      const button = document.createElement('button');
      button.type = 'button';
      button.id = `group-mention-option-${index}`;
      button.className = 'slash-command-item mention-menu-item';
      button.dataset.mentionId = candidate.id;
      button.setAttribute('role', 'option');
      button.addEventListener('mouseenter', () => {
        mentionMenuActiveIndex = index;
        syncGroupMentionMenuSelection(menu);
      });
      button.addEventListener('mousedown', (event) => event.preventDefault());
      button.addEventListener('click', () => applyGroupMentionSuggestion(candidate));

      const avatar = document.createElement('span');
      avatar.className = 'mention-menu-avatar';
      avatar.textContent = candidate.id === 'all' ? '@' : Array.from(candidate.name)[0] || '?';
      const copy = document.createElement('span');
      copy.className = 'mention-menu-copy';
      const name = document.createElement('span');
      name.className = 'mention-menu-name';
      name.textContent = candidate.name;
      const meta = document.createElement('span');
      meta.className = 'mention-menu-meta';
      meta.textContent = `@${candidate.id} · ${groupMentionRoleLabel(candidate.role)}`;
      copy.append(name, meta);
      button.append(avatar, copy);
      fragment.appendChild(button);
    });
  }

  slashMenuSuggestions = [];
  menu.classList.add('mention-menu');
  menu.setAttribute('role', 'listbox');
  menu.setAttribute('aria-label', tr('group.mentionSuggestions'));
  menu.hidden = false;
  menu.replaceChildren(fragment);
  input.setAttribute('aria-controls', menu.id);
  input.setAttribute('aria-expanded', 'true');
  syncGroupMentionMenuSelection(menu);
  syncToolDrawerBounds();
  return true;
}

function renderComposerSuggestionMenu(): void {
  if (renderGroupMentionMenu()) return;
  mentionMenuSuggestions = [];
  mentionMenuActiveIndex = 0;
  activeMentionQuery = null;
  renderSlashCommandMenu();
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
      button.id = `slash-command-option-${index}`;
      button.className = 'slash-command-item';
      button.setAttribute('role', 'option');
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

  menu.classList.remove('mention-menu');
  menu.setAttribute('role', 'listbox');
  menu.setAttribute('aria-label', tr('slash.suggestions'));
  menu.hidden = false;
  menu.replaceChildren(fragment);
  dom.input.setAttribute('aria-controls', menu.id);
  dom.input.setAttribute('aria-expanded', 'true');
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

function handleGroupMentionKeydown(e: KeyboardEvent): boolean {
  const menu = dom.slashCommandMenu;
  if (
    e.isComposing ||
    !menu ||
    menu.hidden ||
    !menu.classList.contains('mention-menu') ||
    !activeMentionQuery
  ) {
    return false;
  }

  if (e.key === 'Escape') {
    e.preventDefault();
    closeSlashCommandMenu();
    return true;
  }
  if (mentionMenuSuggestions.length === 0) return false;
  if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
    e.preventDefault();
    const direction = e.key === 'ArrowDown' ? 1 : -1;
    mentionMenuActiveIndex =
      (mentionMenuActiveIndex + direction + mentionMenuSuggestions.length) %
      mentionMenuSuggestions.length;
    syncGroupMentionMenuSelection(menu);
    return true;
  }
  if (e.key === 'Tab' || (e.key === 'Enter' && !e.shiftKey)) {
    e.preventDefault();
    applyGroupMentionSuggestion(mentionMenuSuggestions[mentionMenuActiveIndex]);
    return true;
  }
  return false;
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
  if (
    state.sessionSwitchInFlight ||
    state.sessionIdentityMutationInFlight ||
    state.composerModelSwitchInFlight
  )
    return;

  const text = dom.input.value.trim();
  if (!text && state.pendingImages.length === 0) return;
  const protectedReadOnlyCommand =
    state.storageMode === 'protected' && canSendWhileStorageProtected(text);
  if (state.storageMode === 'protected' && !protectedReadOnlyCommand) return;

  if (state.activeGroupId) {
    if (state.imageUploadInFlight) return;
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
    if (
      targetMode === 'mentions' &&
      mentionedGroupTargets(text, state.activeGroupMembers).length === 0
    ) {
      addSystem(tr('group.mentionRequired'));
      return;
    }
    if (!areGroupMessageTargetsModelReady(text)) return;
    state.ws.send(
      JSON.stringify({
        type: 'group_message',
        text,
        targets,
        target_mode: targetMode,
        start_runs: true,
        run_mode: 'execute',
      }),
    );
    if (!state.busy) {
      setBusy(true);
    }
    pushInputHistory(text);
    dom.input.value = '';
    dom.input.style.height = 'auto';
    closeSlashCommandMenu();
    syncComposerAvailability();
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
    const commandName = commandText.split(/\s+/, 1)[0].toLowerCase();
    const targetSessionId =
      commandName === '/switch' ? commandText.trim().split(/\s+/, 2)[1] || '' : '';
    if (targetSessionId && (state.composerSessionIdentityPending || state.imageUploadInFlight)) {
      return;
    }
    const modelGateBypassed = canBypassComposerModelGate(commandText);
    if (state.imageUploadInFlight && !modelGateBypassed) return;
    if (!isComposerModelReady() && !modelGateBypassed) return;
    if (commandName === '/switch') {
      if (targetSessionId) {
        beginComposerSessionTransition(true, targetSessionId);
        renderSessionDrawer();
      }
    }
    if (commandName === '/clear' || commandName === '/new') {
      // These commands replace the current transcript and remove its active
      // plan. Switch optimistically so the following plan-less history stays
      // in Execute mode; an error event re-renders the still-active plan and
      // restores Plan mode when the reset did not commit.
      setPlanMode(false);
    }
    sendCmd(commandText);
    pushInputHistory(commandText);
    dom.input.value = '';
    dom.input.style.height = 'auto';
    closeSlashCommandMenu();
    syncComposerAvailability();
    syncToolDrawerBounds();
    return;
  }

  if (state.activePlan && ['planning', 'needs_input', 'ready'].includes(state.activePlan.status)) {
    addSystem(tr('plan.composer.resolveActive'));
    document
      .querySelector<HTMLElement>('.plan-artifact-card:not([data-historical="true"])')
      ?.focus();
    return;
  }

  if (state.imageUploadInFlight || !isComposerModelReady()) return;

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
  if (state.storageMode === 'protected' && !canSendWhileStorageProtected(normalizedCmd)) return;
  setBusy(true);
  state.ws.send(normalizedCmd);
}

export function initInputListeners(options: InputListenerOptions = {}) {
  if (options.onGroupMentionTargetModeActivated) {
    onGroupMentionTargetModeActivated = options.onGroupMentionTargetModeActivated;
  }
  if (_listenerInit) return;
  _listenerInit = true;
  dom.input.addEventListener('keydown', (e) => {
    if (e.isComposing || e.keyCode === 229) return;
    if (handleGroupMentionKeydown(e)) return;
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
      syncComposerAvailability();
    }
  });
  dom.input.addEventListener('input', () => {
    dom.input.style.height = 'auto';
    dom.input.style.height = Math.min(dom.input.scrollHeight, 120) + 'px';
    renderComposerSuggestionMenu();
    syncComposerAvailability();
    syncToolDrawerBounds();
  });
  dom.input.addEventListener('focus', () => {
    renderComposerSuggestionMenu();
  });
  dom.input.addEventListener('compositionend', () => {
    renderComposerSuggestionMenu();
    syncComposerAvailability();
  });
  dom.input.addEventListener('click', () => {
    renderComposerSuggestionMenu();
  });
  dom.input.addEventListener('keyup', (event) => {
    if (['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) {
      renderComposerSuggestionMenu();
    }
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

export function refreshInputMenus(): void {
  if (!dom.input || document.activeElement !== dom.input) {
    closeSlashCommandMenu();
    return;
  }
  renderComposerSuggestionMenu();
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
