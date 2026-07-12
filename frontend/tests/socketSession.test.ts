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
    localStorage.clear();
    vi.resetModules();
    stateModule = await import('../src/state.js');
    sessionsRendererModule = await import('../src/renderers/sessions.js');
    utilsModule = await import('../src/utils.js');
    const { setLanguage } = await import('../src/i18n.js');
    setLanguage('en');
    mountSessionDrawerDom();
    stateModule.state.activeSessionId = '';
    stateModule.state.activeGroupId = '';
    stateModule.state.pendingDeleteSessionId = '';
    stateModule.state.reconnectDelay = 1000;
    stateModule.state.reconnectAttempts = 0;
    stateModule.state.sessionSwitchInFlight = false;
    stateModule.state.sessionIdentityMutationInFlight = false;
    stateModule.state.composerSessionTransitionPending = false;
    stateModule.state.composerSessionIdentityPending = false;
    stateModule.state.imageUploadInFlight = false;
    stateModule.state.sessionDrawerExpanded = true;
    stateModule.state.sessions = [];
    stateModule.state.sessionGroups = [];

    (globalThis as unknown as { WebSocket: unknown }).WebSocket =
      mockWebSocket as unknown as typeof WebSocket;
    mockWebSocket.mockReset();
  });

  it('connects to default websocket path when no active session is selected', async () => {
    const { connect } = await import('../src/socket.js');

    connect(() => {});

    expect(mockWebSocket).toHaveBeenCalledWith('ws://localhost:3000/ws');
  });

  it('starts a model revision handshake when the socket opens', async () => {
    vi.stubGlobal('fetch', vi.fn().mockReturnValue(new Promise(() => {})));
    const { acceptComposerConfigRevision, acceptComposerSocketModelPayloadRevision } =
      await import('../src/composerAvailability.js');
    const { connect } = await import('../src/socket.js');
    stateModule.state.composerConfigRevision = 50;
    stateModule.state.composerSessionModelRevision = 50;

    connect(() => {});
    const socket = mockWebSocket.mock.instances[0] as unknown as { onopen?: () => void };
    socket.onopen?.();

    // An HTTP response cannot consume the connection-scoped handshake.
    expect(acceptComposerConfigRevision(49)).toBe(false);
    expect(acceptComposerSocketModelPayloadRevision(5)).toBe(true);
    expect(stateModule.state.composerConfigRevision).toBe(5);
    expect(stateModule.state.composerSessionModelRevision).toBeNull();
  });

  it('retranslates the current connection state without resetting it to offline', async () => {
    const { setLanguage } = await import('../src/i18n.js');
    const { connect, refreshConnectionStatus } = await import('../src/socket.js');

    setLanguage('en');
    connect(() => {});
    expect(stateModule.dom.connLabel?.textContent).toBe('Connecting...');

    setLanguage('zh-CN');
    refreshConnectionStatus();

    expect(stateModule.dom.connDot?.className).toBe('conn-dot connecting');
    expect(stateModule.dom.connLabel?.textContent).toBe('连接中...');
  });

  it('connects to the selected websocket session when active session is restored', async () => {
    const { connect } = await import('../src/socket.js');
    stateModule.state.activeSessionId = 'research-notes';

    connect(() => {});

    expect(mockWebSocket).toHaveBeenCalledWith('ws://localhost:3000/ws?session=research-notes');
  });

  it('connects to the selected websocket group when active group is restored', async () => {
    const { connect } = await import('../src/socket.js');
    stateModule.state.activeSessionId = 'main';
    stateModule.state.activeGroupId = 'review-group';

    connect(() => {});

    expect(mockWebSocket).toHaveBeenCalledWith(
      'ws://localhost:3000/ws?group=review-group&session=main',
    );
  });

  it('uses main session query for group sockets even when another session is active', async () => {
    const { connect } = await import('../src/socket.js');
    stateModule.state.activeSessionId = 'worker-a';
    stateModule.state.activeGroupId = 'review-group';

    connect(() => {});

    expect(mockWebSocket).toHaveBeenCalledWith(
      'ws://localhost:3000/ws?group=review-group&session=main',
    );
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

  it('hides session delete actions while a group chat is active', async () => {
    const onDelete = vi.fn();
    stateModule.state.sessions = [
      { id: 'main', name: 'Main' },
      { id: 'project-alpha', name: 'Project Alpha' },
    ];
    stateModule.state.activeSessionId = 'main';
    stateModule.state.activeGroupId = 'review-group';
    stateModule.state.sessionGroups = [{ id: 'review-group', name: 'Review Group' }];

    sessionsRendererModule.initSessionDrawer({
      onCreate: vi.fn(),
      onDelete,
      onSwitch: vi.fn(),
      onSwitchGroup: vi.fn(),
    });

    expect(
      stateModule.dom.sessionDrawerList?.querySelector(
        '[data-session-id="project-alpha"] [data-session-action="delete"]',
      ),
    ).toBeNull();
    expect(onDelete).not.toHaveBeenCalled();
  });

  it('keeps the current session row clickable so mobile navigation can close', async () => {
    const onSwitch = vi.fn();
    stateModule.state.sessions = [
      { id: 'main', name: 'Main' },
      { id: 'research-notes', name: 'Research Notes' },
    ];
    stateModule.state.activeSessionId = 'research-notes';

    sessionsRendererModule.initSessionDrawer({
      onCreate: vi.fn(),
      onDelete: vi.fn(),
      onSwitch,
    });

    const currentButton = stateModule.dom.sessionDrawerList?.querySelector<HTMLButtonElement>(
      '[data-session-id="research-notes"] [data-session-action="switch"]',
    );

    expect(currentButton?.disabled).toBe(false);
    expect(currentButton?.getAttribute('aria-current')).toBe('true');
    expect(currentButton?.getAttribute('aria-label')).toBe('Current session: Research Notes');
    currentButton?.click();
    expect(onSwitch).toHaveBeenCalledWith('research-notes');
  });

  it('keeps invalid session ids disabled', async () => {
    const onSwitch = vi.fn();
    const onRename = vi.fn();
    stateModule.state.sessions = [
      { id: '', name: 'Invalid Session' },
      { id: '   ', name: 'Whitespace Session' },
    ];

    sessionsRendererModule.initSessionDrawer({
      onCreate: vi.fn(),
      onDelete: vi.fn(),
      onRename,
      onSwitch,
    });

    const invalidButton = stateModule.dom.sessionDrawerList?.querySelector<HTMLButtonElement>(
      '[data-session-id=""] [data-session-action="switch"]',
    );
    const invalidRow = invalidButton?.closest('.session-drawer-row');
    expect(invalidButton?.disabled).toBe(true);
    expect(invalidButton?.getAttribute('aria-label')).toBe('Unavailable session Invalid Session');
    expect(invalidButton?.hasAttribute('aria-current')).toBe(false);
    expect(invalidRow?.classList.contains('is-active')).toBe(false);
    expect(invalidRow?.classList.contains('is-disabled')).toBe(true);
    expect(invalidRow?.querySelector('.session-drawer-row-badge')?.textContent).toBe('Unavailable');
    expect(
      stateModule.dom.sessionDrawerList?.querySelector<HTMLButtonElement>(
        '[data-session-id="   "] [data-session-action="switch"]',
      )?.disabled,
    ).toBe(true);
    expect(
      stateModule.dom.sessionDrawerList?.querySelector('.session-drawer-row-actions'),
    ).toBeNull();
    invalidButton?.click();
    expect(onSwitch).not.toHaveBeenCalled();
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

  it('renders duplicate valid session ids only once', async () => {
    stateModule.state.sessions = [
      { id: 'research-notes', name: 'Research Notes', updated_at: 20 },
      { id: 'research-notes', name: 'Stale Duplicate', updated_at: 10 },
    ];
    stateModule.state.activeSessionId = 'research-notes';

    sessionsRendererModule.initSessionDrawer({
      onCreate: vi.fn(),
      onDelete: vi.fn(),
      onSwitch: vi.fn(),
    });

    expect(
      stateModule.dom.sessionDrawerList?.querySelectorAll('[data-session-id="research-notes"]'),
    ).toHaveLength(1);
    expect(
      stateModule.dom.sessionDrawerList?.querySelectorAll('[aria-current="true"]'),
    ).toHaveLength(1);
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

  it('renders group rows and wires group actions', async () => {
    const onSwitchGroup = vi.fn();
    const onRenameGroup = vi.fn();
    const onDeleteGroup = vi.fn();
    const onCreateGroup = vi.fn();
    stateModule.state.sessions = [{ id: 'main', name: 'Main' }];
    stateModule.state.sessionGroups = [
      {
        id: 'review-group',
        name: 'Review Group',
        members: 2,
        messages: 3,
        running: 1,
        updated_at: 40,
      },
    ];

    sessionsRendererModule.initSessionDrawer({
      onCreate: vi.fn(),
      onCreateGroup,
      onDelete: vi.fn(),
      onDeleteGroup,
      onRenameGroup,
      onSwitch: vi.fn(),
      onSwitchGroup,
    });

    const createGroupButton = stateModule.dom.sessionDrawerList?.querySelector<HTMLButtonElement>(
      '.session-drawer-section-action',
    );
    const switchGroupButton = stateModule.dom.sessionDrawerList?.querySelector<HTMLButtonElement>(
      '[data-group-id="review-group"] [data-session-action="switch-group"]',
    );
    const renameGroupButton = stateModule.dom.sessionDrawerList?.querySelector<HTMLButtonElement>(
      '[data-group-id="review-group"] [data-session-action="rename"]',
    );
    const deleteGroupButton = stateModule.dom.sessionDrawerList?.querySelector<HTMLButtonElement>(
      '[data-group-id="review-group"] [data-session-action="delete"]',
    );

    createGroupButton?.click();
    switchGroupButton?.click();
    renameGroupButton?.click();
    deleteGroupButton?.click();

    expect(createGroupButton?.querySelector('use')?.getAttribute('href')).toBe('#icon-plus');
    expect(onCreateGroup).toHaveBeenCalled();
    expect(onSwitchGroup).toHaveBeenCalledWith('review-group');
    expect(onRenameGroup).toHaveBeenCalledWith('review-group');
    expect(onDeleteGroup).toHaveBeenCalledWith('review-group');
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

  it('disables Session and Group navigation controls while an image upload is active', async () => {
    const onCreate = vi.fn();
    const onCreateGroup = vi.fn();
    const onSwitch = vi.fn();
    const onSwitchGroup = vi.fn();
    stateModule.state.sessions = [
      { id: 'main', name: 'Main' },
      { id: 'research-notes', name: 'Research Notes' },
    ];
    stateModule.state.sessionGroups = [{ id: 'review-group', name: 'Review Group' }];
    stateModule.state.activeSessionId = 'main';
    stateModule.state.imageUploadInFlight = true;

    sessionsRendererModule.initSessionDrawer({
      onCreate,
      onCreateGroup,
      onDelete: vi.fn(),
      onSwitch,
      onSwitchGroup,
    });

    const sessionButton = stateModule.dom.sessionDrawerList?.querySelector<HTMLButtonElement>(
      '[data-session-id="research-notes"] [data-session-action="switch"]',
    );
    const groupButton = stateModule.dom.sessionDrawerList?.querySelector<HTMLButtonElement>(
      '[data-group-id="review-group"] [data-session-action="switch-group"]',
    );
    const createGroupButton = stateModule.dom.sessionDrawerList?.querySelector<HTMLButtonElement>(
      '.session-drawer-section-action',
    );

    expect(stateModule.dom.sessionDrawerNewBtn?.disabled).toBe(true);
    expect(sessionButton?.disabled).toBe(true);
    expect(groupButton?.disabled).toBe(true);
    expect(createGroupButton?.disabled).toBe(true);
    stateModule.dom.sessionDrawerNewBtn?.click();
    sessionButton?.click();
    groupButton?.click();
    createGroupButton?.click();
    expect(onCreate).not.toHaveBeenCalled();
    expect(onCreateGroup).not.toHaveBeenCalled();
    expect(onSwitch).not.toHaveBeenCalled();
    expect(onSwitchGroup).not.toHaveBeenCalled();
  });

  it('disables identity navigation while a slash Session switch is awaiting confirmation', async () => {
    const onCreateGroup = vi.fn();
    const onSwitch = vi.fn();
    stateModule.state.sessions = [
      { id: 'main', name: 'Main' },
      { id: 'research-notes', name: 'Research Notes' },
    ];
    stateModule.state.sessionGroups = [{ id: 'review-group', name: 'Review Group' }];
    stateModule.state.activeSessionId = 'main';
    stateModule.state.composerSessionTransitionPending = true;

    sessionsRendererModule.initSessionDrawer({
      onCreate: vi.fn(),
      onCreateGroup,
      onDelete: vi.fn(),
      onSwitch,
      onSwitchGroup: vi.fn(),
    });

    const sessionButton = stateModule.dom.sessionDrawerList?.querySelector<HTMLButtonElement>(
      '[data-session-id="research-notes"] [data-session-action="switch"]',
    );
    const createGroupButton = stateModule.dom.sessionDrawerList?.querySelector<HTMLButtonElement>(
      '.session-drawer-section-action',
    );
    expect(stateModule.dom.sessionDrawerNewBtn?.disabled).toBe(true);
    expect(sessionButton?.disabled).toBe(true);
    expect(createGroupButton?.disabled).toBe(true);
    sessionButton?.click();
    createGroupButton?.click();
    expect(onSwitch).not.toHaveBeenCalled();
    expect(onCreateGroup).not.toHaveBeenCalled();
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

  it('marks only the target group as switching when entering group chat', async () => {
    stateModule.state.sessions = [{ id: 'main', name: 'Main' }];
    stateModule.state.activeSessionId = 'main';
    stateModule.state.activeGroupId = 'review-group';
    stateModule.state.sessionGroups = [{ id: 'review-group', name: 'Review Group' }];
    stateModule.state.sessionSwitchInFlight = true;

    sessionsRendererModule.initSessionDrawer({
      onCreate: vi.fn(),
      onDelete: vi.fn(),
      onSwitch: vi.fn(),
      onSwitchGroup: vi.fn(),
    });

    const pendingRows = stateModule.dom.sessionDrawerList?.querySelectorAll(
      '.session-drawer-row.is-pending',
    );
    const mainRow = stateModule.dom.sessionDrawerList?.querySelector('[data-session-id="main"]');
    const groupRow = stateModule.dom.sessionDrawerList?.querySelector(
      '[data-group-id="review-group"]',
    );

    expect(pendingRows).toHaveLength(1);
    expect(mainRow?.classList.contains('is-pending')).toBe(false);
    expect(mainRow?.querySelector('.session-drawer-row-badge')).toBeNull();
    expect(groupRow?.classList.contains('is-pending')).toBe(true);
    expect(groupRow?.querySelector('.session-drawer-row-badge')?.textContent).toBe('Switching');
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
    expect(activeSwitchButton?.disabled).toBe(false);
    expect(activeSwitchButton?.getAttribute('aria-current')).toBe('true');
    expect(activeSwitchButton?.getAttribute('aria-label')).toBe('Current session: research-notes');
  });

  it('keeps a restored active group visible while the group list is loading', async () => {
    stateModule.state.sessions = [{ id: 'main', name: 'Main' }];
    stateModule.state.activeSessionId = 'main';
    stateModule.state.activeGroupId = 'review-group';
    stateModule.state.sessionGroups = [];

    sessionsRendererModule.initSessionDrawer({
      onCreate: vi.fn(),
      onDelete: vi.fn(),
      onSwitch: vi.fn(),
      onSwitchGroup: vi.fn(),
    });

    const activeGroupRow = stateModule.dom.sessionDrawerList?.querySelector(
      '[data-group-id="review-group"]',
    );
    const activeGroupButton = activeGroupRow?.querySelector<HTMLButtonElement>(
      '[data-session-action="switch-group"]',
    );

    expect(activeGroupRow).not.toBeNull();
    expect(activeGroupRow?.classList.contains('is-active')).toBe(true);
    expect(activeGroupRow?.querySelector('.session-drawer-row-badge')?.textContent).toBe('Current');
    expect(activeGroupButton?.getAttribute('aria-current')).toBe('true');
    expect(activeGroupButton?.getAttribute('aria-label')).toBe('Current group: review-group');
  });

  it('drops the session switch lock when reconnect finally fails', async () => {
    const composerModule = await import('../src/composerAvailability.js');
    const { connect } = await import('../src/socket.js');
    const sockets: Array<{ onclose?: () => void; close: ReturnType<typeof vi.fn> }> = [];

    stateModule.state.sessionSwitchInFlight = true;
    composerModule.beginComposerSessionTransition(false, 'unreachable-session');
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
    expect(stateModule.state.composerSessionTransitionPending).toBe(false);
  });

  it('restores a pending slash switch before reconnecting the source Session', async () => {
    const composerModule = await import('../src/composerAvailability.js');
    const { connect } = await import('../src/socket.js');
    const sockets: Array<{ onclose?: () => void }> = [];
    mockWebSocket.mockImplementation(() => {
      const socket = {
        close: vi.fn(),
        onopen: undefined,
        onclose: undefined,
        onerror: undefined,
        onmessage: undefined,
        send: vi.fn(),
        readyState: 1,
      };
      sockets.push(socket);
      return socket as unknown as WebSocket;
    });
    stateModule.state.activeSessionId = 'main';
    composerModule.applyComposerConfig({}, true, 30);
    composerModule.setComposerExplicitPrimaryModelConfigured(true, 30);
    composerModule.setComposerSessionModelConfigured(false, false, true, 30);
    composerModule.beginComposerSessionTransition(true, 'target-session');
    expect(
      composerModule.updateComposerSessionTransitionFallback('main', true, false, false, true, 30),
    ).toBe(true);

    connect(() => {});
    stateModule.state.reconnectAttempts = 3;
    sockets[0].onclose?.();

    expect(stateModule.state.composerSessionTransitionPending).toBe(false);
    expect(stateModule.state.composerSessionModelRevision).toBe(30);
    expect(stateModule.state.composerSessionModelOverridePresent).toBe(true);
    expect(stateModule.state.composerModelAvailability).toBe('session-model-unconfigured');
  });

  it('keeps sessionSwitchInFlight true during reconnect cleanup until the new session payload arrives', async () => {
    const { reconnectToActiveSession } = await import('../src/socket.js');

    stateModule.state.sessionSwitchInFlight = true;

    reconnectToActiveSession(() => {});

    expect(stateModule.state.sessionSwitchInFlight).toBe(true);
  });

  it('detaches the previous socket message handler during reconnect', async () => {
    const sockets: Array<{
      close: ReturnType<typeof vi.fn>;
      onmessage?: ((event: { data: string }) => void) | null;
    }> = [];
    mockWebSocket.mockImplementation(() => {
      const socket = {
        close: vi.fn(),
        onopen: undefined,
        onclose: undefined,
        onerror: undefined,
        onmessage: undefined,
        send: vi.fn(),
        readyState: 1,
      };
      sockets.push(socket);
      return socket as unknown as WebSocket;
    });
    const onMessage = vi.fn();
    const { connect, reconnectToActiveSession } = await import('../src/socket.js');

    connect(onMessage);
    const staleHandler = sockets[0].onmessage;
    reconnectToActiveSession(onMessage);

    expect(sockets[0].onmessage).toBeNull();
    staleHandler?.({ data: JSON.stringify({ type: 'session', id: 'old' }) });
    expect(onMessage).not.toHaveBeenCalled();
  });

  it('does not clear the pending delete target before the delete request returns', async () => {
    stateModule.state.pendingDeleteSessionId = 'research-notes';

    const targetSessionId = stateModule.state.pendingDeleteSessionId;

    expect(targetSessionId).toBe('research-notes');
    expect(stateModule.state.pendingDeleteSessionId).toBe('research-notes');
  });
});
