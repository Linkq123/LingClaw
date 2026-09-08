import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ACTIVE_GROUP_STORAGE_KEY, ACTIVE_SESSION_STORAGE_KEY } from '../src/sessionPersistence.js';

const indexHtml = readFileSync(resolve(process.cwd(), 'index.html'), 'utf8');

interface StartupScenario {
  persistedSessionId: string;
  sessions: Array<{ id: string; name: string }>;
  sessionListStatus?: number;
  sessionIdsCaseSensitive?: boolean;
  persistedGroupId?: string;
  groupsEnabled?: boolean;
  clientConfigStatus?: number;
  executionIdentityProtocol?: number | null;
  expectSocket?: boolean;
  clientConfigResponder?: (request: number, signal: AbortSignal | null) => Promise<Response>;
  blockLocalStoragePropertyAtImport?: boolean;
}

interface StartedWorkspace {
  events: string[];
  socketUrl: URL | null;
  socket: TestWebSocket | null;
  stateModule: typeof import('../src/state.js');
}

interface TestWebSocket {
  readyState: number;
  onopen: (() => void) | null;
  onclose: (() => void) | null;
  onerror: (() => void) | null;
  onmessage: ((event: MessageEvent<string>) => void) | null;
  sent: unknown[];
  close(): void;
  receiveRaw(payload: unknown): void;
}

function jsonResponse(payload: unknown, status = 200): Response {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

async function startWorkspace(
  scenario: StartupScenario & { expectSocket: false },
): Promise<StartedWorkspace & { socketUrl: null }>;
async function startWorkspace(
  scenario: StartupScenario,
): Promise<StartedWorkspace & { socketUrl: URL }>;
async function startWorkspace(scenario: StartupScenario): Promise<StartedWorkspace> {
  const body = indexHtml.match(/<body[^>]*>([\s\S]*?)<\/body>/i)?.[1];
  if (!body) throw new Error('index.html body not found');
  document.body.innerHTML = body;
  localStorage.setItem(ACTIVE_SESSION_STORAGE_KEY, scenario.persistedSessionId);
  if (scenario.persistedGroupId) {
    localStorage.setItem(ACTIVE_GROUP_STORAGE_KEY, scenario.persistedGroupId);
  }

  const events: string[] = [];
  const socketUrls: string[] = [];
  const sockets: TestWebSocket[] = [];
  let clientConfigRequests = 0;

  class FakeWebSocket {
    static readonly OPEN = 1;
    static readonly CLOSED = 3;

    readyState = FakeWebSocket.OPEN;
    onopen: (() => void) | null = null;
    onclose: (() => void) | null = null;
    onerror: (() => void) | null = null;
    onmessage: ((event: MessageEvent<string>) => void) | null = null;
    sent: unknown[] = [];

    constructor(url: string) {
      events.push(`websocket:${url}`);
      socketUrls.push(url);
      sockets.push(this);
    }

    send(payload: unknown): void {
      this.sent.push(payload);
    }

    close(): void {
      this.readyState = FakeWebSocket.CLOSED;
    }

    receiveRaw(payload: unknown): void {
      this.onmessage?.(
        new MessageEvent('message', {
          data: JSON.stringify(payload),
        }),
      );
    }
  }

  vi.stubGlobal('WebSocket', FakeWebSocket);
  vi.stubGlobal(
    'fetch',
    vi.fn<typeof fetch>((input, init) => {
      const url =
        typeof input === 'string' ? input : input instanceof URL ? input.toString() : input.url;
      const method = init?.method ?? (input instanceof Request ? input.method : 'GET');
      events.push(`fetch:${method}:${url}`);

      if (url === '/api/config') {
        return Promise.resolve(
          jsonResponse({
            config: {},
            configuredModelsAvailable: false,
            explicitPrimaryModelConfigured: false,
            configRevision: 1,
          }),
        );
      }
      if (url === '/api/sessions') {
        if (scenario.sessionListStatus && scenario.sessionListStatus !== 200) {
          return Promise.resolve(
            jsonResponse({ error: 'Session list is unavailable.' }, scenario.sessionListStatus),
          );
        }
        return Promise.resolve(
          jsonResponse({
            session_ids_case_sensitive: scenario.sessionIdsCaseSensitive !== false,
            sessions: scenario.sessions.map((session) => ({
              ...session,
              updated_at: 1,
              corrupt: false,
            })),
          }),
        );
      }
      if (url === '/api/client-config') {
        clientConfigRequests += 1;
        if (scenario.clientConfigResponder) {
          return scenario.clientConfigResponder(clientConfigRequests, init?.signal ?? null);
        }
        if (scenario.clientConfigStatus && scenario.clientConfigStatus !== 200) {
          return Promise.resolve(
            jsonResponse(
              { error: 'Client capability negotiation is unavailable.' },
              scenario.clientConfigStatus,
            ),
          );
        }
        const protocols =
          scenario.executionIdentityProtocol === null
            ? undefined
            : { execution_identity: scenario.executionIdentityProtocol ?? 1 };
        return Promise.resolve(
          jsonResponse({
            upload_token: 'upload-token',
            s3_config_id: '',
            features: { groups: scenario.groupsEnabled === true },
            ...(protocols ? { protocols } : {}),
          }),
        );
      }
      if (url === '/api/session-groups') {
        return Promise.resolve(
          jsonResponse({
            groups: scenario.persistedGroupId
              ? [
                  {
                    id: scenario.persistedGroupId,
                    name: 'Persisted Group',
                    members: 1,
                    running: 0,
                    created_at: 1,
                    updated_at: 1,
                    corrupt: false,
                  },
                ]
              : [],
          }),
        );
      }
      if (url === '/api/health') {
        return Promise.resolve(jsonResponse({ version: 'test' }));
      }
      return Promise.resolve(jsonResponse({ error: `Unexpected fetch URL: ${url}` }, 404));
    }),
  );

  const originalStorage = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');
  if (scenario.blockLocalStoragePropertyAtImport) {
    Object.defineProperty(globalThis, 'localStorage', {
      configurable: true,
      get: () => {
        throw new DOMException('blocked', 'SecurityError');
      },
    });
  }

  try {
    await import('../src/main.js');
    const stateModule = await import('../src/state.js');
    if (scenario.expectSocket === false) {
      await vi.waitFor(() =>
        expect(stateModule.state.executionIdentityProtocol).toBe('unavailable'),
      );
      expect(socketUrls).toHaveLength(0);
      return { events, socketUrl: null, socket: null, stateModule };
    }
    await vi.waitFor(() => expect(socketUrls).toHaveLength(1));
    return { events, socketUrl: new URL(socketUrls[0]), socket: sockets[0], stateModule };
  } finally {
    if (scenario.blockLocalStoragePropertyAtImport) {
      if (originalStorage) {
        Object.defineProperty(globalThis, 'localStorage', originalStorage);
      } else {
        Reflect.deleteProperty(globalThis, 'localStorage');
      }
    }
  }
}

function expectSessionRestoreBeforeGroupAndSocket(events: string[]): void {
  const sessionListIndex = events.indexOf('fetch:GET:/api/sessions');
  const clientConfigIndex = events.indexOf('fetch:GET:/api/client-config');
  const websocketIndex = events.findIndex((event) => event.startsWith('websocket:'));
  expect(sessionListIndex).toBeGreaterThanOrEqual(0);
  expect(clientConfigIndex).toBeGreaterThan(sessionListIndex);
  expect(websocketIndex).toBeGreaterThan(clientConfigIndex);
}

function expectNoSessionCreate(events: string[]): void {
  expect(events).not.toContain('fetch:POST:/api/session');
}

function expectNoGroupDiscovery(events: string[]): void {
  expect(events).not.toContain('fetch:GET:/api/session-groups');
}

function makeDirectComposerReady(socket: TestWebSocket): void {
  socket.onopen?.();
  socket.receiveRaw({ type: 'history', messages: [] });
  socket.receiveRaw({
    type: 'session',
    id: 'main',
    name: 'Main',
    model: 'test/model',
    effort: 'off',
    modelOverridePresent: false,
    modelOverrideConfigured: false,
    effectiveModelConfigured: true,
    explicitPrimaryModelConfigured: true,
    capabilities: { image: false, s3: false },
    configRevision: 2,
  });
}

describe('startup Session restoration', () => {
  beforeEach(() => {
    vi.resetModules();
    vi.unstubAllGlobals();
    localStorage.clear();
  });

  afterEach(() => {
    vi.useRealTimers();
    localStorage.clear();
    vi.unstubAllGlobals();
  });

  it('continues startup with Summary density when the localStorage property is blocked', async () => {
    const { socketUrl, stateModule } = await startWorkspace({
      persistedSessionId: 'main',
      sessions: [{ id: 'main', name: 'Main' }],
      blockLocalStoragePropertyAtImport: true,
    });

    expect(socketUrl.searchParams.get('session')).toBe('main');
    expect(stateModule.state.reasoningDensity).toBe('summary');
  });

  it('negotiates an explicit legacy daemon before opening its first socket', async () => {
    const { events, socketUrl, stateModule } = await startWorkspace({
      persistedSessionId: 'main',
      sessions: [{ id: 'main', name: 'Main' }],
      executionIdentityProtocol: null,
    });

    expect(socketUrl.searchParams.get('session')).toBe('main');
    expect(stateModule.state.executionIdentityProtocol).toBe('legacy');
    expect(stateModule.state.legacyExecutionSocketGeneration).toBe(
      stateModule.state.socketGeneration,
    );
    expectSessionRestoreBeforeGroupAndSocket(events);
  });

  it('fails visibly before WebSocket creation when capability negotiation fails', async () => {
    const { events, stateModule } = await startWorkspace({
      persistedSessionId: 'main',
      sessions: [{ id: 'main', name: 'Main' }],
      clientConfigStatus: 503,
      expectSocket: false,
    });

    expect(stateModule.state.ws).toBeNull();
    expect(stateModule.state.busy).toBe(false);
    await vi.waitFor(() => expect(document.body.textContent).toContain('Connection setup failed'));
    expect(events.some((event) => event.startsWith('websocket:'))).toBe(false);
  });

  it.each([
    { stall: 'headers', staleProtocol: null },
    { stall: 'json body', staleProtocol: 99 },
  ])(
    'aborts a stalled bootstrap $stall negotiation when a Session switch wins',
    async ({ stall, staleProtocol }) => {
      let firstSignal: AbortSignal | null = null;
      let releaseStale!: () => void;
      let resolveHeaders: ((response: Response) => void) | null = null;
      let resolveBody: ((payload: unknown) => void) | null = null;
      const stalePayload = {
        features: { groups: false },
        ...(staleProtocol == null ? {} : { protocols: { execution_identity: staleProtocol } }),
      };
      const started = startWorkspace({
        persistedSessionId: 'main',
        sessions: [
          { id: 'main', name: 'Main' },
          { id: 'session-b', name: 'Session B' },
        ],
        clientConfigResponder: (request, signal) => {
          if (request > 1) {
            return Promise.resolve(
              jsonResponse({
                features: { groups: true },
                protocols: { execution_identity: 1 },
              }),
            );
          }
          firstSignal = signal;
          if (stall === 'headers') {
            const response = new Promise<Response>((resolve) => {
              resolveHeaders = resolve;
            });
            releaseStale = () => resolveHeaders?.(jsonResponse(stalePayload));
            return response;
          }
          const body = new Promise<unknown>((resolve) => {
            resolveBody = resolve;
          });
          releaseStale = () => resolveBody?.(stalePayload);
          return Promise.resolve({
            ok: true,
            status: 200,
            json: () => body,
          } as Response);
        },
      });
      const stateModule = await import('../src/state.js');
      const { reconnectToActiveSession } = await import('../src/socket.js');
      await vi.waitFor(() => expect(firstSignal).not.toBeNull());
      stateModule.state.activeSessionId = 'session-b';
      const switched = reconnectToActiveSession((message: unknown) => {
        const featureStatus = message as { type?: string; features?: { groups?: boolean } };
        if (featureStatus.type === 'feature_status') {
          stateModule.state.groupsEnabled = featureStatus.features?.groups === true;
        }
      });
      await vi.waitFor(() => expect(firstSignal?.aborted).toBe(true));
      await switched;
      const workspace = await started;

      releaseStale();
      await Promise.resolve();
      await Promise.resolve();
      expect(workspace.events.filter((event) => event.startsWith('websocket:'))).toHaveLength(1);
      expect(workspace.socketUrl.searchParams.get('session')).toBe('session-b');
      expect(workspace.socketUrl.searchParams.get('group')).toBeNull();
      expect(stateModule.state.activeSessionId).toBe('session-b');
      expect(stateModule.state.executionIdentityProtocol).toBe('strict');
      expect(stateModule.state.groupsEnabled).toBe(true);
      expect(stateModule.state.busy).toBe(false);
      expect(document.body.textContent).not.toContain('Connection setup failed');
    },
  );

  it('aborts bootstrap negotiation when a Group target connection supersedes it', async () => {
    let firstSignal: AbortSignal | null = null;
    let resolveFirst!: (response: Response) => void;
    const firstResponse = new Promise<Response>((resolve) => {
      resolveFirst = resolve;
    });
    const started = startWorkspace({
      persistedSessionId: 'main',
      sessions: [{ id: 'main', name: 'Main' }],
      clientConfigResponder: (request, signal) => {
        if (request === 1) {
          firstSignal = signal;
          return firstResponse;
        }
        return Promise.resolve(
          jsonResponse({
            features: { groups: true },
            protocols: { execution_identity: 1 },
          }),
        );
      },
    });
    const stateModule = await import('../src/state.js');
    const socketModule = await import('../src/socket.js');
    await vi.waitFor(() => expect(firstSignal).not.toBeNull());
    stateModule.state.groupsEnabled = true;
    stateModule.state.activeGroupId = 'group-b';

    const switched = socketModule.reconnectToActiveSession(() => {});
    await vi.waitFor(() => expect(firstSignal?.aborted).toBe(true));
    const workspace = await started;
    await switched;
    resolveFirst(
      jsonResponse({
        features: { groups: false },
        protocols: { execution_identity: 1 },
      }),
    );
    await Promise.resolve();

    expect(workspace.events.filter((event) => event.startsWith('websocket:'))).toHaveLength(1);
    expect(workspace.socketUrl.searchParams.get('group')).toBe('group-b');
    expect(stateModule.state.activeGroupId).toBe('group-b');
    expect(stateModule.state.groupsEnabled).toBe(true);
    expect(stateModule.state.executionIdentityProtocol).toBe('strict');
  });

  it('bounds bootstrap capability negotiation and fails closed without a socket', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    let firstSignal: AbortSignal | null = null;
    const started = startWorkspace({
      persistedSessionId: 'main',
      sessions: [{ id: 'main', name: 'Main' }],
      expectSocket: false,
      clientConfigResponder: (_request, signal) => {
        firstSignal = signal;
        return new Promise<Response>(() => {});
      },
    });
    await vi.waitFor(() => expect(firstSignal).not.toBeNull());

    await vi.advanceTimersByTimeAsync(5_000);
    const workspace = await started;

    expect(firstSignal?.aborted).toBe(true);
    expect(workspace.events.some((event) => event.startsWith('websocket:'))).toBe(false);
    expect(workspace.stateModule.state.executionIdentityProtocol).toBe('unavailable');
    expect(workspace.stateModule.state.groupsEnabled).toBe(false);
    expect(workspace.stateModule.state.busy).toBe(false);
  });

  it('rejects an unknown bootstrap execution identity protocol before WebSocket creation', async () => {
    const workspace = await startWorkspace({
      persistedSessionId: 'main',
      sessions: [{ id: 'main', name: 'Main' }],
      executionIdentityProtocol: 99,
      expectSocket: false,
    });

    expect(workspace.events.some((event) => event.startsWith('websocket:'))).toBe(false);
    expect(workspace.stateModule.state.executionIdentityProtocol).toBe('unavailable');
    expect(workspace.stateModule.state.groupsEnabled).toBe(false);
  });

  it('fail-closes the exact strict socket after a real message send receives identityless start', async () => {
    const workspace = await startWorkspace({
      persistedSessionId: 'main',
      sessions: [{ id: 'main', name: 'Main' }],
    });
    const socket = workspace.socket!;
    makeDirectComposerReady(socket);
    const inputModule = await import('../src/input.js');
    const socketModule = await import('../src/socket.js');
    const input = workspace.stateModule.dom.input;
    if (!input) throw new Error('composer input missing');
    input.value = 'real direct message';

    inputModule.send();
    expect(workspace.stateModule.state.busy).toBe(true);
    expect(socket.sent).toHaveLength(1);
    expect(workspace.stateModule.dom.stopBtn?.style.display).toBe('flex');

    socket.receiveRaw({ type: 'start', react_visible: false, phase: 'analyze', cycle: 1 });

    expect(workspace.stateModule.state.busy).toBe(false);
    expect(workspace.stateModule.state.ws).toBeNull();
    expect(workspace.stateModule.state.activeExecutionRunId).toBe(0);
    expect(workspace.stateModule.state.currentRoundStartedAt).toBe(0);
    expect(workspace.stateModule.dom.stopBtn?.style.display).toBe('none');
    expect(socket.readyState).toBe(WebSocket.CLOSED);
    expect(socket.onmessage).toBeNull();
    socket.receiveRaw({ type: 'done', phase: 'failed', reason: 'provider_error' });
    const sentBeforeRetry = socket.sent.length;
    inputModule.sendCmd('/status');
    expect(socket.sent).toHaveLength(sentBeforeRetry);
    expect(document.querySelectorAll('.execution-stack')).toHaveLength(0);

    await socketModule.connect(() => {});
    expect(workspace.events.filter((event) => event.startsWith('websocket:'))).toHaveLength(2);
    expect(workspace.stateModule.state.executionIdentityProtocol).toBe('strict');
    expect(workspace.stateModule.state.busy).toBe(false);
  });

  it('completes only the current running stack when a later strict start omits its identity', async () => {
    const workspace = await startWorkspace({
      persistedSessionId: 'main',
      sessions: [{ id: 'main', name: 'Main' }],
    });
    const socket = workspace.socket!;
    makeDirectComposerReady(socket);
    workspace.stateModule.state.pendingPlanExecutionId = 'plan-protocol-running';
    socket.receiveRaw({
      type: 'start',
      run_connection_id: 'strict-running-protocol-owner',
      react_visible: true,
      phase: 'analyze',
      cycle: 1,
    });
    socket.receiveRaw({
      type: 'thinking_start',
    });
    socket.receiveRaw({
      type: 'tool_call',
      id: 'protocol-tool',
      name: 'read_file',
      arguments: { path: 'spec.txt' },
    });

    const runningStack = document.querySelector<HTMLElement>('.execution-stack');
    expect(runningStack?.dataset.executionStatus).toBe('running');
    expect(runningStack?.dataset.executionPlanId).toBe('plan-protocol-running');
    const header = runningStack?.querySelector<HTMLButtonElement>('.execution-stack-header');
    header?.click();
    header?.click();
    expect(runningStack?.dataset.executionUserToggled).toBe('true');
    expect(runningStack?.classList.contains('is-expanded')).toBe(true);

    socket.receiveRaw({ type: 'start', react_visible: true, phase: 'analyze', cycle: 2 });

    expect(socket.readyState).toBe(WebSocket.CLOSED);
    expect(socket.onmessage).toBeNull();
    expect(workspace.stateModule.state.ws).toBeNull();
    expect(workspace.stateModule.state.activeExecutionStack).toBeNull();
    expect(workspace.stateModule.state.activeExecutionRunId).toBe(0);
    expect(workspace.stateModule.state.busy).toBe(false);
    expect(document.querySelectorAll('.execution-stack')).toHaveLength(1);
    expect(runningStack?.dataset.executionStatus).toBe('incomplete');
    expect(runningStack?.classList.contains('is-expanded')).toBe(true);
    expect(runningStack?.querySelector<HTMLElement>('.execution-stack-recovery')?.hidden).toBe(
      false,
    );
    expect(runningStack?.querySelector('.execution-stack-recovery-action')?.textContent).toContain(
      'Review',
    );
    const summary = runningStack?.querySelector('.execution-stack-summary')?.textContent || '';
    expect(summary).toContain('identity validation failed');
    expect(
      runningStack?.querySelector('.execution-stack-header')?.getAttribute('aria-label'),
    ).toContain(summary);

    socket.receiveRaw({
      type: 'done',
      run_connection_id: 'strict-running-protocol-owner',
      phase: 'finish',
      reason: 'complete',
    });
    expect(runningStack?.dataset.executionStatus).toBe('incomplete');

    const socketModule = await import('../src/socket.js');
    const recoveredHandler = vi.fn();
    await socketModule.connect(recoveredHandler);
    const replacement = workspace.stateModule.state.ws as unknown as TestWebSocket;
    replacement.receiveRaw({
      type: 'start',
      run_connection_id: 'strict-recovered-run',
      react_visible: true,
      phase: 'analyze',
      cycle: 1,
    });
    expect(recoveredHandler).toHaveBeenCalledWith(
      expect.objectContaining({
        type: 'start',
        run_connection_id: 'strict-recovered-run',
      }),
    );
    expect(document.querySelectorAll('.execution-stack')).toHaveLength(1);
    expect(runningStack?.dataset.executionStatus).toBe('incomplete');
  });

  it('fail-closes a real slash-command optimistic busy state on identityless start', async () => {
    const workspace = await startWorkspace({
      persistedSessionId: 'main',
      sessions: [{ id: 'main', name: 'Main' }],
    });
    const socket = workspace.socket!;
    makeDirectComposerReady(socket);
    const { sendCmd } = await import('../src/input.js');

    sendCmd('/status');
    expect(socket.sent).toEqual(['/status']);
    expect(workspace.stateModule.state.busy).toBe(true);

    socket.receiveRaw({ type: 'start', react_visible: false, phase: 'analyze', cycle: 1 });

    expect(workspace.stateModule.state.busy).toBe(false);
    expect(workspace.stateModule.state.ws).toBeNull();
    expect(workspace.stateModule.dom.stopBtn?.style.display).toBe('none');
    expect(socket.onmessage).toBeNull();
  });

  it('restores Plan action controls without creating a stack on identityless start', async () => {
    const workspace = await startWorkspace({
      persistedSessionId: 'main',
      sessions: [{ id: 'main', name: 'Main' }],
    });
    const socket = workspace.socket!;
    makeDirectComposerReady(socket);
    socket.receiveRaw({
      type: 'plan_state',
      plan: {
        plan_id: 'plan-protocol-boundary',
        revision: 1,
        status: 'ready',
        message_index: 1,
        created_at: 1,
        updated_at: 1,
        artifact: {
          title: 'Protocol boundary plan',
          goal: 'Keep the Plan recoverable',
          steps: [{ id: 'step-1', title: 'Execute safely' }],
        },
        progress: [{ id: 'step-1', title: 'Execute safely', status: 'pending' }],
      },
    });
    const { executePendingPlan } = await import('../src/renderers/pending-plan.js');
    const execute = document.querySelector<HTMLButtonElement>('[data-action="execute-plan"]');

    executePendingPlan(execute);
    expect(workspace.stateModule.state.busy).toBe(true);
    expect(workspace.stateModule.state.pendingPlanExecutionId).toBe('plan-protocol-boundary');

    socket.receiveRaw({ type: 'start', react_visible: false, phase: 'analyze', cycle: 1 });

    expect(workspace.stateModule.state.busy).toBe(false);
    expect(workspace.stateModule.state.pendingPlanExecutionId).toBe('');
    expect(workspace.stateModule.state.activePlan?.plan_id).toBe('plan-protocol-boundary');
    expect(document.querySelectorAll('.execution-stack')).toHaveLength(0);
    expect(workspace.stateModule.state.ws).toBeNull();
  });

  it('restores a listed Session before preserving it as the Group return Session', async () => {
    const { events, socketUrl, stateModule } = await startWorkspace({
      persistedSessionId: 'research-notes',
      sessions: [
        { id: 'main', name: 'Main' },
        { id: 'research-notes', name: 'Research notes' },
      ],
      persistedGroupId: 'persisted-group',
      groupsEnabled: true,
    });

    expect(stateModule.state.activeSessionId).toBe('main');
    expect(stateModule.state.groupReturnSessionId).toBe('research-notes');
    expect(localStorage.getItem(ACTIVE_SESSION_STORAGE_KEY)).toBe('research-notes');
    expect(socketUrl.searchParams.get('group')).toBe('persisted-group');
    expect(socketUrl.searchParams.get('session')).toBe('main');
    expectSessionRestoreBeforeGroupAndSocket(events);
    expectNoSessionCreate(events);
  });

  it('restores a Windows case alias as the canonical Group return Session', async () => {
    const { events, socketUrl, stateModule } = await startWorkspace({
      persistedSessionId: 'research-notes',
      sessions: [
        { id: 'main', name: 'Main' },
        { id: 'Research-Notes', name: 'Research notes' },
      ],
      sessionIdsCaseSensitive: false,
      persistedGroupId: 'persisted-group',
      groupsEnabled: true,
    });

    expect(stateModule.state.activeSessionId).toBe('main');
    expect(stateModule.state.groupReturnSessionId).toBe('Research-Notes');
    expect(localStorage.getItem(ACTIVE_SESSION_STORAGE_KEY)).toBe('Research-Notes');
    expect(socketUrl.searchParams.get('group')).toBe('persisted-group');
    expect(socketUrl.searchParams.get('session')).toBe('main');
    expectSessionRestoreBeforeGroupAndSocket(events);
    expectNoSessionCreate(events);
  });

  it('connects a Windows case alias using the canonical server Session id', async () => {
    const { events, socketUrl, stateModule } = await startWorkspace({
      persistedSessionId: 'research-notes',
      sessions: [
        { id: 'main', name: 'Main' },
        { id: 'Research-Notes', name: 'Research notes' },
      ],
      sessionIdsCaseSensitive: false,
    });

    expect(stateModule.state.activeSessionId).toBe('Research-Notes');
    expect(localStorage.getItem(ACTIVE_SESSION_STORAGE_KEY)).toBe('Research-Notes');
    expect(socketUrl.searchParams.get('group')).toBeNull();
    expect(socketUrl.searchParams.get('session')).toBe('Research-Notes');
    expectSessionRestoreBeforeGroupAndSocket(events);
    expectNoSessionCreate(events);
  });

  it('does not merge case-distinct Session ids under Linux semantics', async () => {
    const { events, socketUrl, stateModule } = await startWorkspace({
      persistedSessionId: 'research-notes',
      sessions: [
        { id: 'main', name: 'Main' },
        { id: 'Research-Notes', name: 'Research notes' },
      ],
      sessionIdsCaseSensitive: true,
    });

    expect(stateModule.state.activeSessionId).toBe('main');
    expect(localStorage.getItem(ACTIVE_SESSION_STORAGE_KEY)).toBe('main');
    expect(socketUrl.searchParams.get('group')).toBeNull();
    expect(socketUrl.searchParams.get('session')).toBe('main');
    expectSessionRestoreBeforeGroupAndSocket(events);
    expectNoSessionCreate(events);
  });

  it('rejects a ghost before Group restoration and WebSocket connection', async () => {
    const { events, socketUrl, stateModule } = await startWorkspace({
      persistedSessionId: 'ghost-session',
      sessions: [{ id: 'main', name: 'Main' }],
      persistedGroupId: 'persisted-group',
      groupsEnabled: true,
    });

    expect(stateModule.state.activeSessionId).toBe('main');
    expect(stateModule.state.groupReturnSessionId).toBe('');
    expect(localStorage.getItem(ACTIVE_SESSION_STORAGE_KEY)).toBe('main');
    expect(socketUrl.searchParams.get('group')).toBe('persisted-group');
    expect(socketUrl.searchParams.get('session')).toBe('main');
    expectSessionRestoreBeforeGroupAndSocket(events);
    expectNoSessionCreate(events);
  });

  it('falls back to and persists main when the Session list request fails', async () => {
    const { events, socketUrl, stateModule } = await startWorkspace({
      persistedSessionId: 'research-notes',
      sessions: [],
      sessionListStatus: 503,
    });

    expect(stateModule.state.activeSessionId).toBe('main');
    expect(localStorage.getItem(ACTIVE_SESSION_STORAGE_KEY)).toBe('main');
    expect(socketUrl.searchParams.get('group')).toBeNull();
    expect(socketUrl.searchParams.get('session')).toBe('main');
    expectSessionRestoreBeforeGroupAndSocket(events);
    expectNoSessionCreate(events);
  });

  it('makes zero Group discovery requests when Groups are disabled', async () => {
    const { events, socketUrl, stateModule } = await startWorkspace({
      persistedSessionId: 'main',
      sessions: [{ id: 'main', name: 'Main' }],
      persistedGroupId: 'old-disabled-group',
      groupsEnabled: false,
    });

    expect(stateModule.state.groupsEnabled).toBe(false);
    expect(stateModule.state.activeGroupId).toBe('');
    expect(socketUrl.searchParams.get('group')).toBeNull();
    expect(socketUrl.searchParams.get('session')).toBe('main');
    expectNoGroupDiscovery(events);
    expectNoSessionCreate(events);
  });
});
