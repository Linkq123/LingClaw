import { dom, state } from '../state.js';
import type { SessionGroupSummary, SessionSummary } from '../types.js';
import { normalizePendingDeleteSessionId } from '../utils.js';
import { tr } from '../i18n.js';
import { iconMarkup } from '../icons.js';
import type { IconName } from '../icons.js';

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
const RECENT_SESSION_LIMIT = 12;

let sessionSearchQuery = '';
let earlierSessionsExpanded = false;
let openRowMenuElement: HTMLElement | null = null;
let openRowMenuTrigger: HTMLButtonElement | null = null;
let openRowMenuId = '';
let drawerListenerController: AbortController | null = null;

type SessionRowAction = {
  action: 'rename' | 'delete';
  danger?: boolean;
  icon: IconName;
  label: string;
  onSelect: () => void;
};

function normalizedSearchValue(value: string): string {
  return value.trim().toLocaleLowerCase();
}

function matchesSearch(item: { id: string; name: string }, query: string): boolean {
  if (!query) return true;
  return `${item.name || ''}\n${item.id || ''}`.toLocaleLowerCase().includes(query);
}

export function closeSessionRowMenu({ restoreFocus = false } = {}): void {
  const trigger = openRowMenuTrigger;
  trigger?.setAttribute('aria-expanded', 'false');
  trigger?.closest('.session-drawer-row')?.classList.remove('has-open-menu');
  openRowMenuElement?.remove();
  openRowMenuElement = null;
  openRowMenuTrigger = null;
  openRowMenuId = '';
  if (restoreFocus && trigger?.isConnected) trigger.focus();
}

function focusMenuItem(menu: HTMLElement, position: 'first' | 'last'): void {
  const items = Array.from(menu.querySelectorAll<HTMLButtonElement>('[role="menuitem"]'));
  const target = position === 'last' ? items.at(-1) : items[0];
  target?.focus();
}

function positionSessionRowMenu(menu: HTMLElement, trigger: HTMLButtonElement): void {
  const drawer = dom.sessionDrawer;
  if (!drawer) return;
  const drawerRect = drawer.getBoundingClientRect();
  const triggerRect = trigger.getBoundingClientRect();
  const width = menu.offsetWidth || 176;
  const height = menu.offsetHeight || 92;
  const left = Math.max(
    8,
    Math.min(triggerRect.right - drawerRect.left - width, drawerRect.width - width - 8),
  );
  const spaceBelow = drawerRect.bottom - triggerRect.bottom;
  const idealTop =
    spaceBelow >= height + 8
      ? triggerRect.bottom - drawerRect.top + 4
      : triggerRect.top - drawerRect.top - height - 4;
  const top = Math.max(8, Math.min(idealTop, drawerRect.height - height - 8));
  menu.style.left = `${left}px`;
  menu.style.top = `${top}px`;
}

function handleSessionRowMenuKeydown(event: KeyboardEvent, menu: HTMLElement): void {
  const items = Array.from(menu.querySelectorAll<HTMLButtonElement>('[role="menuitem"]'));
  if (items.length === 0) return;
  const currentIndex = items.indexOf(document.activeElement as HTMLButtonElement);
  let nextIndex = currentIndex;
  if (event.key === 'ArrowDown') nextIndex = (currentIndex + 1 + items.length) % items.length;
  else if (event.key === 'ArrowUp') nextIndex = (currentIndex - 1 + items.length) % items.length;
  else if (event.key === 'Home') nextIndex = 0;
  else if (event.key === 'End') nextIndex = items.length - 1;
  else if (event.key === 'Tab') {
    closeSessionRowMenu();
    return;
  } else {
    return;
  }
  event.preventDefault();
  items[nextIndex]?.focus();
}

function openSessionRowMenu(
  trigger: HTMLButtonElement,
  menuId: string,
  actions: SessionRowAction[],
  focusPosition: 'first' | 'last' = 'first',
): void {
  if (openRowMenuId === menuId && openRowMenuTrigger === trigger) {
    closeSessionRowMenu({ restoreFocus: true });
    return;
  }
  closeSessionRowMenu();
  const drawer = dom.sessionDrawer;
  if (!drawer) return;

  const menu = document.createElement('div');
  menu.id = menuId;
  menu.className = 'session-drawer-row-menu';
  menu.setAttribute('role', 'menu');
  menu.addEventListener('keydown', (event) => handleSessionRowMenuKeydown(event, menu));

  for (const item of actions) {
    const button = document.createElement('button');
    button.type = 'button';
    button.className = `session-drawer-row-menu-item${item.danger ? ' is-danger' : ''}`;
    button.dataset.sessionAction = item.action;
    button.setAttribute('role', 'menuitem');
    button.innerHTML = `${iconMarkup(item.icon)}<span></span>`;
    const label = button.querySelector('span');
    if (label) label.textContent = item.label;
    button.addEventListener('click', () => {
      closeSessionRowMenu();
      item.onSelect();
    });
    menu.appendChild(button);
  }

  drawer.appendChild(menu);
  openRowMenuElement = menu;
  openRowMenuTrigger = trigger;
  openRowMenuId = menuId;
  trigger.setAttribute('aria-expanded', 'true');
  trigger.closest('.session-drawer-row')?.classList.add('has-open-menu');
  positionSessionRowMenu(menu, trigger);
  queueMicrotask(() => focusMenuItem(menu, focusPosition));
}

function appendSessionRowActions(
  row: HTMLElement,
  kind: 'session' | 'group',
  itemId: string,
  itemName: string,
  actions: SessionRowAction[],
): void {
  if (actions.length === 0) return;
  const actionGroup = document.createElement('div');
  actionGroup.className = 'session-drawer-row-actions';
  const trigger = document.createElement('button');
  trigger.type = 'button';
  trigger.className = 'session-drawer-row-action session-drawer-row-menu-trigger';
  trigger.dataset.sessionAction = 'menu';
  const menuId = `${kind}-row-menu-${encodeURIComponent(itemId)}`;
  trigger.setAttribute('aria-haspopup', 'menu');
  trigger.setAttribute('aria-expanded', 'false');
  trigger.setAttribute('aria-controls', menuId);
  trigger.setAttribute('aria-label', tr('session.moreActions', { name: itemName }));
  trigger.title = tr('session.moreActions', { name: itemName });
  trigger.innerHTML = iconMarkup('more');
  trigger.addEventListener('click', (event) => {
    event.stopPropagation();
    openSessionRowMenu(trigger, menuId, actions);
  });
  trigger.addEventListener('keydown', (event) => {
    if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp') return;
    event.preventDefault();
    openSessionRowMenu(trigger, menuId, actions, event.key === 'ArrowUp' ? 'last' : 'first');
  });
  actionGroup.appendChild(trigger);
  row.appendChild(actionGroup);
}

function identityNavigationBlocked(): boolean {
  return (
    state.sessionSwitchInFlight ||
    state.sessionIdentityMutationInFlight ||
    state.composerSessionTransitionPending ||
    state.composerSessionIdentityPending ||
    state.imageUploadInFlight
  );
}

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

function splitRecentSessions(sessions: RenderableSession[]): {
  recent: RenderableSession[];
  earlier: RenderableSession[];
} {
  const recent: RenderableSession[] = [];
  const included = new Set<RenderableSession>();
  const addById = (id: string) => {
    const item = sessions.find((session) => session.id === id);
    if (!item || included.has(item)) return;
    recent.push(item);
    included.add(item);
  };

  addById('main');
  if (!state.activeGroupId) addById(state.activeSessionId);
  for (const session of sessions) {
    if (recent.length >= RECENT_SESSION_LIMIT) break;
    if (included.has(session)) continue;
    recent.push(session);
    included.add(session);
  }

  return {
    recent,
    earlier: sessions.filter((session) => !included.has(session)),
  };
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
    !identityNavigationBlocked() &&
    !session.pending &&
    hasValidId(session.id) &&
    session.id !== 'main' &&
    session.id !== state.activeSessionId
  );
}

function isSessionRenameable(session: RenderableSession): boolean {
  return (
    !identityNavigationBlocked() &&
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
  if (identityNavigationBlocked() || !hasValidSessionId) {
    row.classList.add('is-disabled');
  }

  const mainButton = document.createElement('button');
  mainButton.type = 'button';
  mainButton.className = 'session-drawer-row-main';
  mainButton.dataset.sessionAction = 'switch';
  mainButton.disabled =
    identityNavigationBlocked() ||
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
      closeSessionRowMenu();
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

  const actions: SessionRowAction[] = [];
  if (isSessionRenameable(session)) {
    actions.push({
      action: 'rename',
      icon: 'edit',
      label: tr('session.rename', { name: session.name || session.id }),
      onSelect: () => callbacks?.onRename?.(session.id),
    });
  }
  if (isSessionDeleteable(session)) {
    actions.push({
      action: 'delete',
      danger: true,
      icon: 'trash',
      label: tr('session.delete', { name: session.name || session.id }),
      onSelect: () => callbacks?.onDelete(session.id),
    });
  }
  appendSessionRowActions(row, 'session', session.id, session.name || session.id, actions);

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
    button.disabled = identityNavigationBlocked();
    button.addEventListener('click', () => {
      if (!button.disabled) action();
    });
    header.appendChild(button);
  }
  return header;
}

function createEarlierSessionsHeader(count: number): HTMLElement {
  const header = createSectionHeader(tr('session.sectionEarlier'), count);
  header.classList.add('session-drawer-section-collapsible');
  const button = document.createElement('button');
  button.type = 'button';
  button.className = 'session-drawer-section-toggle';
  button.dataset.sessionEarlierToggle = 'true';
  button.setAttribute('aria-expanded', String(earlierSessionsExpanded));
  button.setAttribute(
    'aria-label',
    earlierSessionsExpanded ? tr('session.hideEarlier') : tr('session.showEarlier', { count }),
  );
  button.title = button.getAttribute('aria-label') || '';
  button.innerHTML = iconMarkup(earlierSessionsExpanded ? 'chevron-down' : 'chevron-right');
  button.addEventListener('click', () => {
    earlierSessionsExpanded = !earlierSessionsExpanded;
    renderSessionDrawer();
    queueMicrotask(() =>
      dom.sessionDrawerList
        ?.querySelector<HTMLButtonElement>('[data-session-earlier-toggle="true"]')
        ?.focus(),
    );
  });
  header.appendChild(button);
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
  if (identityNavigationBlocked() || !hasValidGroupId) row.classList.add('is-disabled');

  const mainButton = document.createElement('button');
  mainButton.type = 'button';
  mainButton.className = 'session-drawer-row-main';
  mainButton.dataset.sessionAction = 'switch-group';
  mainButton.disabled =
    identityNavigationBlocked() ||
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
    if (!mainButton.disabled) {
      closeSessionRowMenu();
      callbacks?.onSwitchGroup?.(group.id);
    }
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

  const actions: SessionRowAction[] = [];
  if (
    !identityNavigationBlocked() &&
    !group.pending &&
    !group.corrupt &&
    hasValidGroupId &&
    callbacks?.onRenameGroup
  ) {
    actions.push({
      action: 'rename',
      icon: 'edit',
      label: tr('session.renameGroup', { name: group.name || group.id }),
      onSelect: () => callbacks?.onRenameGroup?.(group.id),
    });
  }
  if (
    !identityNavigationBlocked() &&
    !group.pending &&
    hasValidGroupId &&
    callbacks?.onDeleteGroup
  ) {
    actions.push({
      action: 'delete',
      danger: true,
      icon: 'trash',
      label: tr('session.deleteGroup', { name: group.name || group.id }),
      onSelect: () => callbacks?.onDeleteGroup?.(group.id),
    });
  }
  appendSessionRowActions(row, 'group', group.id, group.name || group.id, actions);
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
    dom.sessionDrawerNewBtn.disabled = identityNavigationBlocked();
    dom.sessionDrawerNewBtn.setAttribute('aria-label', tr('session.newSessionShort'));
    dom.sessionDrawerNewBtn.title = tr('session.newSessionShort');
  }
}

export function initSessionDrawer(nextCallbacks: SessionDrawerCallbacks): void {
  callbacks = nextCallbacks;
  closeSessionRowMenu();
  sessionSearchQuery = '';
  earlierSessionsExpanded = false;
  state.sessionDrawerExpanded = loadDrawerPreference();
  drawerListenerController?.abort();
  drawerListenerController = new AbortController();
  const { signal } = drawerListenerController;
  if (dom.sessionDrawerSearchInput) {
    dom.sessionDrawerSearchInput.value = '';
    dom.sessionDrawerSearchInput.addEventListener(
      'input',
      () => {
        sessionSearchQuery = normalizedSearchValue(dom.sessionDrawerSearchInput?.value || '');
        renderSessionDrawer();
      },
      { signal },
    );
  }
  document.addEventListener(
    'pointerdown',
    (event) => {
      const target = event.target;
      if (!(target instanceof Node) || !openRowMenuElement) return;
      if (openRowMenuElement.contains(target) || openRowMenuTrigger?.contains(target)) return;
      closeSessionRowMenu();
    },
    { capture: true, signal },
  );
  document.addEventListener(
    'keydown',
    (event) => {
      if (event.key !== 'Escape' || !openRowMenuElement) return;
      event.preventDefault();
      event.stopPropagation();
      closeSessionRowMenu({ restoreFocus: true });
    },
    { capture: true, signal },
  );
  window.addEventListener('resize', () => closeSessionRowMenu(), { signal });
  dom.sessionDrawerList?.addEventListener('scroll', () => closeSessionRowMenu(), {
    passive: true,
    signal,
  });
  renderSessionDrawer();
}

export function disposeSessionDrawer(): void {
  drawerListenerController?.abort();
  drawerListenerController = null;
  closeSessionRowMenu();
  callbacks = null;
}

export function toggleSessionDrawerExpanded(): void {
  state.sessionDrawerExpanded = !state.sessionDrawerExpanded;
  persistDrawerPreference();
  renderSessionDrawer();
}

export function renderSessionDrawer(): void {
  applyDrawerChrome();
  closeSessionRowMenu();

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
  if (sessionSearchQuery) {
    const matchingSessions = sessions.filter((session) =>
      matchesSearch(session, sessionSearchQuery),
    );
    const matchingGroups = groups.filter((group) => matchesSearch(group, sessionSearchQuery));
    if (matchingSessions.length === 0 && matchingGroups.length === 0) {
      const empty = document.createElement('div');
      empty.className = 'session-drawer-empty session-drawer-search-empty';
      empty.textContent = tr('session.noMatches');
      dom.sessionDrawerList.replaceChildren(empty);
      return;
    }
    if (matchingSessions.length > 0) {
      nodes.push(createSectionHeader(tr('session.sectionSessions'), matchingSessions.length));
      nodes.push(...matchingSessions.map(createSessionRow));
    }
    if (matchingGroups.length > 0) {
      nodes.push(createSectionHeader(tr('session.sectionGroups'), matchingGroups.length));
      nodes.push(...matchingGroups.map(createGroupRow));
    }
    dom.sessionDrawerList.replaceChildren(...nodes);
    return;
  }

  if (sessions.length > 0) {
    const { recent, earlier } = splitRecentSessions(sessions);
    nodes.push(createSectionHeader(tr('session.sectionRecent'), recent.length));
    nodes.push(...recent.map(createSessionRow));
    if (earlier.length > 0) {
      nodes.push(createEarlierSessionsHeader(earlier.length));
      if (earlierSessionsExpanded) nodes.push(...earlier.map(createSessionRow));
    }
  } else {
    nodes.push(createSectionHeader(tr('session.sectionSessions'), 0));
    const empty = document.createElement('div');
    empty.className = 'session-drawer-empty session-drawer-empty-compact';
    empty.textContent = tr('session.noSessions');
    nodes.push(empty);
  }
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
