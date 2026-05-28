import { dom, state } from '../state.js';
import type { SessionSummary } from '../types.js';
import { normalizePendingDeleteSessionId, shouldSwitchToSelectedSession } from '../utils.js';

export const SESSION_DRAWER_STORAGE_KEY = 'lingclaw.sessionDrawerExpanded';

type SessionDrawerCallbacks = {
  onCreate: () => void;
  onDelete: (sessionId: string) => void;
  onSwitch: (sessionId: string) => void;
};

type RenderableSession = SessionSummary & {
  pending?: boolean;
};

let callbacks: SessionDrawerCallbacks | null = null;

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
  const items: RenderableSession[] = state.sessions.map((session) => ({
    ...session,
    pending: state.sessionSwitchInFlight && session.id === state.activeSessionId,
  }));
  if (state.activeSessionId && !items.some((session) => session.id === state.activeSessionId)) {
    items.unshift({
      id: state.activeSessionId,
      name: state.activeSessionId,
      pending: state.sessionSwitchInFlight,
    });
  }
  return items;
}

function currentBadgeLabel(session: RenderableSession): string {
  if (session.pending) {
    return 'Switching';
  }
  if (session.corrupt) {
    return 'Corrupt';
  }
  if (session.id === state.activeSessionId) {
    return 'Current';
  }
  return '';
}

function isSessionDeleteable(session: RenderableSession): boolean {
  return (
    !state.sessionSwitchInFlight &&
    !session.pending &&
    session.id !== 'main' &&
    session.id !== state.activeSessionId
  );
}

function createSessionRow(session: RenderableSession): HTMLElement {
  const row = document.createElement('div');
  row.className = 'session-drawer-row';
  row.dataset.sessionId = session.id;
  if (session.id === state.activeSessionId) {
    row.classList.add('is-active');
  }
  if (session.corrupt) {
    row.classList.add('is-corrupt');
  }
  if (session.pending) {
    row.classList.add('is-pending');
  }
  if (state.sessionSwitchInFlight) {
    row.classList.add('is-disabled');
  }

  const mainButton = document.createElement('button');
  mainButton.type = 'button';
  mainButton.className = 'session-drawer-row-main';
  mainButton.dataset.sessionAction = 'switch';
  mainButton.disabled =
    state.sessionSwitchInFlight ||
    session.pending === true ||
    !shouldSwitchToSelectedSession(state.sessions, state.activeSessionId, session.id);
  mainButton.setAttribute(
    'aria-label',
    session.corrupt ? `Unavailable session ${session.name || session.id}` : `Switch to ${session.name || session.id}`,
  );
  if (session.id === state.activeSessionId) {
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

  if (isSessionDeleteable(session)) {
    const deleteButton = document.createElement('button');
    deleteButton.type = 'button';
    deleteButton.className = 'session-drawer-row-delete';
    deleteButton.dataset.sessionAction = 'delete';
    deleteButton.setAttribute('aria-label', `Delete ${session.name || session.id}`);
    deleteButton.title = `Delete ${session.name || session.id}`;
    deleteButton.textContent = '×';
    deleteButton.addEventListener('click', () => {
      callbacks?.onDelete(session.id);
    });
    row.appendChild(deleteButton);
  }

  return row;
}

function applyDrawerChrome(): void {
  if (!dom.sessionDrawer) {
    return;
  }

  dom.sessionDrawer.classList.toggle('is-collapsed', !state.sessionDrawerExpanded);
  dom.sessionDrawer.dataset.expanded = String(state.sessionDrawerExpanded);

  if (dom.sessionDrawerToggleBtn) {
    dom.sessionDrawerToggleBtn.textContent = state.sessionDrawerExpanded ? '<' : '>';
    dom.sessionDrawerToggleBtn.setAttribute(
      'aria-label',
      state.sessionDrawerExpanded ? 'Collapse sessions drawer' : 'Expand sessions drawer',
    );
    dom.sessionDrawerToggleBtn.setAttribute('aria-expanded', String(state.sessionDrawerExpanded));
  }

  if (dom.sessionDrawerNewBtn) {
    dom.sessionDrawerNewBtn.textContent = state.sessionDrawerExpanded ? 'New Session' : '+';
    dom.sessionDrawerNewBtn.disabled = state.sessionSwitchInFlight;
    dom.sessionDrawerNewBtn.setAttribute('aria-label', 'New session');
    dom.sessionDrawerNewBtn.title = 'New session';
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

  if (!state.sessionDrawerExpanded) {
    dom.sessionDrawerList.hidden = true;
    dom.sessionDrawerList.replaceChildren();
    return;
  }

  dom.sessionDrawerList.hidden = false;
  const sessions = renderableSessions();
  if (sessions.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'session-drawer-empty';
    empty.textContent = 'No sessions yet';
    dom.sessionDrawerList.replaceChildren(empty);
    return;
  }

  dom.sessionDrawerList.replaceChildren(...sessions.map(createSessionRow));
}
