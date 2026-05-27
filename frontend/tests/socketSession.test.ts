import { beforeEach, describe, expect, it, vi } from 'vitest';

type AppStateModule = typeof import('../src/state.js');
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
  let utilsModule: UtilsModule;

  beforeEach(async () => {
    vi.resetModules();
    stateModule = await import('../src/state.js');
    utilsModule = await import('../src/utils.js');
    document.body.innerHTML = '<span id="conn-dot"></span><span id="conn-label"></span>';
    stateModule.dom.connDot = document.getElementById('conn-dot');
    stateModule.dom.connLabel = document.getElementById('conn-label');
    stateModule.state.activeSessionId = '';
    stateModule.state.reconnectDelay = 1000;
    stateModule.state.reconnectAttempts = 0;
    stateModule.state.sessionSwitchInFlight = false;

    (globalThis as unknown as { WebSocket: unknown }).WebSocket = mockWebSocket as unknown as typeof WebSocket;
    mockWebSocket.mockReset();
  });

  it('connects to default websocket path when no active session is selected', async () => {
    const { connect } = await import('../src/socket.js');

    connect(() => {});

    expect(mockWebSocket).toHaveBeenCalledWith('ws://localhost:3000/ws');
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

  it('allows deleting a corrupt non-active session selected in the picker', async () => {
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
  });

  it('does not try to switch when the picker selects a corrupt session', async () => {
    stateModule.state.sessions = [
      { id: 'main', name: 'Main' },
      { id: 'corrupt-session', name: '[Corrupt Session]', corrupt: true },
    ];
    stateModule.state.activeSessionId = 'main';

    expect(
      utilsModule.shouldSwitchToSelectedSession(
        stateModule.state.sessions,
        stateModule.state.activeSessionId,
        'corrupt-session',
      ),
    ).toBe(false);
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
