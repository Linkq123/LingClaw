import { dom, state } from '../state.js';
import type { SessionGroupSummary, SessionSummary } from '../types.js';
import { normalizePendingDeleteSessionId } from '../utils.js';
import { tr } from '../i18n.js';
import { iconMarkup } from '../icons.js';

export const SESSION_DRAWER_STORAGE_KEY = 'lingclaw.sessionDrawerExpanded';

type SessionDrawerCallbacks = {
  onCreate: () => void;
  onCreateGroup?: () => void;
  onDelete: (sessionId: string) => void;
  onDeleteGroup?: (groupId: string) => void;
  onRename?: (sessionId: string) => void;
  onRenameGroup?: (groupId: string) => void;
  onSwitch: (sessionId: string) => void;
  onSwitchGroup?: (groupId: string) => void;
};

type RenderableSession = SessionSummary & {
  pending?: boolean;
};

type RenderableGroup = SessionGroupSummary & {
  pending?: boolean;
};

let callbacks: SessionDrawerCallbacks | null = null;

function hasValidId(id: string): boolean {
  return id.trim().length > 0;
}

function isCurrentSessionId(id: string): boolean {
  return hasValidId(id) && !state.activeGroupId && id === state.activeSessionId;
}

function isCurrentGroupId(id: string): boolean {
  return hasValidId(id) && hasValidId(state.activeGroupId) && id === state.activeGroupId;
}

function dedupeValidIds<T extends { id: string }>(items: T[]): T[] {
  const seen = new Set<string>();
  return items.filter((item) => {
    if (!hasValidId(item.id)) return true;
    if (seen.has(item.id)) return false;
    seen.add(item.id);
    return true;
  });
}

function isMobileDrawerViewport(): boolean {
  if (typeof window === 'undefined') return false;
  if (typeof window.matchMedia === 'function')
    return window.matchMedia('(max-width: 768px)').matches;
  return window.innerWidth <= 768;
}

export function syncSessionDrawerToggleChrome(): void {
  if (!dom.sessionDrawerToggleBtn) return;
  if (isMobileDrawerViewport()) {
    dom.sessionDrawerToggleBtn.innerHTML = iconMarkup('close');
    dom.sessionDrawerToggleBtn.setAttribute('aria-label', tr('workspace.closeNavigation'));
    dom.sessionDrawerToggleBtn.setAttribute('aria-expanded', String(state.mobileNavigationOpen));
    return;
  }
  dom.sessionDrawerToggleBtn.innerHTML = iconMarkup(
    state.sessionDrawerExpanded ? 'chevron-left' : 'chevron-right',
  );
  dom.sessionDrawerToggleBtn.setAttribute(
    'aria-label',
    state.sessionDrawerExpanded ? tr('session.collapseDrawer') : tr('session.expandDrawer'),
  );
  dom.sessionDrawerToggleBtn.setAttribute('aria-expanded', String(state.sessionDrawerExpanded));
}

function loadDrawerPreference(): boolean {
  try {
    const value = globalThis.localStorage?.getItem(SESSION_DRAWER_STORAGE_KEY);
    if (value == null) {
      return true;
    }
    return value !== 'false';
  } catch {
    return true;
  }
}

function persistDrawerPreference(): void {
  try {
    globalThis.localStorage?.setItem(
      SESSION_DRAWER_STORAGE_KEY,
      String(state.sessionDrawerExpanded),
    );
  } catch {
    // ignore local persistence failures
  }
}

function renderableSessions(): RenderableSession[] {
  const items: RenderableSession[] = dedupeValidIds(
    state.sessions.map((session) => ({
      ...session,
      pending: state.sessionSwitchInFlight && isCurrentSessionId(session.id),
    })),
  );
  if (
    hasValidId(state.activeSessionId) &&
    !items.some((session) => session.id === state.activeSessionId)
  ) {
    items.unshift({
      id: state.activeSessionId,
      name: state.activeSessionId,
      pending: state.sessionSwitchInFlight && !state.activeGroupId,
    });
  }
  items.sort((a, b) => {
    if (a.id === 'main' && b.id !== 'main') return -1;
    if (a.id !== 'main' && b.id === 'main') return 1;
    return (b.updated_at ?? 0) - (a.updated_at ?? 0) || a.id.localeCompare(b.id);
  });
  return items;
}

function renderableGroups(): RenderableGroup[] {
  const items: RenderableGroup[] = dedupeValidIds(
    state.sessionGroups.map((group) => ({
      ...group,
      pending: state.sessionSwitchInFlight && isCurrentGroupId(group.id),
    })),
  );
  if (hasValidId(state.activeGroupId) && !items.some((group) => group.id === state.activeGroupId)) {
    items.unshift({
      id: state.activeGroupId,
      name: state.activeGroupId,
      pending: state.sessionSwitchInFlight,
    });
  }
  items.sort((a, b) => (b.updated_at ?? 0) - (a.updated_at ?? 0) || a.id.localeCompare(b.id));
  return items;
}

function currentBadgeLabel(session: RenderableSession): string {
  if (session.pending) {
    return tr('common.switching');
  }
  if (session.corrupt) {
    return tr('common.corrupt');
  }
  if (!hasValidId(session.id)) {
    return tr('common.unavailable');
  }
  if (isCurrentSessionId(session.id)) {
    return tr('common.current');
  }
  return '';
}

function currentGroupBadgeLabel(group: RenderableGroup): string {
  if (group.pending) return tr('common.switching');
  if (group.corrupt) return tr('common.corrupt');
  if (!hasValidId(group.id)) return tr('common.unavailable');
  if (isCurrentGroupId(group.id)) return tr('common.current');
  if ((group.running ?? 0) > 0) return tr('session.groupRunning', { count: group.running ?? 0 });
  return '';
}

function isSessionDeleteable(session: RenderableSession): boolean {
  return (
    !state.activeGroupId &&
    !state.sessionSwitchInFlight &&
    !session.pending &&
    hasValidId(session.id) &&
    session.id !== 'main' &&
    session.id !== state.activeSessionId
  );
}

function isSessionRenameable(session: RenderableSession): boolean {
  return (
    !state.sessionSwitchInFlight &&
    !session.pending &&
    hasValidId(session.id) &&
    session.corrupt !== true &&
    callbacks?.onRename != null
  );
}

function createSessionRow(session: RenderableSession): HTMLElement {
  const row = document.createElement('div');
  row.className = 'session-drawer-row';
  row.dataset.sessionId = session.id;
  const hasValidSessionId = hasValidId(session.id);
  const isCurrentSession = isCurrentSessionId(session.id);
  if (isCurrentSession) {
    row.classList.add('is-active');
  }
  if (session.corrupt) {
    row.classList.add('is-corrupt');
  }
  if (session.pending) {
    row.classList.add('is-pending');
  }
  if (state.sessionSwitchInFlight || !hasValidSessionId) {
    row.classList.add('is-disabled');
  }

  const mainButton = document.createElement('button');
  mainButton.type = 'button';
  mainButton.className = 'session-drawer-row-main';
  mainButton.dataset.sessionAction = 'switch';
  mainButton.disabled =
    state.sessionSwitchInFlight ||
    session.pending === true ||
    session.corrupt === true ||
    !hasValidSessionId;
  mainButton.setAttribute(
    'aria-label',
    session.corrupt || !hasValidSessionId
      ? tr('session.unavailable', { name: session.name || session.id })
      : isCurrentSession
        ? tr('session.current', { name: session.name || session.id })
        : tr('session.switchTo', { name: session.name || session.id }),
  );
  if (isCurrentSession) {
    mainButton.setAttribute('aria-current', 'true');
  }
  mainButton.addEventListener('click', () => {
    if (!mainButton.disabled) {
      callbacks?.onSwitch(session.id);
    }
  });

  const content = document.createElement('div');
  content.className = 'session-drawer-row-content';

  const titleRow = document.createElement('div');
  titleRow.className = 'session-drawer-row-titlebar';

  const title = document.createElement('div');
  title.className = 'session-drawer-row-title';
  title.textContent = session.name || session.id;

  const badgeLabel = currentBadgeLabel(session);
  if (badgeLabel) {
    const badge = document.createElement('span');
    badge.className = 'session-drawer-row-badge';
    badge.textContent = badgeLabel;
    titleRow.append(title, badge);
  } else {
    titleRow.append(title);
  }

  const meta = document.createElement('div');
  meta.className = 'session-drawer-row-meta';
  meta.textContent = session.id;

  content.append(titleRow, meta);
  mainButton.appendChild(content);
  row.appendChild(mainButton);

  const actions: HTMLElement[] = [];
  if (isSessionRenameable(session)) {
    const renameButton = document.createElement('button');
    renameButton.type = 'button';
    renameButton.className = 'session-drawer-row-action session-drawer-row-rename';
    renameButton.dataset.sessionAction = 'rename';
    renameButton.setAttribute(
      'aria-label',
      tr('session.rename', { name: session.name || session.id }),
    );
    renameButton.title = tr('session.rename', { name: session.name || session.id });
    renameButton.innerHTML = iconMarkup('edit');
    renameButton.addEventListener('click', () => {
      callbacks?.onRename?.(session.id);
    });
    actions.push(renameButton);
  }
  if (isSessionDeleteable(session)) {
    const deleteButton = document.createElement('button');
    deleteButton.type = 'button';
    deleteButton.className = 'session-drawer-row-action session-drawer-row-delete';
    deleteButton.dataset.sessionAction = 'delete';
    deleteButton.setAttribute(
      'aria-label',
      tr('session.delete', { name: session.name || session.id }),
    );
    deleteButton.title = tr('session.delete', { name: session.name || session.id });
    deleteButton.innerHTML = iconMarkup('trash');
    deleteButton.addEventListener('click', () => {
      callbacks?.onDelete(session.id);
    });
    actions.push(deleteButton);
  }
  if (actions.length > 0) {
    const actionGroup = document.createElement('div');
    actionGroup.className = 'session-drawer-row-actions';
    actionGroup.append(...actions);
    row.appendChild(actionGroup);
  }

  return row;
}

function createSectionHeader(label: string, count: number, action?: () => void): HTMLElement {
  const header = document.createElement('div');
  header.className = 'session-drawer-section';
  const text = document.createElement('span');
  text.textContent = `${label}${count > 0 ? ` ${count}` : ''}`;
  header.appendChild(text);
  if (action) {
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'session-drawer-section-action';
    button.innerHTML = iconMarkup('plus');
    button.title =
      label === tr('session.sectionGroups')
        ? tr('session.newGroup')
        : tr('session.newSessionShort');
    button.setAttribute('aria-label', button.title);
    button.addEventListener('click', action);
    header.appendChild(button);
  }
  return header;
}

function createGroupRow(group: RenderableGroup): HTMLElement {
  const row = document.createElement('div');
  row.className = 'session-drawer-row session-drawer-row-group';
  row.dataset.groupId = group.id;
  const hasValidGroupId = hasValidId(group.id);
  const isCurrentGroup = isCurrentGroupId(group.id);
  if (isCurrentGroup) row.classList.add('is-active');
  if (group.corrupt) row.classList.add('is-corrupt');
  if (group.pending) row.classList.add('is-pending');
  if (state.sessionSwitchInFlight || !hasValidGroupId) row.classList.add('is-disabled');

  const mainButton = document.createElement('button');
  mainButton.type = 'button';
  mainButton.className = 'session-drawer-row-main';
  mainButton.dataset.sessionAction = 'switch-group';
  mainButton.disabled =
    state.sessionSwitchInFlight ||
    group.pending === true ||
    group.corrupt === true ||
    !hasValidGroupId;
  mainButton.setAttribute(
    'aria-label',
    group.corrupt || !hasValidGroupId
      ? tr('session.unavailableGroup', { name: group.name || group.id })
      : isCurrentGroup
        ? tr('session.currentGroup', { name: group.name || group.id })
        : tr('session.openGroup', { name: group.name || group.id }),
  );
  if (isCurrentGroup) mainButton.setAttribute('aria-current', 'true');
  mainButton.addEventListener('click', () => {
    if (!mainButton.disabled) callbacks?.onSwitchGroup?.(group.id);
  });

  const content = document.createElement('div');
  content.className = 'session-drawer-row-content';
  const titleRow = document.createElement('div');
  titleRow.className = 'session-drawer-row-titlebar';
  const title = document.createElement('div');
  title.className = 'session-drawer-row-title';
  title.textContent = group.name || group.id;
  const badgeLabel = currentGroupBadgeLabel(group);
  if (badgeLabel) {
    const badge = document.createElement('span');
    badge.className = 'session-drawer-row-badge';
    badge.textContent = badgeLabel;
    titleRow.append(title, badge);
  } else {
    titleRow.append(title);
  }
  const meta = document.createElement('div');
  meta.className = 'session-drawer-row-meta';
  meta.textContent = tr('session.groupMembersMeta', {
    id: group.id,
    count: group.members ?? 0,
  });
  content.append(titleRow, meta);
  mainButton.appendChild(content);
  row.appendChild(mainButton);

  const actions: HTMLElement[] = [];
  if (
    !state.sessionSwitchInFlight &&
    !group.pending &&
    !group.corrupt &&
    hasValidGroupId &&
    callbacks?.onRenameGroup
  ) {
    const renameButton = document.createElement('button');
    renameButton.type = 'button';
    renameButton.className = 'session-drawer-row-action session-drawer-row-rename';
    renameButton.dataset.sessionAction = 'rename';
    renameButton.setAttribute(
      'aria-label',
      tr('session.renameGroup', { name: group.name || group.id }),
    );
    renameButton.title = tr('session.renameGroup', { name: group.name || group.id });
    renameButton.innerHTML = iconMarkup('edit');
    renameButton.addEventListener('click', () => callbacks?.onRenameGroup?.(group.id));
    actions.push(renameButton);
  }
  if (
    !state.sessionSwitchInFlight &&
    !group.pending &&
    hasValidGroupId &&
    callbacks?.onDeleteGroup
  ) {
    const deleteButton = document.createElement('button');
    deleteButton.type = 'button';
    deleteButton.className = 'session-drawer-row-action session-drawer-row-delete';
    deleteButton.dataset.sessionAction = 'delete';
    deleteButton.setAttribute(
      'aria-label',
      tr('session.deleteGroup', { name: group.name || group.id }),
    );
    deleteButton.title = tr('session.deleteGroup', { name: group.name || group.id });
    deleteButton.innerHTML = iconMarkup('trash');
    deleteButton.addEventListener('click', () => callbacks?.onDeleteGroup?.(group.id));
    actions.push(deleteButton);
  }
  if (actions.length > 0) {
    const actionGroup = document.createElement('div');
    actionGroup.className = 'session-drawer-row-actions';
    actionGroup.append(...actions);
    row.appendChild(actionGroup);
  }
  return row;
}

function applyDrawerChrome(): void {
  if (!dom.sessionDrawer) {
    return;
  }

  dom.sessionDrawer.classList.toggle('is-collapsed', !state.sessionDrawerExpanded);
  dom.sessionDrawer.dataset.expanded = String(state.sessionDrawerExpanded);

  syncSessionDrawerToggleChrome();

  if (dom.sessionDrawerNewBtn) {
    dom.sessionDrawerNewBtn.innerHTML = `${iconMarkup('plus')}<span class="sidebar-new-label">${tr(
      'session.newSession',
    )}</span>`;
    dom.sessionDrawerNewBtn.disabled = state.sessionSwitchInFlight;
    dom.sessionDrawerNewBtn.setAttribute('aria-label', tr('session.newSessionShort'));
    dom.sessionDrawerNewBtn.title = tr('session.newSessionShort');
  }
}

export function initSessionDrawer(nextCallbacks: SessionDrawerCallbacks): void {
  callbacks = nextCallbacks;
  state.sessionDrawerExpanded = loadDrawerPreference();
  renderSessionDrawer();
}

export function toggleSessionDrawerExpanded(): void {
  state.sessionDrawerExpanded = !state.sessionDrawerExpanded;
  persistDrawerPreference();
  renderSessionDrawer();
}

export function renderSessionDrawer(): void {
  applyDrawerChrome();

  if (!dom.sessionDrawerList) {
    return;
  }

  state.pendingDeleteSessionId = normalizePendingDeleteSessionId(
    state.sessions,
    state.activeSessionId,
    state.pendingDeleteSessionId,
  );

  dom.sessionDrawerList.hidden = false;
  const sessions = renderableSessions();
  const groups = renderableGroups();
  if (sessions.length === 0 && groups.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'session-drawer-empty';
    empty.textContent = tr('session.noSessions');
    dom.sessionDrawerList.replaceChildren(empty);
    return;
  }

  const nodes: HTMLElement[] = [];
  nodes.push(createSectionHeader(tr('session.sectionSessions'), sessions.length));
  nodes.push(...sessions.map(createSessionRow));
  nodes.push(
    createSectionHeader(tr('session.sectionGroups'), groups.length, callbacks?.onCreateGroup),
  );
  if (groups.length > 0) {
    nodes.push(...groups.map(createGroupRow));
  } else {
    const empty = document.createElement('div');
    empty.className = 'session-drawer-empty session-drawer-empty-compact';
    empty.textContent = tr('session.noGroups');
    nodes.push(empty);
  }
  dom.sessionDrawerList.replaceChildren(...nodes);
}
