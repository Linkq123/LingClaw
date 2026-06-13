import { beforeEach, describe, expect, it, vi } from 'vitest';

describe('group sessions', () => {
  beforeEach(() => {
    vi.resetModules();
    localStorage.clear();
    document.body.innerHTML = `
      <aside id="session-drawer">
        <button id="session-drawer-toggle-btn"></button>
        <button id="session-drawer-new-btn"></button>
        <div id="session-drawer-list"></div>
      </aside>
    `;
  });

  it('persists and clears the active group id', async () => {
    const {
      ACTIVE_GROUP_STORAGE_KEY,
      isRecoverableActiveGroupConnectionError,
      loadActiveGroupId,
      persistActiveGroupId,
    } = await import('../src/sessionPersistence.js');

    persistActiveGroupId(' review-group ');

    expect(localStorage.getItem(ACTIVE_GROUP_STORAGE_KEY)).toBe('review-group');
    expect(loadActiveGroupId()).toBe('review-group');

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

    const groupRow = stateModule.dom.sessionDrawerList?.querySelector('[data-group-id="review-group"]');
    groupRow?.querySelector<HTMLButtonElement>('[data-session-action="switch-group"]')?.click();
    groupRow?.querySelector<HTMLButtonElement>('[data-session-action="rename"]')?.click();
    groupRow?.querySelector<HTMLButtonElement>('[data-session-action="delete"]')?.click();
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

  it('sends group messages with selected and mentions target modes', async () => {
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
    stateModule.state.activeGroupId = 'review-group';
    stateModule.state.activeGroupMembers = ['worker-a', 'worker-b'];
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
});
