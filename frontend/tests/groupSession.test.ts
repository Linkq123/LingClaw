import { beforeEach, describe, expect, it, vi } from 'vitest';

describe('group sessions', () => {
  beforeEach(async () => {
    vi.resetModules();
    localStorage.clear();
    document.body.innerHTML = `
      <aside id="session-drawer">
        <button id="session-drawer-toggle-btn"></button>
        <button id="session-drawer-new-btn"></button>
        <div id="session-drawer-list"></div>
      </aside>
    `;
    const { state } = await import('../src/state.js');
    state.composerModelAvailability = 'ready';
    state.sessionSwitchInFlight = false;
    state.sessionIdentityMutationInFlight = false;
    state.composerSessionTransitionPending = false;
    state.composerSessionIdentityPending = false;
    state.imageUploadInFlight = false;
    state.groupsEnabled = true;
  });

  it('persists and clears the active group id', async () => {
    const {
      ACTIVE_GROUP_STORAGE_KEY,
      ACTIVE_SESSION_STORAGE_KEY,
      isRecoverableActiveGroupConnectionError,
      loadActiveGroupId,
      loadActiveSessionId,
      mainSessionStateForGroupControl,
      persistActiveGroupId,
      persistActiveSessionId,
      sessionIdAfterLeavingGroup,
      shouldApplyGroupRunStatusUpdate,
    } = await import('../src/sessionPersistence.js');

    persistActiveSessionId('worker-a');
    persistActiveGroupId(' review-group ');

    const groupSessionState = mainSessionStateForGroupControl(loadActiveSessionId());

    expect(localStorage.getItem(ACTIVE_GROUP_STORAGE_KEY)).toBe('review-group');
    expect(localStorage.getItem(ACTIVE_SESSION_STORAGE_KEY)).toBe('worker-a');
    expect(loadActiveGroupId()).toBe('review-group');
    expect(groupSessionState).toEqual({
      activeSessionId: 'main',
      groupReturnSessionId: 'worker-a',
    });
    expect('pendingDeleteSessionId' in groupSessionState).toBe(false);
    expect(sessionIdAfterLeavingGroup(groupSessionState.groupReturnSessionId, '')).toBe('worker-a');

    persistActiveGroupId('');

    expect(localStorage.getItem(ACTIVE_GROUP_STORAGE_KEY)).toBeNull();
    expect(loadActiveGroupId()).toBe('');
    expect(
      isRecoverableActiveGroupConnectionError("Group 'review-group' not found", 'review-group'),
    ).toBe(true);
    expect(isRecoverableActiveGroupConnectionError('Invalid group id', 'review-group')).toBe(true);
    expect(isRecoverableActiveGroupConnectionError('Invalid session id.', 'review-group')).toBe(
      true,
    );
    expect(isRecoverableActiveGroupConnectionError('Invalid session id.', '')).toBe(false);
    expect(mainSessionStateForGroupControl('worker-a')).toEqual({
      activeSessionId: 'main',
      groupReturnSessionId: 'worker-a',
    });
    expect(mainSessionStateForGroupControl(' main ')).toEqual({
      activeSessionId: 'main',
      groupReturnSessionId: '',
    });
    expect(
      shouldApplyGroupRunStatusUpdate({ status: 'completed', updatedAt: 10 }, 'running', 10),
    ).toBe(false);
    expect(
      shouldApplyGroupRunStatusUpdate({ status: 'completed', updatedAt: 10 }, 'running', 9),
    ).toBe(false);
    expect(
      shouldApplyGroupRunStatusUpdate({ status: 'running', updatedAt: 10 }, 'completed', 10),
    ).toBe(true);
  });

  it('renders group drawer rows and exposes group callbacks', async () => {
    const stateModule = await import('../src/state.js');
    const sessionsRenderer = await import('../src/renderers/sessions.js');
    stateModule.initDomRefs();
    stateModule.state.sessionDrawerExpanded = true;
    stateModule.state.sessions = [{ id: 'main', name: 'Main' }];
    stateModule.state.sessionGroups = [
      {
        id: 'review-group',
        name: 'Review Group',
        members: 2,
        messages: 3,
        running: 1,
        updated_at: 10,
      },
    ];

    const onCreateGroup = vi.fn();
    const onSwitchGroup = vi.fn();
    const onRenameGroup = vi.fn();
    const onDeleteGroup = vi.fn();
    sessionsRenderer.initSessionDrawer({
      onCreate: vi.fn(),
      onCreateGroup,
      onDelete: vi.fn(),
      onDeleteGroup,
      onRenameGroup,
      onSwitch: vi.fn(),
      onSwitchGroup,
    });

    const groupRow = stateModule.dom.sessionDrawerList?.querySelector(
      '[data-group-id="review-group"]',
    );
    const groupSwitchButton = groupRow?.querySelector<HTMLButtonElement>(
      '[data-session-action="switch-group"]',
    );
    groupSwitchButton?.click();
    const menuTrigger = groupRow?.querySelector<HTMLButtonElement>('[data-session-action="menu"]');
    menuTrigger?.click();
    stateModule.dom.sessionDrawer
      ?.querySelector<HTMLButtonElement>('[role="menu"] [data-session-action="rename"]')
      ?.click();
    menuTrigger?.click();
    stateModule.dom.sessionDrawer
      ?.querySelector<HTMLButtonElement>('[role="menu"] [data-session-action="delete"]')
      ?.click();
    stateModule.dom.sessionDrawerList
      ?.querySelector<HTMLButtonElement>('.session-drawer-section-action')
      ?.click();

    expect(groupRow?.textContent).toContain('Review Group');
    expect(groupRow?.textContent).toContain('1 running');
    expect(onSwitchGroup).toHaveBeenCalledWith('review-group');
    expect(onRenameGroup).toHaveBeenCalledWith('review-group');
    expect(onDeleteGroup).toHaveBeenCalledWith('review-group');
    expect(onCreateGroup).toHaveBeenCalled();
  });

  it('uses a localized neutral label for the current group', async () => {
    const stateModule = await import('../src/state.js');
    const { setLanguage } = await import('../src/i18n.js');
    const sessionsRenderer = await import('../src/renderers/sessions.js');
    setLanguage('en');
    stateModule.initDomRefs();
    stateModule.state.sessions = [{ id: 'main', name: 'Main' }];
    stateModule.state.activeGroupId = 'review-group';
    stateModule.state.sessionGroups = [
      {
        id: 'review-group',
        name: 'Review Group',
        members: 2,
        messages: 3,
        running: 0,
        updated_at: 10,
      },
    ];
    sessionsRenderer.initSessionDrawer({
      onCreate: vi.fn(),
      onDelete: vi.fn(),
      onSwitch: vi.fn(),
      onSwitchGroup: vi.fn(),
    });

    const currentGroupButton = stateModule.dom.sessionDrawerList?.querySelector<HTMLButtonElement>(
      '[data-session-action="switch-group"]',
    );
    expect(currentGroupButton?.getAttribute('aria-current')).toBe('true');
    expect(currentGroupButton?.getAttribute('aria-label')).toBe('Current group: Review Group');
    const activeRows = stateModule.dom.sessionDrawerList?.querySelectorAll(
      '.session-drawer-row.is-active',
    );
    expect(activeRows).toHaveLength(1);
    expect(activeRows?.[0]?.getAttribute('data-group-id')).toBe('review-group');

    setLanguage('zh-CN');
    sessionsRenderer.renderSessionDrawer();
    expect(
      stateModule.dom.sessionDrawerList
        ?.querySelector<HTMLButtonElement>('[data-session-action="switch-group"]')
        ?.getAttribute('aria-label'),
    ).toBe('当前群聊：Review Group');
  });

  it('renders duplicate valid group ids only once', async () => {
    const stateModule = await import('../src/state.js');
    const sessionsRenderer = await import('../src/renderers/sessions.js');
    stateModule.initDomRefs();
    stateModule.state.activeGroupId = 'review-group';
    stateModule.state.sessionGroups = [
      { id: 'review-group', name: 'Review Group', updated_at: 20 },
      { id: 'review-group', name: 'Stale Duplicate', updated_at: 10 },
    ];

    sessionsRenderer.initSessionDrawer({
      onCreate: vi.fn(),
      onDelete: vi.fn(),
      onSwitch: vi.fn(),
      onSwitchGroup: vi.fn(),
    });

    expect(
      stateModule.dom.sessionDrawerList?.querySelectorAll('[data-group-id="review-group"]'),
    ).toHaveLength(1);
    expect(
      stateModule.dom.sessionDrawerList?.querySelectorAll('[aria-current="true"]'),
    ).toHaveLength(1);
  });

  it('keeps invalid group ids disabled', async () => {
    const stateModule = await import('../src/state.js');
    const sessionsRenderer = await import('../src/renderers/sessions.js');
    stateModule.initDomRefs();
    stateModule.state.sessionGroups = [
      {
        id: '',
        name: 'Invalid Group',
        members: 0,
        messages: 0,
        running: 0,
        updated_at: 0,
      },
    ];
    const onSwitchGroup = vi.fn();
    const onRenameGroup = vi.fn();
    const onDeleteGroup = vi.fn();
    sessionsRenderer.initSessionDrawer({
      onCreate: vi.fn(),
      onDelete: vi.fn(),
      onSwitch: vi.fn(),
      onSwitchGroup,
      onRenameGroup,
      onDeleteGroup,
    });

    const invalidGroupButton = stateModule.dom.sessionDrawerList?.querySelector<HTMLButtonElement>(
      '[data-session-action="switch-group"]',
    );
    const invalidGroupRow = invalidGroupButton?.closest('.session-drawer-row');
    expect(invalidGroupButton?.disabled).toBe(true);
    expect(invalidGroupButton?.getAttribute('aria-label')).toBe('Unavailable group Invalid Group');
    expect(invalidGroupButton?.hasAttribute('aria-current')).toBe(false);
    expect(invalidGroupRow?.classList.contains('is-active')).toBe(false);
    expect(invalidGroupRow?.classList.contains('is-disabled')).toBe(true);
    expect(invalidGroupRow?.querySelector('.session-drawer-row-badge')?.textContent).toBe(
      'Unavailable',
    );
    expect(
      stateModule.dom.sessionDrawerList?.querySelector('.session-drawer-row-actions'),
    ).toBeNull();
    invalidGroupButton?.click();
    expect(onSwitchGroup).not.toHaveBeenCalled();
  });

  it('sends group messages with selected and mentions target modes', async () => {
    document.body.innerHTML = `
      <main id="chat"></main>
      <div id="input-area">
        <div id="group-target-bar"></div>
        <div id="slash-command-menu" hidden></div>
        <textarea id="input"></textarea>
        <button id="stop"></button>
        <button id="send"></button>
        <span id="send-icon"></span>
      </div>
    `;
    const stateModule = await import('../src/state.js');
    stateModule.initDomRefs();
    const sendMock = vi.fn();
    stateModule.state.ws = { readyState: 1, send: sendMock } as unknown as WebSocket;
    stateModule.state.activeGroupId = 'review-group';
    stateModule.state.activeGroupMembers = ['worker-a', 'worker-b'];
    stateModule.state.groupModelConfiguredMembers = new Set(['worker-a', 'worker-b']);
    stateModule.state.groupTargetMode = 'selected';
    stateModule.state.groupSelectedTargets = ['worker-b', 'missing-worker'];
    stateModule.state.pendingImages = [];
    stateModule.state.planModeEnabled = false;

    const { send, stopAgent } = await import('../src/input.js');
    stateModule.dom.input!.value = 'check backend';
    send();

    expect(JSON.parse(sendMock.mock.calls[0][0])).toEqual({
      type: 'group_message',
      text: 'check backend',
      targets: ['worker-b'],
      target_mode: 'selected',
      start_runs: true,
      run_mode: 'execute',
    });
    expect(stateModule.state.busy).toBe(true);

    stateModule.state.groupTargetMode = 'mentions';
    stateModule.dom.input!.value = '@worker-a check frontend';
    send();

    expect(JSON.parse(sendMock.mock.calls[1][0])).toEqual({
      type: 'group_message',
      text: '@worker-a check frontend',
      targets: [],
      target_mode: 'mentions',
      start_runs: true,
      run_mode: 'execute',
    });

    stateModule.dom.input!.value = 'check frontend without a mention';
    send();
    expect(sendMock).toHaveBeenCalledTimes(2);
    expect(stateModule.dom.chat?.textContent).toContain(
      'Mention at least one group member before sending.',
    );

    stopAgent();

    expect(JSON.parse(sendMock.mock.calls[2][0])).toEqual({
      type: 'group_stop',
    });
  });

  it('does not send slash commands as group messages', async () => {
    document.body.innerHTML = `
      <main id="chat"></main>
      <div id="input-area">
        <div id="group-target-bar"></div>
        <div id="slash-command-menu" hidden></div>
        <textarea id="input"></textarea>
      </div>
    `;
    const stateModule = await import('../src/state.js');
    stateModule.initDomRefs();
    const sendMock = vi.fn();
    stateModule.state.ws = { readyState: 1, send: sendMock } as unknown as WebSocket;
    stateModule.state.activeGroupId = 'review-group';
    stateModule.state.activeGroupMembers = ['worker-a'];
    stateModule.state.groupTargetMode = 'all';
    stateModule.state.pendingImages = [];

    const { send, sendCmd } = await import('../src/input.js');
    stateModule.dom.input!.value = '/status';
    send();
    sendCmd('/help');

    expect(sendMock).not.toHaveBeenCalled();
    expect(stateModule.dom.chat?.textContent).toContain(
      'Slash commands are not supported in group chat.',
    );
  });

  it('only sends group messages when every selected target has an effective model', async () => {
    document.body.innerHTML = `
      <div id="input-area">
        <div id="group-target-bar"></div>
        <div id="slash-command-menu" hidden></div>
        <textarea id="input"></textarea>
        <button id="stop"></button>
        <button id="send"></button>
        <span id="send-icon"></span>
      </div>
    `;
    const stateModule = await import('../src/state.js');
    stateModule.initDomRefs();
    const sendMock = vi.fn();
    stateModule.state.ws = { readyState: 1, send: sendMock } as unknown as WebSocket;
    stateModule.state.composerModelAvailability = 'agent-model-unconfigured';
    stateModule.state.activeGroupId = 'review-group';
    stateModule.state.activeGroupMembers = ['worker-a', 'worker-b'];
    stateModule.state.groupTargetMode = 'selected';
    stateModule.state.groupSelectedTargets = ['worker-a'];
    stateModule.state.groupModelConfiguredMembers = new Set(['worker-a']);
    stateModule.state.pendingImages = [];

    const { send } = await import('../src/input.js');
    stateModule.dom.input!.value = 'run configured target';
    send();
    expect(sendMock).toHaveBeenCalledTimes(1);

    stateModule.state.busy = false;
    stateModule.state.groupSelectedTargets = ['worker-b'];
    stateModule.dom.input!.value = 'do not use fallback';
    send();
    expect(sendMock).toHaveBeenCalledTimes(1);
    expect(stateModule.dom.input?.value).toBe('do not use fallback');
  });
});
