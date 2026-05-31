import { beforeEach, describe, expect, it, vi } from 'vitest';

type AppStateModule = typeof import('../src/state.js');
type SessionsRendererModule = typeof import('../src/renderers/sessions.js');
type UtilsModule = typeof import('../src/utils.js');

const mockWebSocket = vi.fn();

vi.mock('../src/constants.js', () => ({
  MAX_RECONNECT_ATTEMPTS: 3,
}));

vi.mock('../src/renderers/chat.js', () => ({
  addSystem: vi.fn(),
  setBusy: vi.fn(),
}));

vi.mock('../src/renderers/auto-trace.js', () => ({
  clearActiveAutoTrace: vi.fn(),
  clearCompressionOutcome: vi.fn(),
}));

vi.mock('../src/renderers/react-status.js', () => ({
  clearReactStatus: vi.fn(),
}));

vi.mock('../src/renderers/tools.js', () => ({
  closeToolDrawer: vi.fn(),
}));

vi.mock('../src/handlers/stream.js', () => ({
  finishAssistantStream: vi.fn(),
  finishReasoningStream: vi.fn(),
}));

describe('socket session binding', () => {
  let stateModule: AppStateModule;
  let sessionsRendererModule: SessionsRendererModule;
  let utilsModule: UtilsModule;

  function mountSessionDrawerDom() {
    document.body.innerHTML = `
      <span id="conn-dot"></span>
      <span id="conn-label"></span>
      <aside id="session-drawer">
        <div class="session-drawer-header">
          <button id="session-drawer-toggle-btn"></button>
          <h2 class="session-drawer-heading">Sessions</h2>
          <button id="session-drawer-new-btn"></button>
        </div>
        <div id="session-drawer-list"></div>
      </aside>
    `;
    stateModule.initDomRefs();
  }

  beforeEach(async () => {
    vi.resetModules();
    stateModule = await import('../src/state.js');
    sessionsRendererModule = await import('../src/renderers/sessions.js');
    utilsModule = await import('../src/utils.js');
    localStorage.clear();
    mountSessionDrawerDom();
    stateModule.state.activeSessionId = '';
    stateModule.state.pendingDeleteSessionId = '';
    stateModule.state.reconnectDelay = 1000;
    stateModule.state.reconnectAttempts = 0;
    stateModule.state.sessionSwitchInFlight = false;
    stateModule.state.sessionDrawerExpanded = true;
    stateModule.state.sessions = [];

    (globalThis as unknown as { WebSocket: unknown }).WebSocket =
      mockWebSocket as unknown as typeof WebSocket;
    mockWebSocket.mockReset();
  });

  it('connects to default websocket path when no active session is selected', async () => {
    const { connect } = await import('../src/socket.js');

    connect(() => {});

    expect(mockWebSocket).toHaveBeenCalledWith('ws://localhost:3000/ws');
  });

  it('connects to the selected websocket session when active session is restored', async () => {
    const { connect } = await import('../src/socket.js');
    stateModule.state.activeSessionId = 'research-notes';

    connect(() => {});

    expect(mockWebSocket).toHaveBeenCalledWith('ws://localhost:3000/ws?session=research-notes');
  });

  it('keeps only a non-current non-main pending delete target', async () => {
    stateModule.state.sessions = [
      { id: 'main', name: 'Main' },
      { id: 'research-notes', name: 'Research Notes' },
      { id: 'project-alpha', name: 'Project Alpha' },
    ];
    stateModule.state.activeSessionId = 'project-alpha';

    expect(
      utilsModule.normalizePendingDeleteSessionId(
        stateModule.state.sessions,
        stateModule.state.activeSessionId,
        'research-notes',
      ),
    ).toBe('research-notes');

    expect(
      utilsModule.normalizePendingDeleteSessionId(
        stateModule.state.sessions,
        stateModule.state.activeSessionId,
        'project-alpha',
      ),
    ).toBe('');

    expect(
      utilsModule.normalizePendingDeleteSessionId(
        stateModule.state.sessions,
        stateModule.state.activeSessionId,
        'main',
      ),
    ).toBe('');
  });

  it('defaults the session drawer to expanded and persists collapsed state locally', async () => {
    sessionsRendererModule.initSessionDrawer({
      onCreate: vi.fn(),
      onDelete: vi.fn(),
      onSwitch: vi.fn(),
    });

    expect(stateModule.state.sessionDrawerExpanded).toBe(true);
    expect(stateModule.dom.sessionDrawer?.classList.contains('is-collapsed')).toBe(false);

    sessionsRendererModule.toggleSessionDrawerExpanded();

    expect(stateModule.state.sessionDrawerExpanded).toBe(false);
    expect(stateModule.dom.sessionDrawer?.classList.contains('is-collapsed')).toBe(true);
    expect(localStorage.getItem(sessionsRendererModule.SESSION_DRAWER_STORAGE_KEY)).toBe('false');
  });

  it('restores the session drawer state from localStorage', async () => {
    localStorage.setItem(sessionsRendererModule.SESSION_DRAWER_STORAGE_KEY, 'false');

    sessionsRendererModule.initSessionDrawer({
      onCreate: vi.fn(),
      onDelete: vi.fn(),
      onSwitch: vi.fn(),
    });

    expect(stateModule.state.sessionDrawerExpanded).toBe(false);
    expect(stateModule.dom.sessionDrawer?.classList.contains('is-collapsed')).toBe(true);
  });

  it('renders healthy session rows, switches them, and hides delete for current/main rows', async () => {
    const onSwitch = vi.fn();
    const onDelete = vi.fn();
    stateModule.state.sessions = [
      { id: 'main', name: 'Main' },
      { id: 'research-notes', name: 'Research Notes' },
      { id: 'project-alpha', name: 'Project Alpha' },
    ];
    stateModule.state.activeSessionId = 'main';

    sessionsRendererModule.initSessionDrawer({
      onCreate: vi.fn(),
      onDelete,
      onSwitch,
    });

    const switchButton = stateModule.dom.sessionDrawerList?.querySelector<HTMLButtonElement>(
      '[data-session-id="research-notes"] [data-session-action="switch"]',
    );
    switchButton?.click();

    expect(onSwitch).toHaveBeenCalledWith('research-notes');
    expect(
      stateModule.dom.sessionDrawerList?.querySelector(
        '[data-session-id="main"] [data-session-action="delete"]',
      ),
    ).toBeNull();

    const deleteButton = stateModule.dom.sessionDrawerList?.querySelector<HTMLButtonElement>(
      '[data-session-id="project-alpha"] [data-session-action="delete"]',
    );
    deleteButton?.click();

    expect(onDelete).toHaveBeenCalledWith('project-alpha');
  });

  it('keeps main as the first rendered session regardless of recency order', async () => {
    stateModule.state.sessions = [
      { id: 'project-alpha', name: 'Project Alpha', updated_at: 20 },
      { id: 'main', name: 'Main', updated_at: 1 },
      { id: 'research-notes', name: 'Research Notes', updated_at: 10 },
    ];
    stateModule.state.activeSessionId = 'project-alpha';

    sessionsRendererModule.initSessionDrawer({
      onCreate: vi.fn(),
      onDelete: vi.fn(),
      onSwitch: vi.fn(),
    });

    const rows = stateModule.dom.sessionDrawerList?.querySelectorAll('.session-drawer-row');

    expect(rows?.[0]?.getAttribute('data-session-id')).toBe('main');
  });

  it('renders a rename action for healthy sessions', async () => {
    const onRename = vi.fn();
    stateModule.state.sessions = [
      { id: 'main', name: 'Main' },
      { id: 'research-notes', name: 'Research Notes' },
    ];
    stateModule.state.activeSessionId = 'main';

    sessionsRendererModule.initSessionDrawer({
      onCreate: vi.fn(),
      onDelete: vi.fn(),
      onRename,
      onSwitch: vi.fn(),
    });

    const renameButton = stateModule.dom.sessionDrawerList?.querySelector<HTMLButtonElement>(
      '[data-session-id="research-notes"] [data-session-action="rename"]',
    );
    renameButton?.click();

    expect(onRename).toHaveBeenCalledWith('research-notes');
  });

  it('allows deleting a corrupt inactive session but does not switch into it', async () => {
    stateModule.state.sessions = [
      { id: 'main', name: 'Main' },
      { id: 'corrupt-session', name: '[Corrupt Session]', corrupt: true },
    ];
    stateModule.state.activeSessionId = 'main';

    expect(
      utilsModule.pendingDeleteSessionIdForSelection(
        stateModule.state.sessions,
        stateModule.state.activeSessionId,
        'corrupt-session',
        '',
      ),
    ).toBe('corrupt-session');

    expect(
      utilsModule.shouldSwitchToSelectedSession(
        stateModule.state.sessions,
        stateModule.state.activeSessionId,
        'corrupt-session',
      ),
    ).toBe(false);

    const onSwitch = vi.fn();
    const onDelete = vi.fn();
    sessionsRendererModule.initSessionDrawer({
      onCreate: vi.fn(),
      onDelete,
      onSwitch,
    });

    const switchButton = stateModule.dom.sessionDrawerList?.querySelector<HTMLButtonElement>(
      '[data-session-id="corrupt-session"] [data-session-action="switch"]',
    );
    switchButton?.click();

    const deleteButton = stateModule.dom.sessionDrawerList?.querySelector<HTMLButtonElement>(
      '[data-session-id="corrupt-session"] [data-session-action="delete"]',
    );
    deleteButton?.click();

    expect(switchButton?.disabled).toBe(true);
    expect(onSwitch).not.toHaveBeenCalled();
    expect(onDelete).toHaveBeenCalledWith('corrupt-session');
  });

  it('prefers the normalized previous session target for healthy sessions', async () => {
    stateModule.state.sessions = [
      { id: 'main', name: 'Main' },
      { id: 'research-notes', name: 'Research Notes' },
      { id: 'project-alpha', name: 'Project Alpha' },
    ];
    stateModule.state.activeSessionId = 'project-alpha';

    expect(
      utilsModule.pendingDeleteSessionIdForSelection(
        stateModule.state.sessions,
        stateModule.state.activeSessionId,
        'research-notes',
        'research-notes',
      ),
    ).toBe('research-notes');
  });

  it('shows a pending row and disables drawer controls while switching sessions', async () => {
    stateModule.state.sessions = [{ id: 'main', name: 'Main' }];
    stateModule.state.activeSessionId = 'research-notes';
    stateModule.state.sessionSwitchInFlight = true;

    sessionsRendererModule.initSessionDrawer({
      onCreate: vi.fn(),
      onDelete: vi.fn(),
      onSwitch: vi.fn(),
    });

    const pendingRow = stateModule.dom.sessionDrawerList?.querySelector(
      '[data-session-id="research-notes"]',
    );
    const pendingBadge = pendingRow?.querySelector('.session-drawer-row-badge');
    const pendingSwitchButton = pendingRow?.querySelector<HTMLButtonElement>(
      '[data-session-action="switch"]',
    );

    expect(stateModule.dom.sessionDrawerNewBtn?.disabled).toBe(true);
    expect(pendingRow).not.toBeNull();
    expect(pendingBadge?.textContent).toBe('Switching');
    expect(pendingSwitchButton?.disabled).toBe(true);
  });

  it('marks an existing target session as switching while the session reconnect is in flight', async () => {
    stateModule.state.sessions = [
      { id: 'main', name: 'Main' },
      { id: 'research-notes', name: 'Research Notes' },
    ];
    stateModule.state.activeSessionId = 'research-notes';
    stateModule.state.sessionSwitchInFlight = true;

    sessionsRendererModule.initSessionDrawer({
      onCreate: vi.fn(),
      onDelete: vi.fn(),
      onSwitch: vi.fn(),
    });

    const targetRow = stateModule.dom.sessionDrawerList?.querySelector(
      '[data-session-id="research-notes"]',
    );
    const targetBadge = targetRow?.querySelector('.session-drawer-row-badge');
    const targetSwitchButton = targetRow?.querySelector<HTMLButtonElement>(
      '[data-session-action="switch"]',
    );

    expect(targetRow?.classList.contains('is-pending')).toBe(true);
    expect(targetBadge?.textContent).toBe('Switching');
    expect(targetSwitchButton?.disabled).toBe(true);
  });

  it('keeps the active session visible when the drawer list has not caught up yet', async () => {
    stateModule.state.sessions = [{ id: 'main', name: 'Main' }];
    stateModule.state.activeSessionId = 'research-notes';
    stateModule.state.sessionSwitchInFlight = false;

    sessionsRendererModule.initSessionDrawer({
      onCreate: vi.fn(),
      onDelete: vi.fn(),
      onSwitch: vi.fn(),
    });

    const activeRow = stateModule.dom.sessionDrawerList?.querySelector(
      '[data-session-id="research-notes"]',
    );
    const activeBadge = activeRow?.querySelector('.session-drawer-row-badge');
    const activeSwitchButton = activeRow?.querySelector<HTMLButtonElement>(
      '[data-session-action="switch"]',
    );

    expect(activeRow).not.toBeNull();
    expect(activeBadge?.textContent).toBe('Current');
    expect(activeSwitchButton?.disabled).toBe(true);
  });

  it('drops the session switch lock when reconnect finally fails', async () => {
    const { connect } = await import('../src/socket.js');
    const sockets: Array<{ onclose?: () => void; close: ReturnType<typeof vi.fn> }> = [];

    stateModule.state.sessionSwitchInFlight = true;
    mockWebSocket.mockImplementation(() => {
      const socket = {
        close: vi.fn(),
        onopen: undefined,
        onclose: undefined,
        onerror: undefined,
        onmessage: undefined,
        addEventListener: vi.fn(),
        removeEventListener: vi.fn(),
        dispatchEvent: vi.fn(),
        send: vi.fn(),
        readyState: 3,
      };
      sockets.push(socket);
      return socket as unknown as WebSocket;
    });

    connect(() => {});
    stateModule.state.reconnectAttempts = 3;
    sockets[0].onclose?.();

    expect(stateModule.state.sessionSwitchInFlight).toBe(false);
  });

  it('keeps sessionSwitchInFlight true during reconnect cleanup until the new session payload arrives', async () => {
    const { reconnectToActiveSession } = await import('../src/socket.js');

    stateModule.state.sessionSwitchInFlight = true;

    reconnectToActiveSession(() => {});

    expect(stateModule.state.sessionSwitchInFlight).toBe(true);
  });

  it('does not clear the pending delete target before the delete request returns', async () => {
    stateModule.state.pendingDeleteSessionId = 'research-notes';

    const targetSessionId = stateModule.state.pendingDeleteSessionId;

    expect(targetSessionId).toBe('research-notes');
    expect(stateModule.state.pendingDeleteSessionId).toBe('research-notes');
  });
});
