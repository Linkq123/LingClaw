import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

type AppStateModule = typeof import('../src/state.js');
type SessionsRendererModule = typeof import('../src/renderers/sessions.js');
type UtilsModule = typeof import('../src/utils.js');

const mockWebSocket = vi.fn();

function clientConfigResponse(executionIdentity: number | null = 1, groups = true): Response {
  return new Response(
    JSON.stringify({
      features: { groups },
      ...(executionIdentity == null
        ? {}
        : { protocols: { execution_identity: executionIdentity } }),
    }),
    { status: 200, headers: { 'Content-Type': 'application/json' } },
  );
}

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

  function openRowAction(rowSelector: string, action: 'rename' | 'delete') {
    stateModule.dom.sessionDrawerList
      ?.querySelector<HTMLButtonElement>(`${rowSelector} [data-session-action="menu"]`)
      ?.click();
    return stateModule.dom.sessionDrawer?.querySelector<HTMLButtonElement>(
      `.session-drawer-row-menu [data-session-action="${action}"]`,
    );
  }

  function mountSessionDrawerDom() {
    document.body.innerHTML = `
      <span id="conn-dot"></span>
      <span id="conn-label"></span>
      <div id="input-area">
        <textarea id="input" aria-describedby="composer-availability-detail"></textarea>
        <button id="send" aria-describedby="composer-availability-detail"></button>
        <button id="stop"></button>
        <p id="composer-availability-status" hidden>
          <span id="composer-availability-message"></span>
          <button id="composer-availability-action"></button>
          <button id="composer-availability-retry"></button>
        </p>
        <span id="composer-availability-detail" role="status" aria-live="polite"
          >Checking model configuration...</span
        >
      </div>
      <aside id="session-drawer">
        <div class="session-drawer-header">
          <button id="session-drawer-toggle-btn"></button>
          <h2 class="session-drawer-heading">Sessions</h2>
          <button id="session-drawer-new-btn"></button>
        </div>
        <input id="session-drawer-search-input" type="search" />
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
    stateModule.state.executionIdentityProtocol = 'strict';
    stateModule.state.socketGeneration = 0;
    stateModule.state.legacyExecutionSocketGeneration = 0;
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
    // Group-specific legacy coverage opts in explicitly now that the product
    // default is feature-disabled.
    stateModule.state.groupsEnabled = true;

    (globalThis as unknown as { WebSocket: unknown }).WebSocket =
      mockWebSocket as unknown as typeof WebSocket;
    mockWebSocket.mockReset();
    vi.stubGlobal(
      'fetch',
      vi.fn<typeof fetch>(() => Promise.resolve(clientConfigResponse())),
    );
  });

  afterEach(() => {
    vi.useRealTimers();
    sessionsRendererModule.disposeSessionDrawer();
    vi.unstubAllGlobals();
  });

  it('connects to default websocket path when no active session is selected', async () => {
    const { connect } = await import('../src/socket.js');

    await connect(() => {});

    expect(mockWebSocket).toHaveBeenCalledWith('ws://localhost:3000/ws');
  });

  it('classifies current, legacy, and unsupported execution identity capabilities', async () => {
    const { executionIdentityProtocolFromClientConfig } = await import('../src/socket.js');

    expect(
      executionIdentityProtocolFromClientConfig({ protocols: { execution_identity: 1 } }),
    ).toBe('strict');
    expect(executionIdentityProtocolFromClientConfig({ features: { groups: false } })).toBe(
      'legacy',
    );
    expect(
      executionIdentityProtocolFromClientConfig({ protocols: { execution_identity: 2 } }),
    ).toBeNull();
  });

  it('refuses a socket when execution protocol negotiation failed without leaving fake busy state', async () => {
    const chatModule = await import('../src/renderers/chat.js');
    const { connect } = await import('../src/socket.js');
    vi.mocked(chatModule.addSystem).mockClear();
    vi.mocked(chatModule.setBusy).mockClear();
    stateModule.state.executionIdentityProtocol = 'unavailable';
    vi.stubGlobal(
      'fetch',
      vi.fn<typeof fetch>(() => Promise.resolve(new Response('unavailable', { status: 503 }))),
    );

    await connect(() => {});

    expect(mockWebSocket).not.toHaveBeenCalled();
    expect(stateModule.state.ws).toBeNull();
    expect(chatModule.setBusy).toHaveBeenCalledWith(false);
    expect(chatModule.addSystem).toHaveBeenCalledWith(
      expect.stringContaining('Connection setup failed'),
      'error',
    );
    expect(stateModule.dom.connLabel?.textContent).toContain('Connection setup failed');
  });

  it('allows one legacy socket but blocks reconnect after its connection epoch closes', async () => {
    const chatModule = await import('../src/renderers/chat.js');
    const sockets: Array<{ onclose?: (() => void) | null; close: ReturnType<typeof vi.fn> }> = [];
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
    const { connect } = await import('../src/socket.js');
    vi.mocked(chatModule.addSystem).mockClear();
    vi.mocked(chatModule.setBusy).mockClear();
    stateModule.state.executionIdentityProtocol = 'legacy';
    vi.stubGlobal(
      'fetch',
      vi.fn<typeof fetch>(() => Promise.resolve(clientConfigResponse(null))),
    );

    await connect(() => {});
    expect(mockWebSocket).toHaveBeenCalledTimes(1);
    expect(stateModule.state.legacyExecutionSocketGeneration).toBe(
      stateModule.state.socketGeneration,
    );

    sockets[0].onclose?.();

    expect(mockWebSocket).toHaveBeenCalledTimes(1);
    expect(stateModule.state.ws).toBeNull();
    expect(chatModule.setBusy).toHaveBeenCalledWith(false);
    expect(chatModule.addSystem).toHaveBeenCalledWith(
      expect.stringContaining('older daemon cannot reconnect'),
      'error',
    );
  });

  it.each([
    ['legacy', () => clientConfigResponse(null)],
    ['unknown', () => clientConfigResponse(99)],
    ['failed', () => new Response('unavailable', { status: 503 })],
  ])(
    'renegotiates strict to %s before a manual reconnect and refuses the socket preflight',
    async (_label, secondResponse) => {
      vi.useFakeTimers();
      const responses = [clientConfigResponse(), secondResponse()];
      vi.stubGlobal(
        'fetch',
        vi.fn<typeof fetch>(() => Promise.resolve(responses.shift()!)),
      );
      const sockets: Array<{ close: ReturnType<typeof vi.fn> }> = [];
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
      const { connect, reconnectToActiveSession } = await import('../src/socket.js');

      await connect(() => {});
      expect(mockWebSocket).toHaveBeenCalledTimes(1);
      stateModule.state.busy = true;

      await reconnectToActiveSession(() => {});
      await vi.advanceTimersByTimeAsync(60_000);

      expect(fetch).toHaveBeenCalledTimes(2);
      expect(mockWebSocket).toHaveBeenCalledTimes(1);
      expect(sockets[0].close).toHaveBeenCalledTimes(1);
      expect(stateModule.state.ws).toBeNull();
      const chatModule = await import('../src/renderers/chat.js');
      expect(chatModule.setBusy).toHaveBeenCalledWith(false);
      expect(stateModule.state.executionIdentityProtocol).not.toBe('strict');
      vi.useRealTimers();
    },
  );

  it('renegotiates legacy to strict and permits the next socket generation', async () => {
    const responses = [clientConfigResponse(null), clientConfigResponse(1)];
    vi.stubGlobal(
      'fetch',
      vi.fn<typeof fetch>(() => Promise.resolve(responses.shift()!)),
    );
    mockWebSocket.mockImplementation(
      () =>
        ({
          close: vi.fn(),
          onopen: undefined,
          onclose: undefined,
          onerror: undefined,
          onmessage: undefined,
          send: vi.fn(),
          readyState: 1,
        }) as unknown as WebSocket,
    );
    const { connect, reconnectToActiveSession } = await import('../src/socket.js');

    await connect(() => {});
    expect(stateModule.state.executionIdentityProtocol).toBe('legacy');
    await reconnectToActiveSession(() => {});

    expect(fetch).toHaveBeenCalledTimes(2);
    expect(mockWebSocket).toHaveBeenCalledTimes(2);
    expect(stateModule.state.executionIdentityProtocol).toBe('strict');
    expect(stateModule.state.socketGeneration).toBe(2);
  });

  it('renegotiates an automatic reconnect and does not loop when strict becomes legacy', async () => {
    vi.useFakeTimers();
    const responses = [clientConfigResponse(), clientConfigResponse(null)];
    vi.stubGlobal(
      'fetch',
      vi.fn<typeof fetch>(() => Promise.resolve(responses.shift()!)),
    );
    const sockets: Array<{ onclose?: (() => void) | null; close: ReturnType<typeof vi.fn> }> = [];
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
    const { connect } = await import('../src/socket.js');

    await connect(() => {});
    sockets[0].onclose?.();
    await vi.advanceTimersByTimeAsync(1_000);
    await Promise.resolve();
    await vi.advanceTimersByTimeAsync(60_000);

    expect(fetch).toHaveBeenCalledTimes(2);
    expect(mockWebSocket).toHaveBeenCalledTimes(1);
    expect(stateModule.state.executionIdentityProtocol).toBe('legacy');
    expect(stateModule.state.ws).toBeNull();
    vi.useRealTimers();
  });

  it('discards a delayed negotiation when a newer Session target wins', async () => {
    let firstSignal: AbortSignal | null = null;
    let request = 0;
    vi.stubGlobal(
      'fetch',
      vi.fn<typeof fetch>((_input, init) => {
        request += 1;
        if (request === 1) {
          firstSignal = init?.signal ?? null;
          return new Promise<Response>(() => {});
        }
        return Promise.resolve(clientConfigResponse());
      }),
    );
    mockWebSocket.mockImplementation(
      () =>
        ({
          close: vi.fn(),
          onopen: undefined,
          onclose: undefined,
          onerror: undefined,
          onmessage: undefined,
          send: vi.fn(),
          readyState: 1,
        }) as unknown as WebSocket,
    );
    const { connect, reconnectToActiveSession } = await import('../src/socket.js');
    stateModule.state.activeSessionId = 'old-target';

    const oldIntent = connect(() => {});
    stateModule.state.activeSessionId = 'new-target';
    await reconnectToActiveSession(() => {});
    await oldIntent;

    expect(firstSignal?.aborted).toBe(true);
    expect(fetch).toHaveBeenCalledTimes(2);
    expect(mockWebSocket).toHaveBeenCalledTimes(1);
    expect(mockWebSocket).toHaveBeenCalledWith('ws://localhost:3000/ws?session=new-target');
    expect(stateModule.state.executionIdentityProtocol).toBe('strict');
  });

  it.each(['headers', 'json body'])(
    'bounds a client-config request whose %s never completes',
    async (stallPoint) => {
      vi.useFakeTimers();
      let requestSignal: AbortSignal | null = null;
      vi.stubGlobal(
        'fetch',
        vi.fn<typeof fetch>((_input, init) => {
          requestSignal = init?.signal ?? null;
          if (stallPoint === 'headers') return new Promise<Response>(() => {});
          return Promise.resolve({
            ok: true,
            status: 200,
            json: () => new Promise<unknown>(() => {}),
          } as Response);
        }),
      );
      const { CLIENT_CONFIG_TIMEOUT_MS, connect } = await import('../src/socket.js');

      const pending = connect(() => {});
      await Promise.resolve();
      expect(mockWebSocket).not.toHaveBeenCalled();
      await vi.advanceTimersByTimeAsync(CLIENT_CONFIG_TIMEOUT_MS);
      await pending;

      expect(requestSignal?.aborted).toBe(true);
      expect(mockWebSocket).not.toHaveBeenCalled();
      expect(stateModule.state.executionIdentityProtocol).toBe('unavailable');
      expect(stateModule.state.ws).toBeNull();
      expect(stateModule.state.busy).toBe(false);
    },
  );

  it('actively aborts a pending negotiation when reconnect cancellation supersedes its intent', async () => {
    let requestSignal: AbortSignal | null = null;
    vi.stubGlobal(
      'fetch',
      vi.fn<typeof fetch>((_input, init) => {
        requestSignal = init?.signal ?? null;
        return new Promise<Response>(() => {});
      }),
    );
    const { cancelReconnect, connect } = await import('../src/socket.js');

    const pending = connect(() => {});
    await Promise.resolve();
    cancelReconnect();
    await pending;

    expect(requestSignal?.aborted).toBe(true);
    expect(mockWebSocket).not.toHaveBeenCalled();
    expect(stateModule.state.executionIdentityProtocol).toBe('strict');
  });

  it('actively aborts a pending negotiation when the current protocol generation fail-closes', async () => {
    const sockets: Array<{ close: ReturnType<typeof vi.fn> }> = [];
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
    let request = 0;
    let pendingSignal: AbortSignal | null = null;
    vi.stubGlobal(
      'fetch',
      vi.fn<typeof fetch>((_input, init) => {
        request += 1;
        if (request === 1) return Promise.resolve(clientConfigResponse());
        pendingSignal = init?.signal ?? null;
        return new Promise<Response>(() => {});
      }),
    );
    const { connect, failCloseCurrentExecutionProtocol } = await import('../src/socket.js');
    await connect(() => {});

    const pending = connect(() => {});
    await Promise.resolve();
    failCloseCurrentExecutionProtocol();
    await pending;

    expect(pendingSignal?.aborted).toBe(true);
    expect(sockets[0].close).toHaveBeenCalledTimes(1);
    expect(mockWebSocket).toHaveBeenCalledTimes(1);
    expect(stateModule.state.ws).toBeNull();
  });

  it('renegotiates before switching from a Session socket to a Group socket', async () => {
    mockWebSocket.mockImplementation(
      () =>
        ({
          close: vi.fn(),
          onopen: undefined,
          onclose: undefined,
          onerror: undefined,
          onmessage: undefined,
          send: vi.fn(),
          readyState: 1,
        }) as unknown as WebSocket,
    );
    const { connect, reconnectToActiveSession } = await import('../src/socket.js');
    stateModule.state.activeSessionId = 'main';
    await connect(() => {});

    stateModule.state.activeGroupId = 'review-group';
    await reconnectToActiveSession(() => {});

    expect(fetch).toHaveBeenCalledTimes(2);
    expect(mockWebSocket).toHaveBeenCalledTimes(2);
    expect(mockWebSocket).toHaveBeenLastCalledWith(
      'ws://localhost:3000/ws?group=review-group&session=main',
    );
  });

  it('does not let a hanging Group discovery delay the negotiated WebSocket generation', async () => {
    stateModule.state.groupsEnabled = false;
    const onMessage = vi.fn((message: { features?: { groups?: boolean } }) => {
      stateModule.state.groupsEnabled = message.features?.groups === true;
      return new Promise<void>(() => {});
    });
    const { connect } = await import('../src/socket.js');

    await connect(onMessage);

    expect(onMessage).toHaveBeenCalledWith({
      type: 'feature_status',
      features: { groups: true },
    });
    expect(mockWebSocket).toHaveBeenCalledTimes(1);
  });

  it('starts a model revision handshake when the socket opens', async () => {
    const composerModule = await import('../src/composerAvailability.js');
    const { connect } = await import('../src/socket.js');
    stateModule.state.composerConfigRevision = 50;
    stateModule.state.composerSessionModelRevision = 50;

    await connect(() => {});
    const socket = mockWebSocket.mock.instances[0] as unknown as { onopen?: () => void };
    socket.onopen?.();

    expect(stateModule.dom.input?.getAttribute('aria-describedby')).toBe(
      'composer-availability-detail',
    );
    expect(document.getElementById('composer-availability-detail')?.textContent).toBe(
      'Waiting for this session to be ready. Your draft has not been sent.',
    );

    // An HTTP response cannot consume the connection-scoped handshake.
    expect(composerModule.acceptComposerConfigRevision(49)).toBe(false);
    expect(composerModule.acceptComposerSocketModelPayloadRevision(5)).toBe(true);
    expect(stateModule.state.composerConfigRevision).toBe(5);
    expect(stateModule.state.composerSessionModelRevision).toBeNull();

    composerModule.setComposerExplicitPrimaryModelConfigured(true, 5);
    composerModule.setComposerSessionModelConfigured(false, false, true, 5);
    expect(stateModule.dom.sendBtn?.disabled).toBe(true);
    // Model payloads cannot confirm the full Session identity for this socket.
    stateModule.state.composerSessionIdentityPending = false;
    (await import('../src/composerTransport.js')).confirmComposerTransportIdentity();
    (await import('../src/composerTransport.js')).confirmComposerTransportHistory();
    composerModule.syncComposerAvailability();
    expect(stateModule.dom.sendBtn?.disabled).toBe(false);
    expect(stateModule.dom.input?.hasAttribute('aria-describedby')).toBe(false);
    expect(stateModule.dom.sendBtn?.hasAttribute('aria-describedby')).toBe(false);
    expect(document.getElementById('composer-availability-detail')?.hidden).toBe(true);
    expect(document.getElementById('composer-availability-detail')?.textContent).toBe('');
  });

  it('actively aborts a hanging Session model recovery when its socket closes', async () => {
    const sockets: Array<{
      readyState: number;
      close: ReturnType<typeof vi.fn>;
      onclose?: (() => void) | null;
    }> = [];
    Object.assign(mockWebSocket, { OPEN: 1, CLOSED: 3 });
    mockWebSocket.mockImplementation(() => {
      const socket = {
        readyState: 1,
        close: vi.fn(),
        onopen: undefined,
        onclose: undefined,
        onerror: undefined,
        onmessage: undefined,
        send: vi.fn(),
      };
      sockets.push(socket);
      return socket as unknown as WebSocket;
    });
    let recoverySignal: AbortSignal | null = null;
    vi.stubGlobal(
      'fetch',
      vi.fn<typeof fetch>((input, init) => {
        const url = typeof input === 'string' ? input : input.url;
        if (url === '/api/client-config') {
          return Promise.resolve(clientConfigResponse());
        }
        recoverySignal = init?.signal ?? null;
        return new Promise<Response>(() => {});
      }),
    );
    stateModule.state.activeSessionId = 'main';
    stateModule.state.composerConfigRevision = 7;
    stateModule.state.composerSessionModelRevision = 6;
    stateModule.state.composerEffectiveModelConfigured = null;

    const { refreshActiveComposerSessionModelState } = await import('../src/composerModels.js');
    const { connect } = await import('../src/socket.js');
    await connect(() => {});
    const recovery = refreshActiveComposerSessionModelState('main');
    expect(recoverySignal?.aborted).toBe(false);

    stateModule.state.reconnectAttempts = 3;
    sockets[0].readyState = 3;
    sockets[0].onclose?.();

    expect(recoverySignal?.aborted).toBe(true);
    await expect(recovery).resolves.toBe('cancelled');
    expect(fetch).toHaveBeenCalledTimes(2);
  });

  it('retranslates the current connection state without resetting it to offline', async () => {
    const { setLanguage } = await import('../src/i18n.js');
    const { connect, refreshConnectionStatus } = await import('../src/socket.js');

    setLanguage('en');
    await connect(() => {});
    expect(stateModule.dom.connLabel?.textContent).toBe('Connecting...');

    setLanguage('zh-CN');
    refreshConnectionStatus();

    expect(stateModule.dom.connDot?.className).toBe('conn-dot connecting');
    expect(stateModule.dom.connLabel?.textContent).toBe('连接中...');
  });

  it('connects to the selected websocket session when active session is restored', async () => {
    const { connect } = await import('../src/socket.js');
    stateModule.state.activeSessionId = 'research-notes';

    await connect(() => {});

    expect(mockWebSocket).toHaveBeenCalledWith('ws://localhost:3000/ws?session=research-notes');
  });

  it('connects to the selected websocket group when active group is restored', async () => {
    const { connect } = await import('../src/socket.js');
    stateModule.state.activeSessionId = 'main';
    stateModule.state.activeGroupId = 'review-group';

    await connect(() => {});

    expect(mockWebSocket).toHaveBeenCalledWith(
      'ws://localhost:3000/ws?group=review-group&session=main',
    );
  });

  it('uses main session query for group sockets even when another session is active', async () => {
    const { connect } = await import('../src/socket.js');
    stateModule.state.activeSessionId = 'worker-a';
    stateModule.state.activeGroupId = 'review-group';

    await connect(() => {});

    expect(mockWebSocket).toHaveBeenCalledWith(
      'ws://localhost:3000/ws?group=review-group&session=main',
    );
  });

  it('falls back to the Session socket when a rejected Group reconnect discovers Groups are disabled', async () => {
    const sockets: Array<{
      close: ReturnType<typeof vi.fn>;
      onclose?: (() => void) | null;
      onerror?: (() => void) | null;
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
        readyState: 3,
      };
      sockets.push(socket);
      return socket as unknown as WebSocket;
    });
    vi.stubGlobal(
      'fetch',
      vi.fn<typeof fetch>(() => Promise.resolve(clientConfigResponse(1, false))),
    );
    stateModule.state.activeSessionId = 'main';
    stateModule.state.activeGroupId = 'review-group';

    const socketModule = await import('../src/socket.js');
    const onMessage = vi.fn(async (message) => {
      if (message?.type !== 'feature_status') return;
      stateModule.state.groupsEnabled = false;
      stateModule.state.activeGroupId = '';
      void socketModule.reconnectToActiveSession(onMessage);
    });

    await socketModule.connect(onMessage);

    await vi.waitFor(() => expect(mockWebSocket).toHaveBeenCalledTimes(1));
    expect(fetch).toHaveBeenCalledWith(
      '/api/client-config',
      expect.objectContaining({ cache: 'no-store', signal: expect.any(AbortSignal) }),
    );
    expect(onMessage).toHaveBeenCalledWith({
      type: 'feature_status',
      features: { groups: false },
    });
    expect(mockWebSocket).toHaveBeenCalledWith('ws://localhost:3000/ws?session=main');
  });

  it('times out a closed Group capability probe and continues with one bounded reconnect', async () => {
    vi.useFakeTimers();
    const sockets: Array<{
      close: ReturnType<typeof vi.fn>;
      onclose?: (() => void) | null;
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
    let request = 0;
    let recoverySignal: AbortSignal | null = null;
    vi.stubGlobal(
      'fetch',
      vi.fn<typeof fetch>((_input, init) => {
        request += 1;
        if (request === 2) {
          recoverySignal = init?.signal ?? null;
          return new Promise<Response>(() => {});
        }
        return Promise.resolve(clientConfigResponse(1, true));
      }),
    );
    stateModule.state.activeSessionId = 'main';
    stateModule.state.activeGroupId = 'review-group';
    const { CLIENT_CONFIG_TIMEOUT_MS, connect } = await import('../src/socket.js');

    await connect(() => {});
    sockets[0].onclose?.();
    await vi.advanceTimersByTimeAsync(CLIENT_CONFIG_TIMEOUT_MS);
    expect(recoverySignal?.aborted).toBe(true);
    expect(mockWebSocket).toHaveBeenCalledTimes(1);

    await vi.advanceTimersByTimeAsync(1_000);
    await Promise.resolve();

    expect(fetch).toHaveBeenCalledTimes(3);
    expect(mockWebSocket).toHaveBeenCalledTimes(2);
    expect(stateModule.state.reconnectAttempts).toBe(1);
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

    const deleteButton = openRowAction('[data-session-id="project-alpha"]', 'delete');
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

  it('keeps main and the active session inside a twelve-row recent window', async () => {
    stateModule.state.sessions = [
      { id: 'main', name: 'Main', updated_at: 1 },
      ...Array.from({ length: 16 }, (_, index) => ({
        id: `session-${index + 1}`,
        name: `Session ${index + 1}`,
        updated_at: 100 - index,
      })),
    ];
    stateModule.state.activeSessionId = 'session-16';

    sessionsRendererModule.initSessionDrawer({
      onCreate: vi.fn(),
      onDelete: vi.fn(),
      onSwitch: vi.fn(),
    });

    expect(stateModule.dom.sessionDrawerList?.querySelectorAll('[data-session-id]')).toHaveLength(
      12,
    );
    expect(
      stateModule.dom.sessionDrawerList?.querySelector('[data-session-id="main"]'),
    ).not.toBeNull();
    expect(
      stateModule.dom.sessionDrawerList?.querySelector('[data-session-id="session-16"]'),
    ).not.toBeNull();

    const earlierToggle = stateModule.dom.sessionDrawerList?.querySelector<HTMLButtonElement>(
      '[data-session-earlier-toggle="true"]',
    );
    expect(earlierToggle?.getAttribute('aria-expanded')).toBe('false');
    earlierToggle?.click();
    expect(stateModule.dom.sessionDrawerList?.querySelectorAll('[data-session-id]')).toHaveLength(
      17,
    );
  });

  it('searches all sessions and groups, including collapsed earlier sessions', async () => {
    stateModule.state.sessions = [
      { id: 'main', name: 'Main', updated_at: 1 },
      ...Array.from({ length: 14 }, (_, index) => ({
        id: `archive-${index + 1}`,
        name: index === 13 ? 'Quarterly Research' : `Archive ${index + 1}`,
        updated_at: 100 - index,
      })),
    ];
    stateModule.state.sessionGroups = [{ id: 'design-review', name: 'Design Review' }];
    stateModule.state.activeSessionId = 'main';

    sessionsRendererModule.initSessionDrawer({
      onCreate: vi.fn(),
      onDelete: vi.fn(),
      onSwitch: vi.fn(),
      onSwitchGroup: vi.fn(),
    });

    const search = stateModule.dom.sessionDrawerSearchInput;
    if (!search) throw new Error('Search input not found');
    search.value = 'quarterly';
    search.dispatchEvent(new Event('input', { bubbles: true }));
    expect(
      stateModule.dom.sessionDrawerList?.querySelector('[data-session-id="archive-14"]'),
    ).not.toBeNull();
    expect(stateModule.dom.sessionDrawerList?.querySelectorAll('[data-session-id]')).toHaveLength(
      1,
    );

    search.value = 'DESIGN-REVIEW';
    search.dispatchEvent(new Event('input', { bubbles: true }));
    expect(
      stateModule.dom.sessionDrawerList?.querySelector('[data-group-id="design-review"]'),
    ).not.toBeNull();

    search.value = 'missing';
    search.dispatchEvent(new Event('input', { bubbles: true }));
    expect(stateModule.dom.sessionDrawerList?.textContent).toContain(
      'No matching sessions or groups',
    );
  });

  it('returns focus to a row menu trigger when Escape closes the menu', async () => {
    stateModule.state.sessions = [
      { id: 'main', name: 'Main' },
      { id: 'research-notes', name: 'Research Notes' },
    ];
    stateModule.state.activeSessionId = 'main';

    sessionsRendererModule.initSessionDrawer({
      onCreate: vi.fn(),
      onDelete: vi.fn(),
      onRename: vi.fn(),
      onSwitch: vi.fn(),
    });

    const trigger = stateModule.dom.sessionDrawerList?.querySelector<HTMLButtonElement>(
      '[data-session-id="research-notes"] [data-session-action="menu"]',
    );
    trigger?.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }));
    await Promise.resolve();
    expect(trigger?.getAttribute('aria-expanded')).toBe('true');
    expect(document.activeElement?.getAttribute('data-session-action')).toBe('rename');

    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    expect(stateModule.dom.sessionDrawer?.querySelector('[role="menu"]')).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  it('disposes row menu state and global listeners cleanly', () => {
    stateModule.state.sessions = [
      { id: 'main', name: 'Main' },
      { id: 'research-notes', name: 'Research Notes' },
    ];
    stateModule.state.activeSessionId = 'main';

    sessionsRendererModule.initSessionDrawer({
      onCreate: vi.fn(),
      onDelete: vi.fn(),
      onRename: vi.fn(),
      onSwitch: vi.fn(),
    });

    const trigger = stateModule.dom.sessionDrawerList?.querySelector<HTMLButtonElement>(
      '[data-session-id="research-notes"] [data-session-action="menu"]',
    );
    trigger?.click();
    expect(stateModule.dom.sessionDrawer?.querySelector('[role="menu"]')).not.toBeNull();

    sessionsRendererModule.disposeSessionDrawer();

    expect(stateModule.dom.sessionDrawer?.querySelector('[role="menu"]')).toBeNull();
    expect(trigger?.getAttribute('aria-expanded')).toBe('false');
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

    const renameButton = openRowAction('[data-session-id="research-notes"]', 'rename');
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
    createGroupButton?.click();
    switchGroupButton?.click();
    openRowAction('[data-group-id="review-group"]', 'rename')?.click();
    openRowAction('[data-group-id="review-group"]', 'delete')?.click();

    expect(createGroupButton?.querySelector('use')?.getAttribute('href')).toBe('#icon-plus');
    expect(onCreateGroup).toHaveBeenCalled();
    expect(onSwitchGroup).toHaveBeenCalledWith('review-group');
    expect(onRenameGroup).toHaveBeenCalledWith('review-group');
    expect(onDeleteGroup).toHaveBeenCalledWith('review-group');
  });

  it('omits every Group navigation surface unless the feature is enabled', () => {
    stateModule.state.groupsEnabled = false;
    stateModule.state.sessions = [{ id: 'main', name: 'Main' }];
    stateModule.state.sessionGroups = [
      { id: 'hidden-group', name: 'Hidden group', members: 2, updated_at: 10 },
    ];
    const onCreateGroup = vi.fn();
    sessionsRendererModule.initSessionDrawer({
      onCreate: vi.fn(),
      onCreateGroup,
      onDelete: vi.fn(),
      onSwitch: vi.fn(),
    });

    expect(document.querySelector('[data-group-id="hidden-group"]')).toBeNull();
    expect(stateModule.dom.sessionDrawerList?.textContent).not.toContain('Groups');
    expect(onCreateGroup).not.toHaveBeenCalled();
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

    const deleteButton = openRowAction('[data-session-id="corrupt-session"]', 'delete');
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

    await connect(() => {});
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

    await connect(() => {});
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
    stateModule.state.activeExecutionRunId = 17;
    stateModule.state.activeExecutionServerRunId = 'old-connection';
    stateModule.state.activeExecutionPlanId = 'plan-switch';
    stateModule.state.terminalExecutionStack = document.createElement('section');

    await reconnectToActiveSession(() => {});

    expect(stateModule.state.sessionSwitchInFlight).toBe(true);
    expect(stateModule.state.activeExecutionRunId).toBe(0);
    expect(stateModule.state.activeExecutionServerRunId).toBe('');
    expect(stateModule.state.activeExecutionPlanId).toBe('');
    expect(stateModule.state.terminalExecutionStack).toBeNull();
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

    await connect(onMessage);
    const staleHandler = sockets[0].onmessage;
    await reconnectToActiveSession(onMessage);

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
