import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest';

const html = readFileSync(resolve(process.cwd(), 'index.html'), 'utf8');

class TransportSocket {
  static readonly OPEN = 1;
  static readonly CLOSED = 3;
  static instances: TransportSocket[] = [];
  readyState = 0;
  onopen: (() => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: ((event: MessageEvent<string>) => void) | null = null;
  sent: string[] = [];
  constructor(readonly url: string) {
    TransportSocket.instances.push(this);
  }
  open() {
    this.readyState = 1;
    this.onopen?.();
  }
  close() {
    this.readyState = 3;
    this.onclose?.();
  }
  send(value: string) {
    this.sent.push(value);
  }
  receive(value: unknown) {
    this.onmessage?.(new MessageEvent('message', { data: JSON.stringify(value) }));
  }
}

function response(value: unknown): Response {
  return new Response(JSON.stringify(value), { headers: { 'Content-Type': 'application/json' } });
}

function session(id = 'main', configured = true) {
  return {
    type: 'session',
    id,
    name: id,
    model: 'mock/model',
    effort: 'off',
    effectiveModelConfigured: configured,
    explicitPrimaryModelConfigured: configured,
    modelOverridePresent: false,
    modelOverrideConfigured: false,
    capabilities: { image: true, s3: false, s3_config_id: null },
    configRevision: 1,
    usage: {},
  };
}

describe('Composer transport and real socket handlers', () => {
  let app: typeof import('../src/state.js');
  let socketModule: typeof import('../src/socket.js');
  let inputModule: typeof import('../src/input.js');
  let unavailable = false;
  let featureGroups = false;
  let protocol = 1;
  let clientConfigPending: Promise<Response> | null = null;
  let composer: typeof import('../src/composerAvailability.js');
  let mainMessageHandler: Parameters<typeof socketModule.connect>[0];

  function enter(value: string): void {
    app.dom.input!.value = value;
    app.dom.input!.dispatchEvent(new Event('input'));
    app.dom.input!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
  }

  async function beginReconnect(): Promise<TransportSocket> {
    const count = TransportSocket.instances.length;
    socketModule.failCloseCurrentExecutionProtocol();
    unavailable = false;
    protocol = 1;
    app.dom.composerAvailabilityAction!.click();
    await vi.waitFor(() => expect(TransportSocket.instances).toHaveLength(count + 1));
    return TransportSocket.instances.at(-1)!;
  }

  async function readySocket(): Promise<TransportSocket> {
    featureGroups = false;
    app.state.activeGroupId = '';
    app.state.activeSessionId = 'main';
    app.state.pendingImages = [];
    app.state.imageUploadInFlight = false;
    app.state.sessionIdentityMutationInFlight = false;
    app.state.composerModelSwitchInFlight = false;
    app.state.storageMode = 'healthy';
    const socket = await beginReconnect();
    socket.open();
    socket.receive(session());
    socket.receive({ type: 'history', messages: [] });
    await vi.waitFor(() => expect(app.dom.sendBtn?.disabled).toBe(false));
    return socket;
  }

  beforeAll(async () => {
    document.body.innerHTML = html.match(/<body[^>]*>([\s\S]*?)<\/body>/i)![1];
    localStorage.clear();
    HTMLElement.prototype.scrollIntoView = vi.fn();
    vi.stubGlobal('WebSocket', TransportSocket);
    vi.stubGlobal(
      'fetch',
      vi.fn<typeof fetch>((input) => {
        const url =
          typeof input === 'string' ? input : input instanceof URL ? input.toString() : input.url;
        if (url === '/api/client-config') {
          if (clientConfigPending) return clientConfigPending;
          if (unavailable) return Promise.reject(new Error('controlled daemon outage'));
          return Promise.resolve(
            response({
              features: { groups: featureGroups },
              protocols: { execution_identity: protocol },
            }),
          );
        }
        if (url === '/api/config')
          return Promise.resolve(
            response({
              config: {},
              configuredModelsAvailable: true,
              explicitPrimaryModelConfigured: true,
              configRevision: 1,
            }),
          );
        if (url === '/api/sessions')
          return Promise.resolve(response({ sessions: [{ id: 'main', name: 'Main' }] }));
        if (url === '/api/session-groups') return Promise.resolve(response({ groups: [] }));
        if (url === '/api/health') return Promise.resolve(response({ version: 'test' }));
        if (url.startsWith('/api/session-models'))
          return Promise.resolve(
            response({
              session: session(),
              models: [],
              explicitPrimaryModelConfigured: true,
              configRevision: 1,
            }),
          );
        throw new Error(`Unexpected fetch: ${url}`);
      }),
    );
    await import('../src/main.js');
    app = await import('../src/state.js');
    socketModule = await import('../src/socket.js');
    const reconnect = socketModule.reconnectToActiveSession;
    vi.spyOn(socketModule, 'reconnectToActiveSession').mockImplementation((onMessage) => {
      mainMessageHandler = onMessage;
      return reconnect(onMessage);
    });
    inputModule = await import('../src/input.js');
    composer = await import('../src/composerAvailability.js');
    await vi.waitFor(() => expect(TransportSocket.instances).toHaveLength(1));
    const socket = TransportSocket.instances[0];
    socket.open();
    socket.receive(session());
    socket.receive({ type: 'history', messages: [] });
    await vi.waitFor(() => expect(app.dom.sendBtn?.disabled).toBe(false));
  });

  afterAll(() => {
    socketModule?.cancelReconnect();
    if (app?.state.ws) {
      app.state.ws.onclose = null;
      app.state.ws.close();
    }
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
    localStorage.clear();
    document.body.replaceChildren();
  });

  it('blocks mouse and Enter after a real close and failed renegotiation while retaining the draft', async () => {
    const socket = TransportSocket.instances.at(-1)!;
    const images = await import('../src/images.js');
    images.addImageUrl('https://example.test/synthetic.png');
    const attachments = app.state.pendingImages.slice();
    expect(attachments).toHaveLength(1);
    app.dom.input!.value = 'Keep this offline draft';
    app.dom.input!.dispatchEvent(new Event('input'));
    unavailable = true;
    socket.close();
    await vi.waitFor(() => expect(app.state.ws).toBeNull(), { timeout: 2500 });

    expect(app.dom.sendBtn?.disabled).toBe(true);
    expect(app.dom.sendBtn?.getAttribute('aria-describedby')).toContain(
      'composer-availability-detail',
    );
    const before = app.dom.chat!.innerHTML;
    app.dom.sendBtn?.click();
    app.dom.input!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    expect(socket.sent).toEqual([]);
    expect(app.dom.input!.value).toBe('Keep this offline draft');
    expect(app.state.pendingImages).toEqual(attachments);
    expect(app.dom.chat!.innerHTML).toBe(before);
  });

  it('waits for OPEN and full Session preparation, then sends the recovered draft and attachments exactly once', async () => {
    unavailable = false;
    const count = TransportSocket.instances.length;
    const attachments = app.state.pendingImages.slice();
    app.dom.composerAvailabilityAction!.focus();
    app.dom.composerAvailabilityAction!.click();
    await vi.waitFor(() => expect(TransportSocket.instances).toHaveLength(count + 1));
    const socket = TransportSocket.instances.at(-1)!;
    expect(app.dom.sendBtn?.disabled).toBe(true);
    expect(socket.sent).toEqual([]);
    socket.open();
    socket.receive({ ...session(), type: 'session_model_configuration' });
    await Promise.resolve();
    expect(app.state.composerModelAvailability).toBe('ready');
    expect(app.dom.sendBtn?.disabled).toBe(true);
    expect(app.dom.input?.placeholder).toContain('Waiting for this session');
    expect(app.dom.input?.value).toBe('Keep this offline draft');
    expect(app.state.pendingImages).toEqual(attachments);
    socket.receive(session());
    expect(app.dom.sendBtn?.disabled).toBe(true);
    socket.receive({ type: 'history', messages: [] });
    await vi.waitFor(() => expect(app.dom.sendBtn?.disabled).toBe(false));
    expect(document.activeElement).toBe(app.dom.input);
    expect(app.dom.sendBtn?.hasAttribute('aria-describedby')).toBe(false);
    expect(app.dom.sendBtn?.getAttribute('aria-disabled')).toBe('false');
    expect(app.dom.sendBtn?.title).toBe('');
    expect(socket.sent).toEqual([]);
    enter('Keep this offline draft');
    expect(socket.sent).toHaveLength(1);
    expect(JSON.parse(socket.sent[0])).toMatchObject({
      text: 'Keep this offline draft',
      images: attachments,
    });
    expect(app.dom.chat?.querySelectorAll('.msg-row.user')).toHaveLength(1);
    expect(app.state.pendingImages).toEqual([]);
    expect(app.dom.input?.value).toBe('');
  });

  it('rejects all saved callbacks from a superseded socket during negotiation and after recovery', async () => {
    const old = await readySocket();
    const callbacks = {
      open: old.onopen!,
      close: old.onclose!,
      error: old.onerror!,
      message: old.onmessage!,
    };
    let resolveConfig!: (value: Response) => void;
    clientConfigPending = new Promise<Response>((resolve) => {
      resolveConfig = resolve;
    });
    socketModule.failCloseCurrentExecutionProtocol();
    app.dom.composerAvailabilityAction!.click();
    callbacks.open();
    callbacks.close();
    callbacks.error();
    callbacks.message(
      new MessageEvent('message', { data: JSON.stringify(session('stale-session')) }),
    );
    expect(app.dom.sendBtn?.disabled).toBe(true);
    expect(app.state.activeSessionId).toBe('main');
    expect(old.sent).toEqual([]);
    clientConfigPending = null;
    resolveConfig(response({ features: { groups: false }, protocols: { execution_identity: 1 } }));
    await vi.waitFor(() => expect(TransportSocket.instances.at(-1)).not.toBe(old));
    const current = TransportSocket.instances.at(-1)!;
    current.open();
    current.receive(session());
    current.receive({ type: 'history', messages: [] });
    await vi.waitFor(() => expect(app.dom.sendBtn?.disabled).toBe(false));
    callbacks.open();
    callbacks.close();
    callbacks.error();
    callbacks.message(
      new MessageEvent('message', { data: JSON.stringify(session('stale-session', false)) }),
    );
    expect(app.state.ws).toBe(current);
    expect(app.state.activeSessionId).toBe('main');
    expect(app.dom.sendBtn?.disabled).toBe(false);
    expect(app.dom.composerAvailabilityStatus?.hidden).toBe(true);
    expect(current.sent).toEqual([]);
  });

  it.each([0, 2])(
    'keeps cached models and model-free commands blocked when protocol %s is refused',
    async (unsupported) => {
      const socket = await readySocket();
      socketModule.failCloseCurrentExecutionProtocol();
      protocol = unsupported;
      const count = TransportSocket.instances.length;
      app.dom.composerAvailabilityAction!.click();
      await vi.waitFor(() => expect(app.dom.composerAvailabilityAction?.hidden).toBe(false));
      expect(app.state.ws).toBeNull();
      expect(TransportSocket.instances).toHaveLength(count);
      enter('/status');
      expect(app.dom.sendBtn?.disabled).toBe(true);
      expect(app.dom.input?.value).toBe('/status');
      expect(socket.sent).toEqual([]);
      protocol = 1;
    },
  );

  it('bounds a pending negotiation without allowing HTTP model results to unlock Send', async () => {
    const socket = await readySocket();
    socketModule.failCloseCurrentExecutionProtocol();
    vi.useFakeTimers();
    try {
      clientConfigPending = new Promise<Response>(() => {});
      app.dom.composerAvailabilityAction!.click();
      composer.applyComposerConfig({}, true, 1);
      composer.setComposerSessionModelConfigured(false, false, true, 1);
      enter('/help');
      expect(app.dom.sendBtn?.disabled).toBe(true);
      expect(app.dom.input?.placeholder).toContain('Connecting');
      await vi.advanceTimersByTimeAsync(10_000);
      expect(app.state.ws).toBeNull();
      expect(app.dom.sendBtn?.disabled).toBe(true);
      expect(app.dom.composerAvailabilityAction?.hidden).toBe(false);
      expect(app.dom.input?.value).toBe('/help');
      expect(socket.sent).toEqual([]);
    } finally {
      clientConfigPending = null;
      vi.useRealTimers();
    }
  });

  it('keeps model policy separate from transport, including protected read-only slash commands', async () => {
    const socket = await readySocket();
    socket.receive({
      ...session('main', false),
      type: 'session_model_configuration',
      configRevision: 2,
    });
    enter('requires a model');
    expect(app.dom.sendBtn?.disabled).toBe(true);
    expect(socket.sent).toEqual([]);
    enter('/status');
    expect(socket.sent).toEqual(['/status']);
    socket.receive({
      type: 'storage_status',
      storage: { mode: 'protected', code: 'storage_protected' },
    });
    enter('/clear');
    expect(socket.sent).toHaveLength(1);
    enter('/usage');
    expect(socket.sent).toEqual(['/status', '/usage']);
    socketModule.failCloseCurrentExecutionProtocol();
    enter('/help');
    expect(app.dom.sendBtn?.disabled).toBe(true);
    expect(socket.sent).toHaveLength(2);
  });

  it('keeps Plan, Stop and busy intervention on the same current transport', async () => {
    const socket = await readySocket();
    enter('start work');
    enter('follow-up intervention');
    inputModule.stopAgent();
    expect(socket.sent).toHaveLength(3);
    expect(socket.sent[2]).toBe('/stop');
    socketModule.failCloseCurrentExecutionProtocol();
    const plan = await import('../src/renderers/pending-plan.js');
    plan.renderPendingPlanAction({ plan_id: 'offline-plan', message_index: 2, created_at: 1 });
    const execute = document.querySelector<HTMLButtonElement>('[data-action="execute-plan"]');
    expect(execute?.disabled).toBe(true);
    expect(execute?.title).toContain('connection');
    plan.executePendingPlan(execute);
    inputModule.stopAgent();
    expect(socket.sent).toHaveLength(3);
    expect(app.dom.stopBtn?.disabled).toBe(true);
  });

  it('retains drafts on synchronous send failure and never retries automatically', async () => {
    const socket = await readySocket();
    const before = app.dom.chat!.innerHTML;
    vi.spyOn(socket, 'send').mockImplementationOnce(() => {
      throw new Error('closed during send');
    });
    enter('keep after send failure');
    expect(app.dom.input?.value).toBe('keep after send failure');
    expect(app.dom.chat!.innerHTML).toBe(before);
    expect(socket.sent).toEqual([]);
    expect(app.dom.sendBtn?.disabled).toBe(true);
    expect(app.dom.composerAvailabilityAction?.hidden).toBe(false);
  });

  it('disables all later submissions after a real strict identityless start fails closed', async () => {
    const socket = await readySocket();
    enter('accepted by the current socket');
    socket.receive({ type: 'start', cycle: 1 });
    expect(app.state.ws).toBeNull();
    expect(app.state.busy).toBe(false);
    expect(app.dom.sendBtn?.disabled).toBe(true);
    expect(app.dom.composerAvailabilityAction?.hidden).toBe(false);
    enter('/status');
    expect(socket.sent).toHaveLength(1);
    expect(app.dom.input?.value).toBe('/status');
  });

  it('clears attachments for an ordinary history reset and an actual target transition', async () => {
    const socket = await readySocket();
    const images = await import('../src/images.js');
    images.addImageUrl('https://example.test/source.png');
    socket.receive({ type: 'history', messages: [] });
    expect(app.state.pendingImages).toEqual([]);
    images.addImageUrl('https://example.test/source.png');
    expect(app.state.pendingImages).toHaveLength(1);
    composer.beginComposerSessionTransition(true, 'different-session');
    socket.receive(session('different-session'));
    expect(app.dom.sendBtn?.disabled).toBe(true);
    socket.receive({ type: 'history', messages: [] });
    expect(app.state.activeSessionId).toBe('different-session');
    expect(app.state.pendingImages).toEqual([]);
    expect(socket.sent).toEqual([]);
  });

  it('preserves upload and identity locks after transport preparation, while model-free upload commands still work', async () => {
    const socket = await readySocket();
    app.state.imageUploadInFlight = true;
    enter('wait for the image');
    expect(socket.sent).toEqual([]);
    expect(app.dom.sendBtn?.disabled).toBe(true);
    enter('/status');
    expect(socket.sent).toEqual(['/status']);
    app.state.imageUploadInFlight = false;
    app.state.sessionIdentityMutationInFlight = true;
    enter('/help');
    expect(socket.sent).toHaveLength(1);
    expect(app.dom.sendBtn?.disabled).toBe(true);
    app.state.sessionIdentityMutationInFlight = false;
  });

  it('confirms in-band Session switching before allowing a new explicit send', async () => {
    const socket = await readySocket();
    enter('/switch second-session');
    expect(socket.sent).toEqual(['/switch second-session']);
    expect(app.dom.sendBtn?.disabled).toBe(true);
    enter('draft for the confirmed session');
    expect(socket.sent).toHaveLength(1);
    socket.receive(session('second-session'));
    expect(app.state.activeSessionId).toBe('second-session');
    expect(app.dom.sendBtn?.disabled).toBe(true);
    socket.receive({ type: 'history', messages: [] });
    expect(app.dom.sendBtn?.disabled).toBe(false);
    expect(socket.sent).toHaveLength(1);
    enter('draft for the confirmed session');
    expect(socket.sent).toHaveLength(2);
  });

  it('blocks a prepared Group with no targets and keeps its offline commands disabled', async () => {
    const socket = await readySocket();
    app.state.groupsEnabled = true;
    socket.receive({
      type: 'group',
      id: 'test-group',
      name: 'Group',
      members: [],
      member_details: [],
      model_configured_members: [],
      model_member_ids: [],
      configRevision: 1,
    });
    socket.receive({ type: 'group_history', messages: [], runs: [] });
    enter('needs a member');
    expect(socket.sent).toEqual([]);
    expect(app.dom.sendBtn?.disabled).toBe(true);
    expect(app.dom.input?.placeholder).toContain('member');
    socketModule.failCloseCurrentExecutionProtocol();
    enter('/status');
    expect(app.dom.sendBtn?.disabled).toBe(true);
    expect(socket.sent).toEqual([]);
  });

  function groupPayload(id = 'retry-group') {
    return {
      type: 'group',
      id,
      name: id,
      members: ['worker-a'],
      member_details: [],
      model_configured_members: ['worker-a'],
      model_member_ids: ['worker-a'],
      explicitPrimaryModelConfigured: true,
      configRevision: 1,
    };
  }

  async function readyGroup(): Promise<TransportSocket> {
    await readySocket();
    app.dom.input!.value = '';
    featureGroups = true;
    app.state.activeGroupId = 'retry-group';
    const socket = await beginReconnect();
    socket.open();
    socket.receive(groupPayload());
    socket.receive({ type: 'group_history', messages: [], runs: [] });
    expect(app.dom.sendBtn?.disabled).toBe(false);
    return socket;
  }

  async function exhaustRetries(socket: TransportSocket): Promise<TransportSocket> {
    const { MAX_RECONNECT_ATTEMPTS } = await import('../src/constants.js');
    expect(MAX_RECONNECT_ATTEMPTS).toBe(50);
    expect(app.state.reconnectAttempts).toBe(0);
    const initialCount = TransportSocket.instances.length;
    for (let attempt = 1; attempt <= MAX_RECONNECT_ATTEMPTS; attempt++) {
      const delay = app.state.reconnectDelay;
      socket.close();
      await vi.advanceTimersByTimeAsync(0);
      expect(app.state.reconnectAttempts).toBe(attempt);
      await vi.advanceTimersByTimeAsync(delay);
      expect(TransportSocket.instances).toHaveLength(initialCount + attempt);
      const next = TransportSocket.instances.at(-1)!;
      expect(next).not.toBe(socket);
      expect(next.readyState).toBe(0);
      expect(next.sent).toEqual([]);
      socket = next;
    }
    socket.close();
    await vi.advanceTimersByTimeAsync(0);
    expect(app.state.reconnectAttempts).toBe(MAX_RECONNECT_ATTEMPTS);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(TransportSocket.instances).toHaveLength(initialCount + MAX_RECONNECT_ATTEMPTS);
    return socket;
  }

  it.each(['Group', 'Session'] as const)(
    'exhausts all 50 real retry timers for %s and restores explicit same-page submission',
    async (target) => {
      let socket = target === 'Group' ? await readyGroup() : await readySocket();
      const transport = await import('../src/composerTransport.js');
      const images = await import('../src/images.js');
      // Group images remain unsupported; the Session case owns the valid image draft.
      if (target === 'Session') images.addImageUrl('https://example.test/exhaustion.png');
      const attachments = app.state.pendingImages.slice();
      const draft = `Unsent ${target} draft after retry exhaustion`;
      app.dom.input!.value = draft;
      app.dom.input!.dispatchEvent(new Event('input'));
      vi.useFakeTimers();
      try {
        socket = await exhaustRetries(socket);
        expect(transport.composerTransportAvailability()).toBe('offline');
        expect(app.dom.input?.dataset.availability).toBe('offline');
        expect(app.dom.connLabel?.textContent).toBe('Offline');
        expect(app.dom.connDot?.classList.contains('disconnected')).toBe(true);
        expect(document.getElementById('composer-availability-message')?.textContent).toBe(
          'Connection unavailable',
        );
        expect(app.dom.composerAvailabilityAction?.hidden).toBe(false);
        expect(app.dom.sendBtn?.disabled).toBe(true);
        expect(app.dom.sendBtn?.getAttribute('aria-disabled')).toBe('true');
        expect(app.dom.sendBtn?.getAttribute('aria-describedby')).toContain(
          'composer-availability-detail',
        );
        expect(document.getElementById('composer-availability-detail')?.textContent).toBe(
          app.dom.input?.placeholder,
        );
        expect(app.dom.sendBtn?.title).toBe(app.dom.input?.placeholder);
        const offlineChat = app.dom.chat!.innerHTML;
        app.dom.sendBtn!.click();
        enter(draft);
        expect(socket.sent).toEqual([]);
        expect(app.dom.chat!.innerHTML).toBe(offlineChat);
        expect(app.dom.input?.value).toBe(draft);
        expect(app.state.pendingImages).toEqual(attachments);
        let finishNegotiation!: (value: Response) => void;
        clientConfigPending = new Promise<Response>((resolve) => {
          finishNegotiation = resolve;
        });
        const count = TransportSocket.instances.length;
        app.dom.composerAvailabilityAction!.click();
        expect(app.state.reconnectAttempts).toBe(0);
        expect(transport.composerTransportAvailability()).toBe('connecting');
        expect(app.dom.sendBtn?.disabled).toBe(true);
        expect(TransportSocket.instances).toHaveLength(count);
        expect(app.dom.input?.value).toBe(draft);
        clientConfigPending = null;
        finishNegotiation(
          response({ features: { groups: featureGroups }, protocols: { execution_identity: 1 } }),
        );
        await vi.advanceTimersByTimeAsync(0);
        expect(TransportSocket.instances).toHaveLength(count + 1);
        const recovered = TransportSocket.instances.at(-1)!;
        expect(app.dom.sendBtn?.disabled).toBe(true);
        recovered.open();
        expect(app.dom.sendBtn?.disabled).toBe(true);
        recovered.receive(target === 'Group' ? groupPayload() : session());
        expect(app.dom.sendBtn?.disabled).toBe(true);
        recovered.receive({
          type: target === 'Group' ? 'group_history' : 'history',
          messages: [],
          runs: [],
        });
        expect(transport.composerTransportAvailability()).toBe('ready');
        expect(app.dom.sendBtn?.disabled).toBe(false);
        expect(app.dom.sendBtn?.hasAttribute('aria-describedby')).toBe(false);
        expect(app.dom.sendBtn?.title).toBe('');
        expect(app.state.pendingImages).toEqual(attachments);
        expect(recovered.sent).toEqual([]);
        expect(app.dom.chat!.querySelectorAll('.msg-row.user')).toHaveLength(0);
        enter(draft);
        expect(recovered.sent).toHaveLength(1);
        expect(JSON.parse(recovered.sent[0])).toMatchObject({ text: draft });
        if (target === 'Session') {
          expect(JSON.parse(recovered.sent[0])).toMatchObject({ images: attachments });
          expect(app.dom.chat!.querySelectorAll('.msg-row.user')).toHaveLength(1);
        } else {
          expect(JSON.parse(recovered.sent[0])).toMatchObject({
            type: 'group_message',
            targets: [],
            target_mode: 'all',
          });
        }
        expect(app.dom.input?.value).toBe('');
      } finally {
        clientConfigPending = null;
        featureGroups = false;
        socketModule.cancelReconnect();
        vi.useRealTimers();
      }
    },
  );

  it.each(['Session', 'Group'] as const)(
    'ignores late Group probes and old socket callbacks after recovery to a new %s',
    async (target) => {
      const old = await readyGroup();
      const callbacks = {
        open: old.onopen!,
        close: old.onclose!,
        error: old.onerror!,
        message: old.onmessage!,
      };
      let finishProbe!: (value: Response) => void;
      clientConfigPending = new Promise<Response>((resolve) => {
        finishProbe = resolve;
      });
      old.close();
      clientConfigPending = null;
      app.state.activeGroupId = target === 'Group' ? 'new-group' : '';
      // Direct connect deliberately retains the old socket while its new preflight is pending.
      let finishNegotiation!: (value: Response) => void;
      clientConfigPending = new Promise<Response>((resolve) => {
        finishNegotiation = resolve;
      });
      const connecting = socketModule.connect(mainMessageHandler);
      expect(app.state.ws).toBe(old);
      const transport = await import('../src/composerTransport.js');
      vi.useFakeTimers();
      try {
        finishProbe(response({ features: { groups: true }, protocols: { execution_identity: 1 } }));
        await vi.advanceTimersByTimeAsync(0);
        callbacks.open();
        callbacks.close();
        callbacks.error();
        expect(transport.composerTransportAvailability()).toBe('connecting');
        expect(app.dom.sendBtn?.disabled).toBe(true);
        clientConfigPending = null;
        finishNegotiation(
          response({ features: { groups: true }, protocols: { execution_identity: 1 } }),
        );
        await connecting;
        const recovered = TransportSocket.instances.at(-1)!;
        recovered.open();
        recovered.receive(target === 'Group' ? groupPayload('new-group') : session());
        recovered.receive({
          type: target === 'Group' ? 'group_history' : 'history',
          messages: [],
          runs: [],
        });
        const count = TransportSocket.instances.length;
        callbacks.open();
        callbacks.close();
        callbacks.error();
        callbacks.message(new MessageEvent('message', { data: JSON.stringify(groupPayload()) }));
        await vi.advanceTimersByTimeAsync(60_000);
        expect(app.state.ws).toBe(recovered);
        expect(app.state.activeGroupId).toBe(target === 'Group' ? 'new-group' : '');
        expect(TransportSocket.instances).toHaveLength(count);
        expect(transport.composerTransportAvailability()).toBe('ready');
        expect(app.dom.sendBtn?.disabled).toBe(false);
        expect(recovered.sent).toEqual([]);
      } finally {
        clientConfigPending = null;
        featureGroups = false;
        socketModule.cancelReconnect();
        vi.useRealTimers();
      }
    },
  );

  it.each(['model', 'storage'] as const)(
    'still removes an invalid image draft after reconnect when %s capability changes',
    async (changed) => {
      await readySocket();
      const images = await import('../src/images.js');
      if (changed === 'model') images.addImageUrl('https://example.test/invalidated.png');
      else {
        images.updateS3ConfigIdentity('old-storage');
        app.state.pendingImages = [
          {
            url: 'https://example.test/upload.png',
            object_key: 'uploads/image.png',
            attachment_token: 'synthetic',
            s3_config_id: 'old-storage',
          },
        ];
        images.renderImagePreviews();
      }
      app.dom.input!.value = 'Keep text after image capability changes';
      expect(app.state.pendingImages).toHaveLength(1);
      const recovered = await beginReconnect();
      recovered.open();
      recovered.receive({
        ...session(),
        capabilities: {
          image: changed !== 'model',
          s3: false,
          s3_config_id: changed === 'storage' ? 'new-storage' : null,
        },
      });
      recovered.receive({ type: 'history', messages: [] });
      expect(app.state.pendingImages).toEqual([]);
      expect(app.dom.input?.value).toBe('Keep text after image capability changes');
      expect(recovered.sent).toEqual([]);
      expect(app.dom.sendBtn?.disabled).toBe(false);
    },
  );
});
