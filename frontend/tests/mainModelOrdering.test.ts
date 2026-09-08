import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { beforeAll, describe, expect, it, vi } from 'vitest';

const indexHtml = readFileSync(resolve(process.cwd(), 'index.html'), 'utf8');

class FakeWebSocket {
  static readonly OPEN = 1;
  static readonly CLOSED = 3;
  static instances: FakeWebSocket[] = [];

  readonly url: string;
  readyState = FakeWebSocket.OPEN;
  onopen: (() => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: ((event: MessageEvent<string>) => void) | null = null;
  sent: unknown[] = [];
  private readonly instanceId: number;
  private runIdentitySequence = 0;
  private currentRunIdentity = '';

  constructor(url: string) {
    this.url = url;
    this.instanceId = FakeWebSocket.instances.length + 1;
    FakeWebSocket.instances.push(this);
  }

  send(payload: unknown): void {
    this.sent.push(payload);
  }

  close(): void {
    this.readyState = WebSocket.CLOSED;
  }

  private dispatch(payload: unknown): void {
    this.onmessage?.(
      new MessageEvent('message', {
        data: JSON.stringify(payload),
      }),
    );
  }

  receiveRaw(payload: unknown): void {
    this.dispatch(payload);
  }

  receive(payload: unknown): void {
    let eventPayload = payload;
    if (payload && typeof payload === 'object' && !Array.isArray(payload)) {
      const event = { ...(payload as Record<string, unknown>) };
      const eventType = typeof event.type === 'string' ? event.type : '';
      if (eventType === 'start' && !event.subagent) {
        if (!Object.hasOwn(event, 'run_connection_id')) {
          event.run_connection_id = `test-run-${this.instanceId}-${++this.runIdentitySequence}`;
        }
        this.currentRunIdentity = String(event.run_connection_id || '');
      } else if (
        (eventType === 'done' || (eventType === 'error' && event.run_terminal === true)) &&
        !Object.hasOwn(event, 'run_connection_id') &&
        this.currentRunIdentity
      ) {
        event.run_connection_id = this.currentRunIdentity;
      }
      eventPayload = event;
    }
    this.dispatch(eventPayload);
    if (
      eventPayload &&
      typeof eventPayload === 'object' &&
      !Array.isArray(eventPayload) &&
      (eventPayload as Record<string, unknown>).type === 'done'
    ) {
      this.currentRunIdentity = '';
    }
  }
}

function jsonResponse(payload: unknown, status = 200): Response {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

function terminalPlanPayload(
  planId: string,
  status: 'failed' | 'stopped',
  stepStatus: 'blocked' | 'in_progress',
) {
  return {
    plan_id: planId,
    revision: 2,
    status,
    message_index: 2,
    created_at: 1710000000,
    updated_at: 1710000002,
    approved_at: 1710000001,
    finished_at: 1710000002,
    execution_attempt: 1,
    artifact: {
      title: 'Verified execution plan',
      goal: 'Produce the approved result',
      steps: [{ id: 'implement', title: 'Implement the approved result' }],
    },
    progress: [{ id: 'implement', title: 'Implement the approved result', status: stepStatus }],
  };
}

describe('main model payload ordering', () => {
  let socket: FakeWebSocket;
  let stateModule: typeof import('../src/state.js');
  let composerModule: typeof import('../src/composerAvailability.js');
  let groupDetail: Record<string, unknown> | null = null;
  let groupDetailFetchCount = 0;
  let configResponseRevision = 100;
  let deferredGroupDetailResponse: Promise<Response> | null = null;
  let deferredSessionCreateResponse: Promise<Response> | null = null;
  let uploadTokenFetchCount = 0;
  let clientS3ConfigId = 's3-a';

  beforeAll(async () => {
    const body = indexHtml.match(/<body[^>]*>([\s\S]*?)<\/body>/i)?.[1];
    if (!body) throw new Error('index.html body not found');
    document.body.innerHTML = body;

    HTMLElement.prototype.scrollIntoView = vi.fn();

    vi.stubGlobal('WebSocket', FakeWebSocket);
    vi.stubGlobal(
      'fetch',
      vi.fn<typeof fetch>((input, init) => {
        const url = typeof input === 'string' ? input : input.url;
        if (url === '/api/config') {
          return Promise.resolve(
            jsonResponse({
              config: {},
              configuredModelsAvailable: false,
              explicitPrimaryModelConfigured: false,
              configRevision: configResponseRevision,
            }),
          );
        }
        if (url === '/api/sessions') return Promise.resolve(jsonResponse({ sessions: [] }));
        if (url === '/api/session-groups') {
          return Promise.resolve(jsonResponse({ groups: [] }));
        }
        if (url === '/api/session' && init?.method === 'POST') {
          if (!deferredSessionCreateResponse) {
            throw new Error('No deferred Session create response configured.');
          }
          const response = deferredSessionCreateResponse;
          deferredSessionCreateResponse = null;
          return response;
        }
        if (url === '/api/client-config') {
          uploadTokenFetchCount += 1;
          return Promise.resolve(
            jsonResponse({
              upload_token: 'upload-token',
              s3_config_id: clientS3ConfigId,
              features: { groups: true },
              protocols: { execution_identity: 1 },
            }),
          );
        }
        if (url.startsWith('/api/session-group?group=')) {
          groupDetailFetchCount += 1;
          if (deferredGroupDetailResponse) {
            const response = deferredGroupDetailResponse;
            deferredGroupDetailResponse = null;
            return response;
          }
          if (!groupDetail) {
            return Promise.resolve(
              new Response(JSON.stringify({ error: 'Group detail is temporarily unavailable.' }), {
                status: 503,
                headers: { 'Content-Type': 'application/json' },
              }),
            );
          }
          return Promise.resolve(jsonResponse({ group: groupDetail }));
        }
        if (url === '/api/health') return Promise.resolve(jsonResponse({ version: '0.8.3' }));
        throw new Error(`Unexpected fetch URL: ${url}`);
      }),
    );

    await import('../src/main.js');
    stateModule = await import('../src/state.js');
    composerModule = await import('../src/composerAvailability.js');
    await vi.waitFor(() => expect(FakeWebSocket.instances.length).toBeGreaterThan(0));
    socket = FakeWebSocket.instances.at(-1)!;
    await vi.waitFor(() => expect(stateModule.state.composerConfigRevision).toBe(100));
    socket.onopen?.();
    await Promise.resolve();
  });

  it('keeps newer model state while applying older full Session and Group metadata', async () => {
    socket.receive({
      type: 'session_model_configuration',
      id: 'main',
      model: 'gateway/reasoner',
      effort: 'high',
      modelOverridePresent: false,
      modelOverrideConfigured: false,
      effectiveModelConfigured: false,
      explicitPrimaryModelConfigured: false,
      capabilities: { image: false, s3: false },
      configRevision: 101,
    });
    expect(stateModule.state.composerSessionModelRevision).toBe(101);
    expect(stateModule.state.composerCurrentModel).toBe('gateway/reasoner');
    expect(stateModule.state.composerCurrentEffort).toBe('high');

    stateModule.state.sessionSwitchInFlight = true;
    socket.receive({
      type: 'session',
      id: 'main',
      name: 'Main metadata from older snapshot',
      model: 'gateway/stale',
      effort: 'low',
      modelOverridePresent: true,
      modelOverrideConfigured: true,
      effectiveModelConfigured: true,
      explicitPrimaryModelConfigured: true,
      capabilities: { image: true, s3: true },
      usage: { daily_input: 7, daily_output: 8, total_input: 9, total_output: 10 },
      configRevision: 100,
    });

    expect(stateModule.state.activeSessionId).toBe('main');
    expect(stateModule.dom.sessionNameEl?.textContent).toBe('Main metadata from older snapshot');
    expect(stateModule.state.dailyInputTokens).toBe(7);
    expect(stateModule.state.composerSessionModelRevision).toBe(101);
    expect(stateModule.state.composerEffectiveModelConfigured).toBe(false);
    expect(stateModule.state.composerCurrentModel).toBe('gateway/reasoner');
    expect(stateModule.state.composerCurrentEffort).toBe('high');
    expect(stateModule.state.imageCapable).toBe(false);
    expect(stateModule.state.sessionSwitchInFlight).toBe(false);

    socket.receive({
      type: 'session_model_configuration',
      id: 'old-session',
      modelOverridePresent: true,
      modelOverrideConfigured: true,
      effectiveModelConfigured: true,
      explicitPrimaryModelConfigured: true,
      capabilities: { image: true, s3: true },
      configRevision: 102,
    });
    expect(stateModule.state.composerConfigRevision).toBe(101);
    expect(stateModule.state.composerSessionModelRevision).toBe(101);
    expect(stateModule.state.composerEffectiveModelConfigured).toBe(false);
    expect(stateModule.state.imageCapable).toBe(false);

    composerModule.beginComposerSessionTransition(true, 'target-session');
    socket.receive({
      type: 'session_model_configuration',
      id: 'main',
      modelOverridePresent: false,
      modelOverrideConfigured: false,
      effectiveModelConfigured: true,
      explicitPrimaryModelConfigured: true,
      capabilities: { image: true, s3: false },
      configRevision: 102,
    });
    expect(stateModule.state.composerSessionTransitionPending).toBe(true);
    expect(stateModule.state.imageCapable).toBe(true);

    socket.receive({
      type: 'session_model_configuration',
      id: 'target-session',
      modelOverridePresent: false,
      modelOverrideConfigured: false,
      effectiveModelConfigured: true,
      explicitPrimaryModelConfigured: true,
      capabilities: { image: false, s3: false },
      configRevision: 103,
    });
    expect(stateModule.state.activeSessionId).toBe('main');
    expect(stateModule.state.composerSessionTransitionPending).toBe(true);
    expect(stateModule.state.composerSessionModelRevision).toBe(103);
    expect(stateModule.state.imageCapable).toBe(false);

    socket.receive({
      type: 'session',
      id: 'target-session',
      name: 'Target Session',
      modelOverridePresent: false,
      modelOverrideConfigured: false,
      effectiveModelConfigured: true,
      explicitPrimaryModelConfigured: true,
      capabilities: { image: false, s3: false },
      usage: { daily_input: 5, daily_output: 6, total_input: 7, total_output: 8 },
      configRevision: 102,
    });
    expect(stateModule.state.activeSessionId).toBe('target-session');
    expect(stateModule.state.composerSessionTransitionPending).toBe(false);
    expect(stateModule.dom.sessionNameEl?.textContent).toBe('Target Session');
    expect(stateModule.state.composerSessionModelRevision).toBe(103);
    expect(stateModule.state.imageCapable).toBe(false);

    stateModule.state.activeSessionId = 'missing-or-corrupt';
    socket.onopen?.();
    socket.receive({
      type: 'session',
      id: 'main',
      name: 'Authoritative fallback Session',
      modelOverridePresent: false,
      modelOverrideConfigured: false,
      effectiveModelConfigured: false,
      explicitPrimaryModelConfigured: false,
      capabilities: { image: false, s3: false },
      usage: { daily_input: 11, daily_output: 12, total_input: 13, total_output: 14 },
      configRevision: 103,
    });
    expect(stateModule.state.activeSessionId).toBe('main');
    expect(stateModule.dom.sessionNameEl?.textContent).toBe('Authoritative fallback Session');
    expect(stateModule.state.dailyInputTokens).toBe(11);

    socket.receive({
      type: 'session',
      id: 'late-old-session',
      name: 'Late stale Session metadata',
      modelOverridePresent: true,
      modelOverrideConfigured: true,
      effectiveModelConfigured: true,
      explicitPrimaryModelConfigured: true,
      capabilities: { image: true, s3: true },
      usage: { daily_input: 99, daily_output: 99, total_input: 99, total_output: 99 },
      configRevision: 103,
    });
    expect(stateModule.state.activeSessionId).toBe('main');
    expect(stateModule.dom.sessionNameEl?.textContent).toBe('Authoritative fallback Session');
    expect(stateModule.state.dailyInputTokens).toBe(11);

    configResponseRevision = 104;
    composerModule.beginComposerSessionTransition(true, 'main');
    socket.receive({
      type: 'session_model_configuration',
      id: 'main',
      modelOverridePresent: false,
      modelOverrideConfigured: false,
      effectiveModelConfigured: false,
      explicitPrimaryModelConfigured: false,
      capabilities: { image: true, s3: false },
      configRevision: 104,
    });
    expect(stateModule.state.composerSessionTransitionPending).toBe(true);
    expect(stateModule.state.composerConfigRevision).toBe(104);
    expect(stateModule.state.composerSessionModelRevision).toBeNull();

    composerModule.restoreComposerSessionTransition();
    expect(stateModule.state.composerSessionTransitionPending).toBe(false);
    expect(stateModule.state.composerSessionModelRevision).toBe(104);
    await vi.waitFor(() =>
      expect(stateModule.state.composerModelAvailability).toBe('models-unconfigured'),
    );
    expect(stateModule.state.imageCapable).toBe(true);

    stateModule.state.activeGroupId = 'review-group';
    stateModule.state.activeGroupMembers = [];
    composerModule.resetComposerGroupModelConfiguration();
    socket.receive({
      type: 'group_model_configuration',
      id: 'review-group',
      model_member_ids: ['worker-a'],
      model_configured_members: ['worker-a'],
      explicitPrimaryModelConfigured: false,
      configRevision: 105,
    });
    expect(stateModule.state.composerConfigRevision).toBe(105);
    expect(stateModule.state.composerGroupModelRevision).toBeNull();

    groupDetail = {
      id: 'review-group',
      name: 'Review Group',
      members: ['worker-a'],
      member_details: [{ id: 'worker-a', name: 'Worker A', role: 'member' }],
      pending_votes: [],
      model_configured_members: ['worker-a'],
      explicitPrimaryModelConfigured: false,
      configRevision: 105,
      capabilities: { s3: true, s3_config_id: 's3-b' },
    };
    stateModule.state.s3Capable = true;
    stateModule.state.s3ConfigId = 's3-a';
    stateModule.state.uploadToken = 'old-upload-token';
    stateModule.state.pendingImages = [
      { url: 'https://example.com/remote-before-http-refresh.png' },
      {
        url: 'https://images.example/old-before-http-refresh.png',
        object_key: 'uploads/old-before-http-refresh.png',
        attachment_token: 'old-attachment-token',
        s3_config_id: 's3-a',
      },
    ];
    clientS3ConfigId = 's3-b';
    const fetchesBeforeRoster = groupDetailFetchCount;
    socket.receive({
      type: 'group',
      id: 'review-group',
      name: 'Group roster from older snapshot',
      members: ['worker-a'],
      member_details: [{ id: 'worker-a', name: 'Worker A', role: 'member' }],
      pending_votes: [],
      model_member_ids: ['worker-a'],
      model_configured_members: [],
      explicitPrimaryModelConfigured: false,
      configRevision: 104,
    });
    await vi.waitFor(() => {
      expect(groupDetailFetchCount).toBeGreaterThan(fetchesBeforeRoster);
      expect(stateModule.state.composerGroupModelRevision).toBe(105);
    });
    expect([...stateModule.state.groupModelConfiguredMembers]).toEqual(['worker-a']);
    expect(stateModule.state.s3ConfigId).toBe('s3-b');
    expect(stateModule.state.pendingImages).toEqual([
      { url: 'https://example.com/remote-before-http-refresh.png' },
    ]);

    stateModule.state.sessionSwitchInFlight = true;
    socket.receive({
      type: 'group',
      id: 'review-group',
      name: 'Group metadata from older snapshot',
      members: ['worker-a', 'worker-b'],
      member_details: [
        { id: 'worker-a', name: 'Worker A', role: 'member' },
        { id: 'worker-b', name: 'Worker B', role: 'member' },
      ],
      pending_votes: [],
      model_member_ids: ['worker-a', 'worker-b'],
      model_configured_members: [],
      explicitPrimaryModelConfigured: false,
      configRevision: 104,
    });

    expect(stateModule.state.activeGroupMembers).toEqual(['worker-a', 'worker-b']);
    expect(stateModule.dom.sessionNameEl?.textContent).toBe('Group metadata from older snapshot');
    expect(stateModule.state.composerGroupModelRevision).toBe(105);
    expect([...stateModule.state.groupModelConfiguredMembers]).toEqual(['worker-a']);
    expect(stateModule.state.sessionSwitchInFlight).toBe(false);

    socket.receive({
      type: 'group_history',
      group_id: 'review-group',
      members: ['worker-a'],
      member_details: [{ id: 'worker-a', name: 'Worker A', role: 'member' }],
      pending_votes: [],
      model_configured_members: [],
      explicitPrimaryModelConfigured: false,
      configRevision: 105,
      messages: [],
      runs: [],
    });
    expect(stateModule.state.activeGroupMembers).toEqual(['worker-a', 'worker-b']);
    expect(stateModule.state.composerGroupModelRevision).toBe(105);
    expect([...stateModule.state.groupModelConfiguredMembers]).toEqual(['worker-a']);

    let resolveOldGroupDetail!: (response: Response) => void;
    deferredGroupDetailResponse = new Promise<Response>((resolve) => {
      resolveOldGroupDetail = resolve;
    });
    const fetchesBeforeRestart = groupDetailFetchCount;
    socket.receive({
      type: 'group_model_configuration',
      id: 'review-group',
      model_member_ids: ['worker-from-old-roster'],
      model_configured_members: ['worker-from-old-roster'],
      explicitPrimaryModelConfigured: false,
      configRevision: 106,
    });
    await vi.waitFor(() => expect(groupDetailFetchCount).toBeGreaterThan(fetchesBeforeRestart));

    configResponseRevision = 5;
    socket.onopen?.();
    socket.receive({
      type: 'group_model_configuration',
      id: 'review-group',
      model_member_ids: ['worker-a', 'worker-b'],
      model_configured_members: ['worker-b'],
      explicitPrimaryModelConfigured: false,
      configRevision: 5,
    });
    expect(stateModule.state.composerConfigRevision).toBe(5);
    expect(stateModule.state.composerGroupModelRevision).toBe(5);
    expect(stateModule.state.composerSessionIdentityPending).toBe(true);

    socket.receive({
      type: 'group',
      id: 'review-group',
      name: 'Review Group after restart',
      members: ['worker-a', 'worker-b'],
      member_details: [
        { id: 'worker-a', name: 'Worker A', role: 'member' },
        { id: 'worker-b', name: 'Worker B', role: 'member' },
      ],
      pending_votes: [],
      model_member_ids: ['worker-a', 'worker-b'],
      model_configured_members: ['worker-b'],
      explicitPrimaryModelConfigured: false,
      configRevision: 5,
    });
    expect(stateModule.state.composerSessionIdentityPending).toBe(false);

    groupDetail = {
      id: 'review-group',
      name: 'Review Group after restart',
      members: ['worker-a', 'worker-b'],
      member_details: [
        { id: 'worker-a', name: 'Worker A', role: 'member' },
        { id: 'worker-b', name: 'Worker B', role: 'member' },
      ],
      pending_votes: [],
      model_configured_members: ['worker-b'],
      explicitPrimaryModelConfigured: false,
      configRevision: 5,
    };
    resolveOldGroupDetail(
      jsonResponse({
        group: {
          ...groupDetail,
          name: 'Delayed detail from old process',
          model_configured_members: ['worker-a'],
          configRevision: 106,
        },
      }),
    );
    await vi.waitFor(() => {
      expect(groupDetailFetchCount).toBeGreaterThan(fetchesBeforeRestart + 1);
      expect(stateModule.state.composerConfigRevision).toBe(5);
      expect(stateModule.state.composerGroupModelRevision).toBe(5);
      expect([...stateModule.state.groupModelConfiguredMembers]).toEqual(['worker-b']);
    });

    stateModule.state.activeGroupId = '';
    stateModule.state.activeSessionId = 'main';
    stateModule.state.composerSessionIdentityPending = false;
    stateModule.state.imageCapable = true;
    stateModule.state.s3Capable = false;
    stateModule.state.pendingImages = [{ url: 'https://images.example/source-draft.png' }];
    composerModule.setComposerSessionModelConfigured(false, false, true, 5);
    composerModule.beginComposerSessionTransition(true, 'target-before-disconnect');
    socket.receive({
      type: 'session_model_configuration',
      id: 'target-before-disconnect',
      modelOverridePresent: false,
      modelOverrideConfigured: false,
      effectiveModelConfigured: true,
      explicitPrimaryModelConfigured: false,
      capabilities: { image: false, s3: false },
      configRevision: 5,
    });
    expect(stateModule.state.imageCapable).toBe(false);
    expect(stateModule.state.pendingImages).toEqual([]);
    socket.receive({
      type: 'session_model_configuration',
      id: 'target-before-disconnect',
      modelOverridePresent: false,
      modelOverrideConfigured: false,
      effectiveModelConfigured: true,
      explicitPrimaryModelConfigured: false,
      capabilities: { image: false, s3: false },
      configRevision: 5,
    });
    socket.receive({
      type: 'session_model_configuration',
      id: 'main',
      modelOverridePresent: false,
      modelOverrideConfigured: false,
      effectiveModelConfigured: true,
      explicitPrimaryModelConfigured: false,
      capabilities: { image: true, s3: false },
      configRevision: 5,
    });
    expect(stateModule.state.imageCapable).toBe(false);
    expect(stateModule.state.pendingImages).toEqual([]);

    stateModule.state.reconnectAttempts = 50;
    socket.onclose?.();
    expect(stateModule.state.composerSessionTransitionPending).toBe(false);
    expect(stateModule.state.imageCapable).toBe(true);
    expect(stateModule.state.pendingImages).toEqual([
      { url: 'https://images.example/source-draft.png' },
    ]);
  });

  it('applies S3 identity changes while connected to a Group', async () => {
    document.getElementById('composer-availability-action')?.click();
    await vi.waitFor(() => expect(FakeWebSocket.instances.at(-1)).not.toBe(socket));
    socket = FakeWebSocket.instances.at(-1)!;
    socket.onopen?.();
    stateModule.state.activeGroupId = 'review-group';
    stateModule.state.activeGroupMembers = ['worker-a'];
    stateModule.state.groupModelConfiguredMembers = new Set(['worker-a']);
    stateModule.state.s3Capable = true;
    stateModule.state.s3ConfigId = 's3-a';
    stateModule.state.uploadToken = 'old-upload-token';
    stateModule.state.uploadTokenPromise = null;
    stateModule.state.uploadTokenRequestSeq += 1;
    stateModule.state.pendingImages = [
      { url: 'https://example.com/remote.png' },
      {
        url: 'https://images.example/old-storage.png',
        object_key: 'uploads/old-storage.png',
        attachment_token: 'old-attachment-token',
        s3_config_id: 's3-a',
      },
    ];
    clientS3ConfigId = 's3-b';
    const uploadFetchesBefore = uploadTokenFetchCount;

    socket.receive({
      type: 'group_model_configuration',
      id: 'review-group',
      model_member_ids: ['worker-a'],
      model_configured_members: ['worker-a'],
      explicitPrimaryModelConfigured: false,
      capabilities: { s3: true, s3_config_id: 's3-b' },
      configRevision: 6,
    });

    await vi.waitFor(() => {
      expect(uploadTokenFetchCount).toBeGreaterThan(uploadFetchesBefore);
      expect(stateModule.state.uploadTokenPromise).toBeNull();
    });
    expect(stateModule.state.s3ConfigId).toBe('s3-b');
    expect(stateModule.state.pendingImages).toEqual([{ url: 'https://example.com/remote.png' }]);

    stateModule.state.activeGroupId = '';
    stateModule.state.pendingImages = [];
    clientS3ConfigId = 's3-a';
  });

  it('locks attachment changes but keeps the options menu reachable during Session creation', async () => {
    let resolveCreate!: (response: Response) => void;
    deferredSessionCreateResponse = new Promise<Response>((resolveResponse) => {
      resolveCreate = resolveResponse;
    });
    stateModule.state.activeGroupId = '';
    stateModule.state.activeSessionId = 'main';
    stateModule.state.sessionSwitchInFlight = false;
    stateModule.state.sessionIdentityMutationInFlight = false;
    stateModule.state.composerSessionIdentityPending = false;
    stateModule.state.composerSessionTransitionPending = false;
    stateModule.state.imageCapable = true;
    stateModule.state.s3Capable = true;
    stateModule.state.pendingImages = [];
    stateModule.state.uploadToken = '';
    stateModule.state.uploadTokenPromise = null;
    stateModule.state.uploadTokenRequestSeq += 1;
    stateModule.state.s3ConfigId = 's3-a';

    const createButton = stateModule.dom.sessionDrawerNewBtn!;
    createButton.disabled = false;
    createButton.click();
    document.querySelector<HTMLButtonElement>('.action-dialog-submit')?.click();

    await vi.waitFor(() => expect(stateModule.state.sessionIdentityMutationInFlight).toBe(true));
    expect(stateModule.state.sessionSwitchInFlight).toBe(false);
    expect(createButton.disabled).toBe(true);
    expect(stateModule.dom.attachBtn?.disabled).toBe(false);
    expect(stateModule.dom.attachLocalBtn?.disabled).toBe(false);
    expect(stateModule.dom.attachLocalBtn?.getAttribute('aria-disabled')).toBe('true');

    socket.receive({
      type: 'session',
      id: 'main',
      name: 'Late source Session payload',
      modelOverridePresent: false,
      modelOverrideConfigured: false,
      effectiveModelConfigured: true,
      explicitPrimaryModelConfigured: true,
      capabilities: { image: true, s3: true },
      usage: {},
      configRevision: configResponseRevision,
    });
    expect(stateModule.state.sessionIdentityMutationInFlight).toBe(true);
    expect(stateModule.dom.attachBtn?.disabled).toBe(false);
    expect(stateModule.dom.attachLocalBtn?.disabled).toBe(false);
    expect(stateModule.dom.attachLocalBtn?.getAttribute('aria-disabled')).toBe('true');

    const uploadFetchesBefore = uploadTokenFetchCount;
    const { uploadLocalImages } = await import('../src/images.js');
    await uploadLocalImages([new File(['image'], 'image.png', { type: 'image/png' })]);
    expect(uploadTokenFetchCount).toBe(uploadFetchesBefore);
    expect(stateModule.state.imageUploadInFlight).toBe(false);

    resolveCreate(jsonResponse({ session: { id: 'created-session', name: 'Created Session' } }));
    await vi.waitFor(() => expect(stateModule.state.activeSessionId).toBe('created-session'));
    expect(stateModule.state.sessionIdentityMutationInFlight).toBe(false);
    expect(stateModule.state.sessionSwitchInFlight).toBe(true);

    const createdSocket = FakeWebSocket.instances.at(-1)!;
    createdSocket.onopen?.();
    createdSocket.receive({
      type: 'session',
      id: 'created-session',
      name: 'Created Session',
      modelOverridePresent: false,
      modelOverrideConfigured: false,
      effectiveModelConfigured: true,
      explicitPrimaryModelConfigured: true,
      capabilities: { image: true, s3: true },
      usage: {},
      configRevision: configResponseRevision,
    });

    expect(stateModule.state.sessionSwitchInFlight).toBe(false);
    expect(stateModule.dom.attachBtn?.disabled).toBe(false);
  });

  it('does not clear an independent Session transition when a create request fails', async () => {
    let resolveCreate!: (response: Response) => void;
    deferredSessionCreateResponse = new Promise<Response>((resolveResponse) => {
      resolveCreate = resolveResponse;
    });
    stateModule.state.sessionSwitchInFlight = false;
    stateModule.state.sessionIdentityMutationInFlight = false;
    stateModule.state.composerSessionIdentityPending = false;
    stateModule.state.composerSessionTransitionPending = false;
    stateModule.dom.sessionDrawerNewBtn!.disabled = false;
    stateModule.dom.sessionDrawerNewBtn!.click();
    document.querySelector<HTMLButtonElement>('.action-dialog-submit')?.click();
    await vi.waitFor(() => expect(stateModule.state.sessionIdentityMutationInFlight).toBe(true));

    composerModule.beginComposerSessionTransition(false, 'main');
    stateModule.state.sessionSwitchInFlight = true;
    resolveCreate(jsonResponse({ error: 'create failed' }, 500));
    await vi.waitFor(() => expect(stateModule.state.sessionIdentityMutationInFlight).toBe(false));

    expect(stateModule.state.sessionSwitchInFlight).toBe(true);
    expect(stateModule.state.composerSessionTransitionPending).toBe(true);

    composerModule.completeComposerSessionTransition();
    stateModule.state.sessionSwitchInFlight = false;
  });

  it('clears the previous Group member UI before reconnecting to another Group', async () => {
    stateModule.state.activeSessionId = 'main';
    stateModule.state.activeGroupId = 'old-group';
    stateModule.state.activeGroupMembers = ['old-worker'];
    stateModule.state.activeGroupMemberDetails = [
      { id: 'main', name: 'Main', role: 'owner' },
      { id: 'old-worker', name: 'Old worker', role: 'member' },
    ];
    stateModule.state.activeGroupPendingVotes = [
      {
        id: 'old-vote',
        action: 'remove',
        target_session_id: 'old-worker',
        requester_session_id: 'main',
        approvals: ['main'],
        threshold: 1,
        created_at: 1,
        updated_at: 1,
      },
    ];
    stateModule.state.groupMembersDrawerOpen = true;
    stateModule.state.groupTargetPickerOpen = true;
    stateModule.state.groupTargetSearchQuery = 'old';
    stateModule.state.groupMemberMenuId = 'old-worker';
    stateModule.state.groupTargetMode = 'selected';
    stateModule.state.groupSelectedTargets = ['old-worker'];
    stateModule.state.sessionSwitchInFlight = false;
    stateModule.state.sessionIdentityMutationInFlight = false;
    stateModule.state.composerSessionIdentityPending = false;
    stateModule.state.composerSessionTransitionPending = false;
    stateModule.state.imageUploadInFlight = false;
    stateModule.state.sessionGroups = [{ id: 'next-group', name: 'Next Group' }];

    const { renderSessionDrawer } = await import('../src/renderers/sessions.js');
    renderSessionDrawer();
    const socketCountBeforeSwitch = FakeWebSocket.instances.length;
    stateModule.dom.sessionDrawerList
      ?.querySelector<HTMLButtonElement>(
        '[data-group-id="next-group"] [data-session-action="switch-group"]',
      )
      ?.click();

    await vi.waitFor(() =>
      expect(FakeWebSocket.instances.length).toBeGreaterThan(socketCountBeforeSwitch),
    );

    expect(stateModule.state.activeGroupId).toBe('next-group');
    expect(stateModule.state.activeGroupMembers).toEqual([]);
    expect(stateModule.state.activeGroupMemberDetails).toEqual([]);
    expect(stateModule.state.activeGroupPendingVotes).toEqual([]);
    expect(stateModule.state.groupMembersDrawerOpen).toBe(false);
    expect(stateModule.state.groupTargetPickerOpen).toBe(false);
    expect(stateModule.state.groupTargetSearchQuery).toBe('');
    expect(stateModule.state.groupMemberMenuId).toBe('');
    expect(stateModule.state.groupTargetMode).toBe('all');
    expect(stateModule.state.groupSelectedTargets).toEqual([]);
    expect(stateModule.state.sessionSwitchInFlight).toBe(true);

    const nextGroupSocket = FakeWebSocket.instances.at(-1)!;
    socket = nextGroupSocket;
    nextGroupSocket.receive({
      type: 'group',
      id: 'next-group',
      name: 'Next Group',
      members: ['worker-a'],
      member_details: [{ id: 'worker-a', name: 'Worker A', role: 'member' }],
      pending_votes: [],
      model_member_ids: ['worker-a'],
      model_configured_members: ['worker-a'],
      explicitPrimaryModelConfigured: false,
      configRevision: stateModule.state.composerConfigRevision,
    });

    const selectedMode = stateModule.dom.groupTargetBar?.querySelector<HTMLButtonElement>(
      '.group-target-mode[data-mode="selected"]',
    );
    selectedMode?.click();
    await Promise.resolve();
    expect(document.activeElement).toBe(
      stateModule.dom.groupTargetBar?.querySelector('.group-target-picker-search input'),
    );

    stateModule.dom.groupTargetBar
      ?.querySelector<HTMLButtonElement>('.group-target-selection-toggle')
      ?.click();
    await Promise.resolve();
    expect(document.activeElement).toBe(
      stateModule.dom.groupTargetBar?.querySelector('.group-target-selection-toggle'),
    );

    stateModule.dom.groupTargetBar
      ?.querySelector<HTMLButtonElement>('.group-members-toggle')
      ?.click();
    await Promise.resolve();
    expect(document.querySelector('.group-member-role--owner')?.textContent).toBe('Owner');

    document.querySelector<HTMLButtonElement>('.group-member-mention')?.click();
    expect(stateModule.state.groupTargetMode).toBe('mentions');
    expect(stateModule.state.groupTargetPickerOpen).toBe(false);
    expect(stateModule.dom.input?.value).toBe('@worker-a ');
    expect(
      stateModule.dom.groupTargetBar
        ?.querySelector('.group-target-mode[data-mode="mentions"]')
        ?.getAttribute('aria-pressed'),
    ).toBe('true');

    stateModule.dom.groupTargetBar
      ?.querySelector<HTMLButtonElement>('.group-members-toggle')
      ?.click();
    await Promise.resolve();

    document
      .querySelector<HTMLButtonElement>('.group-member-menu-trigger[data-session-id="worker-a"]')
      ?.click();
    await Promise.resolve();
    expect(document.activeElement?.getAttribute('role')).toBe('menuitem');

    document
      .querySelector<HTMLButtonElement>('.group-member-menu-trigger[data-session-id="worker-a"]')
      ?.click();
    await Promise.resolve();
    expect(document.activeElement).toBe(
      document.querySelector('.group-member-menu-trigger[data-session-id="worker-a"]'),
    );
  });

  it('keeps a hard-cap system notice inside the live run until done marks it incomplete', () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    stateModule.state.activeGroupId = '';
    stateModule.state.storageMode = 'healthy';
    currentSocket.receive({ type: 'history', messages: [] });
    currentSocket.receive({ type: 'start', react_visible: true, phase: 'analyze', cycle: 1 });
    currentSocket.receive({
      type: 'tool_call',
      name: 'read_file',
      arguments: '{"path":"README.md"}',
      id: 'hard-cap-tool',
    });
    const stack = stateModule.state.activeExecutionStack!;

    currentSocket.receive({
      type: 'system',
      content: 'Detected abnormal tool loop. Stopping.',
    });

    expect(stack.dataset.executionStatus).toBe('running');
    expect(stateModule.state.activeExecutionStack).toBe(stack);
    expect(stateModule.state.activeExecutionRunId).toBeGreaterThan(0);
    expect(stateModule.state.busy).toBe(true);

    currentSocket.receive({ type: 'done', phase: 'hard_cap', reason: 'hard_cap' });

    expect(stack.dataset.executionStatus).toBe('incomplete');
    expect(stack.classList.contains('is-incomplete')).toBe(true);
    expect(stack.querySelector<HTMLElement>('.execution-stack-recovery')?.hidden).toBe(false);
    expect(stateModule.state.activeExecutionRunId).toBe(0);
    expect(stateModule.state.busy).toBe(false);
  });

  it.each([
    {
      label: 'stopped',
      phase: 'stopped',
      reason: 'user_stop',
      expectedStatus: 'stopped',
    },
    {
      label: 'failed',
      phase: 'failed',
      reason: 'provider_error',
      expectedStatus: 'failed',
    },
    {
      label: 'hard cap',
      phase: 'hard_cap',
      reason: 'hard_cap',
      expectedStatus: 'incomplete',
    },
    {
      label: 'empty response',
      phase: 'finish',
      reason: 'empty_response',
      expectedStatus: 'incomplete',
    },
    {
      label: 'explicit partial',
      phase: 'partial',
      reason: 'partial',
      expectedStatus: 'partial',
    },
    {
      label: 'unknown attention outcome',
      phase: 'unknown',
      reason: 'unknown',
      expectedStatus: 'partial',
    },
  ])(
    'creates one recoverable attention stack for a no-step $label done',
    ({ label, phase, reason, expectedStatus }) => {
      const currentSocket = FakeWebSocket.instances.at(-1)!;
      const runIdentity = `no-step-${label.replaceAll(' ', '-')}`;
      stateModule.state.executionIdentityProtocol = 'strict';
      currentSocket.receive({ type: 'history', messages: [] });
      currentSocket.receiveRaw({
        type: 'start',
        run_connection_id: runIdentity,
        react_visible: false,
        phase: 'analyze',
        cycle: 1,
      });
      expect(document.querySelector('.execution-stack')).toBeNull();

      currentSocket.receiveRaw({
        type: 'done',
        run_connection_id: runIdentity,
        phase,
        reason,
      });

      const stack = document.querySelector<HTMLElement>('.execution-stack');
      const summary = stack?.querySelector('.execution-stack-summary')?.textContent || '';
      expect(document.querySelectorAll('.execution-stack')).toHaveLength(1);
      expect(stack?.dataset.executionStatus).toBe(expectedStatus);
      expect(stack?.dataset.executionClientRunId).toBeTruthy();
      expect(stack?.dataset.executionServerRunId).toBe(runIdentity);
      expect(Number(stack?.dataset.executionDuration || 0)).toBeGreaterThan(0);
      expect(summary).not.toBe('');
      expect(stack?.querySelector<HTMLElement>('.execution-stack-recovery')?.hidden).toBe(false);
      expect(stack?.querySelector('.execution-stack-recovery-action')?.textContent).not.toBe('');
      expect(stack?.querySelector('.execution-stack-header')?.getAttribute('aria-label')).toContain(
        summary,
      );
      expect(stateModule.state.activeExecutionRunId).toBe(0);
      expect(stateModule.state.busy).toBe(false);
    },
  );

  it('does not create an empty stack for a no-step completed run', () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    const runIdentity = 'no-step-complete';
    stateModule.state.executionIdentityProtocol = 'strict';
    currentSocket.receive({ type: 'history', messages: [] });
    currentSocket.receiveRaw({
      type: 'start',
      run_connection_id: runIdentity,
      react_visible: false,
      phase: 'analyze',
      cycle: 1,
    });
    currentSocket.receiveRaw({
      type: 'done',
      run_connection_id: runIdentity,
      phase: 'finish',
      reason: 'complete',
    });

    expect(document.querySelectorAll('.execution-stack')).toHaveLength(0);
    expect(stateModule.state.activeExecutionRunId).toBe(0);
    expect(stateModule.state.busy).toBe(false);
  });

  it('does not create an attention stack for missing or mismatched strict done identity', () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    const runIdentity = 'strict-attention-owner';
    stateModule.state.executionIdentityProtocol = 'strict';
    currentSocket.receive({ type: 'history', messages: [] });
    currentSocket.receiveRaw({
      type: 'start',
      run_connection_id: runIdentity,
      react_visible: false,
      phase: 'analyze',
      cycle: 1,
    });
    const clientRunId = stateModule.state.activeExecutionRunId;

    currentSocket.receiveRaw({ type: 'done', phase: 'failed', reason: 'provider_error' });
    currentSocket.receiveRaw({
      type: 'done',
      run_connection_id: 'another-run',
      phase: 'stopped',
      reason: 'user_stop',
    });

    expect(document.querySelectorAll('.execution-stack')).toHaveLength(0);
    expect(stateModule.state.activeExecutionRunId).toBe(clientRunId);
    expect(stateModule.state.busy).toBe(true);

    currentSocket.receiveRaw({
      type: 'done',
      run_connection_id: runIdentity,
      phase: 'failed',
      reason: 'provider_error',
    });
    expect(document.querySelectorAll('.execution-stack')).toHaveLength(1);
    expect(stateModule.state.activeExecutionRunId).toBe(0);
  });

  it.each([
    { phase: 'finish', reason: 'complete', expectedStatus: null },
    { phase: 'stopped', reason: 'user_stop', expectedStatus: 'stopped' },
    { phase: 'failed', reason: 'provider_error', expectedStatus: 'failed' },
  ])(
    'closes an identityless legacy start with same-socket $phase done',
    ({ phase, reason, expectedStatus }) => {
      const currentSocket = FakeWebSocket.instances.at(-1)!;
      const originalSocketGeneration = stateModule.state.socketGeneration;
      stateModule.state.executionIdentityProtocol = 'legacy';
      stateModule.state.legacyExecutionSocketGeneration = originalSocketGeneration;
      currentSocket.receive({ type: 'history', messages: [] });

      currentSocket.receiveRaw({
        type: 'start',
        react_visible: false,
        phase: 'analyze',
        cycle: 1,
      });
      expect(stateModule.state.activeExecutionServerRunId).toBe(
        `legacy-socket-${originalSocketGeneration}`,
      );
      expect(stateModule.state.busy).toBe(true);

      currentSocket.receiveRaw({ type: 'done', phase, reason });

      const stack = document.querySelector<HTMLElement>('.execution-stack');
      expect(stateModule.state.activeExecutionRunId).toBe(0);
      expect(stateModule.state.busy).toBe(false);
      if (expectedStatus) {
        expect(stack?.dataset.executionStatus).toBe(expectedStatus);
      } else {
        expect(stack).toBeNull();
      }

      currentSocket.receive({ type: 'history', messages: [] });
      stateModule.state.executionIdentityProtocol = 'strict';
      stateModule.state.legacyExecutionSocketGeneration = 0;
    },
  );

  it('keeps an identityless legacy nonterminal error live and closes on terminal error', () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    const originalSocketGeneration = stateModule.state.socketGeneration;
    stateModule.state.executionIdentityProtocol = 'legacy';
    stateModule.state.legacyExecutionSocketGeneration = originalSocketGeneration;
    currentSocket.receive({ type: 'history', messages: [] });
    currentSocket.receiveRaw({
      type: 'start',
      react_visible: false,
      phase: 'analyze',
      cycle: 1,
    });
    const clientRunId = stateModule.state.activeExecutionRunId;

    currentSocket.receiveRaw({
      type: 'error',
      run_terminal: false,
      content: 'A legacy busy command failed.',
    });
    expect(stateModule.state.activeExecutionRunId).toBe(clientRunId);
    expect(stateModule.state.busy).toBe(true);

    currentSocket.receiveRaw({
      type: 'error',
      run_terminal: true,
      content: 'The legacy run failed.',
    });
    const stack = document.querySelector<HTMLElement>('.execution-stack');
    expect(stack?.dataset.executionStatus).toBe('failed');
    expect(stack?.dataset.executionServerRunId).toBe(`legacy-socket-${originalSocketGeneration}`);
    expect(stateModule.state.activeExecutionRunId).toBe(0);
    expect(stateModule.state.busy).toBe(false);

    currentSocket.receive({ type: 'history', messages: [] });
    stateModule.state.executionIdentityProtocol = 'strict';
    stateModule.state.legacyExecutionSocketGeneration = 0;
  });

  it('quarantines identityless legacy terminal events after a socket-generation reset', () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    const originalSocketGeneration = stateModule.state.socketGeneration;
    stateModule.state.executionIdentityProtocol = 'legacy';
    stateModule.state.legacyExecutionSocketGeneration = originalSocketGeneration;
    currentSocket.receive({ type: 'history', messages: [] });
    currentSocket.receiveRaw({
      type: 'start',
      react_visible: false,
      phase: 'analyze',
      cycle: 1,
    });
    currentSocket.receive({ type: 'history', messages: [] });

    stateModule.state.legacyExecutionSocketGeneration = originalSocketGeneration + 1;
    stateModule.state.busy = false;
    currentSocket.receiveRaw({ type: 'done', phase: 'failed', reason: 'provider_error' });
    currentSocket.receiveRaw({
      type: 'error',
      run_terminal: true,
      content: 'Late legacy terminal error.',
    });
    expect(document.querySelectorAll('.execution-stack')).toHaveLength(0);
    expect(stateModule.state.activeExecutionRunId).toBe(0);
    expect(stateModule.state.busy).toBe(false);

    currentSocket.receiveRaw({
      type: 'start',
      run_connection_id: 'new-strict-run',
      react_visible: false,
      phase: 'analyze',
      cycle: 1,
    });
    const newClientRunId = stateModule.state.activeExecutionRunId;
    currentSocket.receiveRaw({ type: 'done', phase: 'stopped', reason: 'user_stop' });
    currentSocket.receiveRaw({
      type: 'error',
      run_terminal: true,
      content: 'Another late identityless terminal.',
    });
    expect(stateModule.state.activeExecutionRunId).toBe(newClientRunId);
    expect(stateModule.state.activeExecutionServerRunId).toBe('new-strict-run');
    expect(stateModule.state.busy).toBe(true);
    expect(document.querySelectorAll('.execution-stack')).toHaveLength(0);

    currentSocket.receiveRaw({
      type: 'done',
      run_connection_id: 'new-strict-run',
      phase: 'finish',
      reason: 'complete',
    });
    expect(stateModule.state.activeExecutionRunId).toBe(0);
    expect(stateModule.state.busy).toBe(false);

    stateModule.state.executionIdentityProtocol = 'strict';
    stateModule.state.legacyExecutionSocketGeneration = 0;
  });

  it.each([
    { label: 'plan already active', code: 'plan_already_active' },
    {
      label: 'stale plan revision',
      code: 'stale_plan_revision',
      plan: terminalPlanPayload('idle-stale-plan', 'failed', 'in_progress'),
    },
    { label: 'plan not ready', code: 'plan_not_ready' },
    { label: 'Session preflight', code: 'session_not_found' },
    { label: 'model preflight', code: 'agent_model_unconfigured' },
    { label: 'workspace preflight', code: 'workspace_unavailable' },
  ])('renders an idle $label error without an execution stack', ({ code, plan }) => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    currentSocket.receive({ type: 'history', messages: [] });
    const previousTerminalStack = stateModule.state.terminalExecutionStack;
    const previousRunId = stateModule.state.activeExecutionRunId;
    const previousPlanId = stateModule.state.activeExecutionPlanId;

    currentSocket.receive({
      type: 'error',
      code,
      content: `Idle preflight error: ${code}`,
      plan,
    });

    expect(document.querySelectorAll('.execution-stack')).toHaveLength(0);
    expect(document.querySelectorAll('.msg-row.error')).toHaveLength(1);
    expect(stateModule.state.activeExecutionRunId).toBe(previousRunId);
    expect(stateModule.state.activeExecutionPlanId).toBe(previousPlanId);
    expect(stateModule.state.terminalExecutionStack).toBe(previousTerminalStack);
  });

  it('creates one failed stack when start is followed by error before any process step', () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    currentSocket.receive({ type: 'history', messages: [] });
    currentSocket.receive({ type: 'start', react_visible: false, phase: 'analyze', cycle: 1 });
    expect(document.querySelector('.execution-stack')).toBeNull();

    currentSocket.receive({
      type: 'error',
      run_terminal: true,
      code: 'provider_error',
      content: 'Analyze failed before producing a process step.',
    });

    const stack = stateModule.state.terminalExecutionStack;
    expect(document.querySelectorAll('.execution-stack')).toHaveLength(1);
    expect(stack).not.toBeNull();
    expect(stack?.dataset.executionStatus).toBe('failed');
    expect(stack?.dataset.executionTerminalSource).toBe('error');
    expect(stack?.dataset.executionClientRunId).toBeTruthy();
    expect(stateModule.state.activeExecutionRunId).toBe(0);
    expect(stateModule.state.activeExecutionPlanId).toBe('');
    expect(stateModule.state.busy).toBe(false);
    expect(stateModule.state.reactStatusRow).toBeNull();
    expect(stateModule.state.reactPhaseTimer).toBe(0);
    expect(stateModule.state.currentRoundStartedAt).toBe(0);
    expect(stateModule.state.currentMsg).toBeNull();
  });

  it('closes an exact run as recoverable incomplete when terminal identity persistence fails', () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    currentSocket.receive({ type: 'history', messages: [] });
    currentSocket.receive({ type: 'start', react_visible: false, phase: 'analyze', cycle: 1 });
    expect(stateModule.state.activeExecutionRunId).toBeGreaterThan(0);

    currentSocket.receive({
      type: 'error',
      run_terminal: true,
      phase: 'incomplete',
      reason: 'terminal_identity_unavailable',
      code: 'terminal_identity_unavailable',
      content: 'SERVER_DIAGNOSTIC_MUST_NOT_REPLACE_LOCALIZED_RECOVERY',
      recoverable: true,
    });

    const stack = stateModule.state.terminalExecutionStack;
    expect(document.querySelectorAll('.execution-stack')).toHaveLength(1);
    expect(document.querySelectorAll('.msg-row.error')).toHaveLength(0);
    expect(stack?.dataset.executionStatus).toBe('incomplete');
    expect(stack?.dataset.executionTerminalSource).toBe('error');
    expect(stack?.dataset.executionOutcomeKey).toBe('execution.terminalIdentityUnavailable');
    expect(stack?.classList.contains('is-expanded')).toBe(true);
    expect(stack?.querySelector('.execution-stack-header')?.getAttribute('aria-expanded')).toBe(
      'true',
    );
    expect(stack?.querySelector('.execution-stack-summary')?.textContent).toContain(
      'could not safely bind',
    );
    expect(stack?.querySelector('.execution-stack-recovery-action')?.textContent).toBe(
      'Review unresolved work',
    );
    expect(document.body.textContent).not.toContain(
      'SERVER_DIAGNOSTIC_MUST_NOT_REPLACE_LOCALIZED_RECOVERY',
    );
    expect(stateModule.state.activeExecutionRunId).toBe(0);
    expect(stateModule.state.busy).toBe(false);
  });

  it('keeps an active run alive across explicit and legacy nonterminal errors', () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    const planId = 'plan-nonterminal-error';
    currentSocket.receive({ type: 'history', messages: [] });
    stateModule.state.pendingPlanExecutionId = planId;
    currentSocket.receive({ type: 'start', react_visible: true, phase: 'analyze', cycle: 1 });

    const stack = stateModule.state.activeExecutionStack!;
    const clientRunId = stateModule.state.activeExecutionRunId;
    const reactRow = stateModule.state.reactStatusRow;
    const roundStartedAt = stateModule.state.currentRoundStartedAt;

    currentSocket.receive({
      type: 'error',
      run_terminal: false,
      code: 'think_update_failed',
      content: 'The busy /think command failed while the Agent kept running.',
    });
    currentSocket.receive({
      type: 'error',
      code: 'legacy_busy_command_error',
      content: 'An unclassified legacy error must also fail safe as nonterminal.',
    });

    expect(document.querySelectorAll('.msg-row.error')).toHaveLength(2);
    expect(stateModule.state.activeExecutionStack).toBe(stack);
    expect(stateModule.state.activeExecutionRunId).toBe(clientRunId);
    expect(stateModule.state.activeExecutionPlanId).toBe(planId);
    expect(stateModule.state.busy).toBe(true);
    expect(stateModule.state.reactStatusRow).toBe(reactRow);
    expect(stateModule.state.currentRoundStartedAt).toBe(roundStartedAt);

    currentSocket.receive({
      type: 'tool_call',
      name: 'read_file',
      arguments: '{"path":"README.md"}',
      id: 'nonterminal-error-tool',
    });
    currentSocket.receive({
      type: 'plan_state',
      plan: {
        plan_id: planId,
        revision: 1,
        status: 'executing',
        message_index: 2,
        created_at: 1710000000,
        updated_at: 1710000001,
        approved_at: 1710000001,
        execution_attempt: 1,
        artifact: {
          title: 'Continue after control error',
          goal: 'Prove nonterminal errors keep the run alive',
          steps: [{ id: 'inspect', title: 'Inspect the workspace' }],
        },
        progress: [{ id: 'inspect', title: 'Inspect the workspace', status: 'in_progress' }],
      },
    });

    expect(stateModule.state.activeExecutionStack).toBe(stack);
    expect(stack.dataset.executionPlanId).toBe(planId);
    expect(document.querySelectorAll('.execution-stack')).toHaveLength(1);

    currentSocket.receive({ type: 'done', phase: 'finish', reason: 'complete' });

    expect(document.querySelectorAll('.execution-stack')).toHaveLength(1);
    expect(stack.dataset.executionStatus).toBe('completed');
    expect(stateModule.state.activeExecutionRunId).toBe(0);
    expect(stateModule.state.busy).toBe(false);
  });

  it('keeps an LLM retry transient and removes it after a successful no-step run', () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    currentSocket.receive({ type: 'history', messages: [] });
    currentSocket.receive({ type: 'start', react_visible: false, phase: 'analyze', cycle: 1 });
    currentSocket.receive({ type: 'progress', kind: 'llm_retry', attempt: 2, max_attempts: 2 });

    expect(document.querySelector('.execution-stack-summary')?.textContent).toContain(
      'Retrying the model request',
    );
    expect(document.querySelector('.msg-row.system')).toBeNull();

    currentSocket.receive({ type: 'delta', content: 'Recovered response.' });
    currentSocket.receive({ type: 'done', phase: 'finish', reason: 'complete' });

    expect(document.body.textContent).not.toContain('Retrying model request');
    expect(document.querySelector('.execution-stack')).toBeNull();
    expect(stateModule.state.busy).toBe(false);
  });

  it('shows terminal provider failure text once after a transient retry', () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    const failure = 'UNIQUE_TERMINAL_PROVIDER_FAILURE';
    currentSocket.receive({ type: 'history', messages: [] });
    currentSocket.receive({ type: 'start', react_visible: false, phase: 'analyze', cycle: 1 });
    currentSocket.receive({ type: 'progress', kind: 'llm_retry', attempt: 2, max_attempts: 2 });
    currentSocket.receive({
      type: 'error',
      run_terminal: true,
      code: 'provider_error',
      content: failure,
    });

    const stack = stateModule.state.terminalExecutionStack;
    expect(document.querySelectorAll('.execution-stack')).toHaveLength(1);
    expect(stack?.dataset.executionStatus).toBe('failed');
    expect(stack?.querySelector('.execution-stack-summary')?.textContent).toBe(failure);
    expect(document.querySelectorAll('.msg-row.error')).toHaveLength(0);
    expect(document.body.textContent?.split(failure)).toHaveLength(2);
    expect(document.body.textContent).not.toContain('Retrying model request');
    expect(stack?.querySelector('.execution-stack-recovery-summary')?.textContent).not.toBe(
      failure,
    );
  });

  it('uses explicit stopped and failed live done outcomes', () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    currentSocket.receive({ type: 'history', messages: [] });
    currentSocket.receive({ type: 'start', react_visible: true, phase: 'analyze', cycle: 1 });
    currentSocket.receive({
      type: 'tool_call',
      name: 'write_file',
      arguments: '{"path":"result.txt"}',
      id: 'stopped-tool',
    });
    const stoppedStack = stateModule.state.activeExecutionStack!;
    currentSocket.receive({ type: 'done', phase: 'stopped', reason: 'user_stop' });
    expect(stoppedStack.dataset.executionStatus).toBe('stopped');

    currentSocket.receive({ type: 'start', react_visible: true, phase: 'analyze', cycle: 1 });
    currentSocket.receive({
      type: 'tool_call',
      name: 'cargo_test',
      arguments: '{}',
      id: 'failed-tool',
    });
    const failedStack = stateModule.state.activeExecutionStack!;
    currentSocket.receive({
      type: 'done',
      phase: 'failed',
      reason: 'completion_contract_failed',
    });
    expect(failedStack.dataset.executionStatus).toBe('failed');
    expect(failedStack.querySelector('.execution-stack-summary')?.textContent).toContain(
      'completion contract',
    );

    currentSocket.receive({ type: 'start', react_visible: true, phase: 'analyze', cycle: 1 });
    currentSocket.receive({
      type: 'tool_call',
      name: 'read_file',
      arguments: '{}',
      id: 'ordinary-error-tool',
    });
    const ordinaryErrorStack = stateModule.state.activeExecutionStack!;
    const stackCount = document.querySelectorAll('.execution-stack').length;
    currentSocket.receive({
      type: 'error',
      run_terminal: true,
      code: 'provider_error',
      content: 'Provider request failed.',
    });
    expect(stateModule.state.terminalExecutionStack).toBe(ordinaryErrorStack);
    expect(stateModule.state.activeExecutionRunId).toBe(0);
    expect(stateModule.state.busy).toBe(false);
    expect(document.querySelectorAll('.execution-stack')).toHaveLength(stackCount);

    currentSocket.receive({ type: 'done', phase: 'failed', reason: 'failed' });
    expect(ordinaryErrorStack.dataset.executionStatus).toBe('failed');
    expect(document.querySelectorAll('.execution-stack')).toHaveLength(stackCount);
    expect(stateModule.state.activeExecutionRunId).toBe(0);
  });

  it('treats an error without done as a complete client-side terminal event', () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    currentSocket.receive({ type: 'history', messages: [] });
    currentSocket.receive({ type: 'start', react_visible: true, phase: 'analyze', cycle: 1 });
    currentSocket.receive({
      type: 'tool_call',
      name: 'read_file',
      arguments: '{"path":"README.md"}',
      id: 'error-only-tool',
    });
    const stack = stateModule.state.activeExecutionStack!;

    currentSocket.receive({
      type: 'error',
      run_terminal: true,
      code: 'provider_error',
      content: 'Provider request failed without a done event.',
    });

    expect(document.querySelectorAll('.execution-stack')).toHaveLength(1);
    expect(stack.dataset.executionStatus).toBe('failed');
    expect(stateModule.state.terminalExecutionStack).toBe(stack);
    expect(stateModule.state.activeExecutionRunId).toBe(0);
    expect(stateModule.state.activeExecutionPlanId).toBe('');
    expect(stateModule.state.busy).toBe(false);
    expect(stateModule.state.currentRoundStartedAt).toBe(0);
    expect(stateModule.state.currentRoundFirstTokenAt).toBe(0);
    expect(stateModule.state.reactStatusRow).toBeNull();
    expect(stateModule.state.reactPhaseTimer).toBe(0);
    expect(stateModule.state.reactPhaseQueue).toEqual([]);
    expect(stateModule.state.currentMsg).toBeNull();

    expect(
      document.body.textContent?.split('Provider request failed without a done event.'),
    ).toHaveLength(2);
    const firstOutcome = {
      status: stack.dataset.executionStatus,
      summary: stack.querySelector('.execution-stack-summary')?.textContent,
      recoverySummary: stack.querySelector('.execution-stack-recovery-summary')?.textContent,
      recoveryLabel: stack.querySelector('.execution-stack-recovery-action')?.textContent,
      aria: stack.querySelector('.execution-stack-header')?.getAttribute('aria-label'),
      source: stack.dataset.executionTerminalSource,
      duration: stack.dataset.executionDuration,
    };

    currentSocket.receive({
      type: 'error',
      code: 'plan_already_active',
      content: 'An unrelated idle Plan request was rejected.',
    });

    expect(document.querySelectorAll('.execution-stack')).toHaveLength(1);
    expect(document.querySelectorAll('.msg-row.error')).toHaveLength(1);
    expect(stateModule.state.terminalExecutionStack).toBe(stack);
    expect(stack.dataset.executionStatus).toBe(firstOutcome.status);
    expect(stack.querySelector('.execution-stack-summary')?.textContent).toBe(firstOutcome.summary);
    expect(stack.querySelector('.execution-stack-recovery-summary')?.textContent).toBe(
      firstOutcome.recoverySummary,
    );
    expect(stack.querySelector('.execution-stack-recovery-action')?.textContent).toBe(
      firstOutcome.recoveryLabel,
    );
    expect(stack.querySelector('.execution-stack-header')?.getAttribute('aria-label')).toBe(
      firstOutcome.aria,
    );
    expect(stack.dataset.executionTerminalSource).toBe(firstOutcome.source);
    expect(stack.dataset.executionDuration).toBe(firstOutcome.duration);
    stack.querySelector<HTMLButtonElement>('.execution-stack-recovery-action')?.click();
    expect(stack.contains(document.activeElement)).toBe(true);

    currentSocket.receive({ type: 'system', content: 'Failure details persisted.' });
    currentSocket.receive({ type: 'success', content: 'Cleanup finished.' });
    expect(stateModule.state.terminalExecutionStack).toBe(stack);
    expect(stack.dataset.executionStatus).toBe('failed');
    expect(stateModule.state.activeExecutionRunId).toBe(0);
    expect(stateModule.state.busy).toBe(false);
  });

  it.each([
    {
      label: 'failed',
      planId: 'plan-terminal-failed',
      stepStatus: 'in_progress' as const,
      errorCode: 'plan_completion_contract_failed',
      errorContent: 'The approved completion contract did not pass.',
      doneReason: 'completion_contract_failed',
      expectedStatus: 'failed',
      sendLateDone: false,
    },
    {
      label: 'blocked',
      planId: 'plan-terminal-blocked',
      stepStatus: 'blocked' as const,
      errorCode: 'plan_execution_incomplete',
      errorContent: 'Plan execution ended with unfinished steps.',
      doneReason: 'incomplete_plan',
      expectedStatus: 'blocked',
      sendLateDone: true,
    },
  ])(
    'lets a late Plan $label state claim its error-only terminal stack',
    ({ planId, stepStatus, errorCode, errorContent, doneReason, expectedStatus, sendLateDone }) => {
      const currentSocket = FakeWebSocket.instances.at(-1)!;
      const runIdentity = `${planId}-run`;
      currentSocket.receive({ type: 'history', messages: [] });
      stateModule.state.pendingPlanExecutionId = planId;
      currentSocket.receive({
        type: 'start',
        run_connection_id: runIdentity,
        react_visible: true,
        phase: 'analyze',
        cycle: 1,
      });
      currentSocket.receive({
        type: 'tool_call',
        name: 'update_plan',
        arguments: '{}',
        id: `${planId}-tool`,
      });
      const stack = stateModule.state.activeExecutionStack!;
      const stackCount = document.querySelectorAll('.execution-stack').length;
      expect(stack.dataset.executionPlanId).toBe(planId);

      currentSocket.receive({
        type: 'error',
        run_terminal: true,
        run_connection_id: runIdentity,
        code: errorCode,
        content: errorContent,
      });

      expect(stack.dataset.executionStatus).toBe('failed');
      expect(stateModule.state.activeExecutionStack).toBeNull();
      expect(stateModule.state.terminalExecutionStack).toBe(stack);
      expect(stateModule.state.activeExecutionRunId).toBe(0);
      expect(stateModule.state.activeExecutionPlanId).toBe('');
      expect(stateModule.state.busy).toBe(false);
      expect(document.querySelectorAll('.execution-stack')).toHaveLength(stackCount);

      currentSocket.receive({
        type: 'plan_state',
        plan: terminalPlanPayload(planId, 'failed', stepStatus),
      });

      const planSummary = stack.querySelector('.execution-stack-summary')?.textContent;
      const recoverySummary = stack.querySelector('.execution-stack-recovery-summary')?.textContent;
      expect(stateModule.state.terminalExecutionStack).toBe(stack);
      expect(stateModule.state.activeExecutionRunId).toBe(0);
      expect(document.querySelectorAll('.execution-stack')).toHaveLength(stackCount);
      expect(stack.dataset.executionStatus).toBe(expectedStatus);
      expect(planSummary).toContain(expectedStatus === 'blocked' ? 'blocked' : 'failed');
      expect(recoverySummary).toBe('This run needs attention before continuing.');
      expect(stack.querySelector('.execution-stack-recovery-action')?.textContent).toBe(
        'Continue remaining steps',
      );
      expect(stack.querySelector('.execution-stack-header')?.getAttribute('aria-label')).toContain(
        planSummary,
      );

      const currentPlanCard = Array.from(
        document.querySelectorAll<HTMLElement>('.plan-artifact-card'),
      ).find((card) => card.dataset.planId === planId && card.dataset.historical !== 'true');
      stack.querySelector<HTMLButtonElement>('.execution-stack-recovery-action')?.click();
      expect(document.activeElement).toBe(currentPlanCard);

      if (sendLateDone) {
        currentSocket.receive({
          type: 'done',
          run_connection_id: runIdentity,
          phase: 'failed',
          reason: doneReason,
        });

        expect(document.querySelectorAll('.execution-stack')).toHaveLength(stackCount);
        expect(stack.dataset.executionStatus).toBe(expectedStatus);
        expect(stack.querySelector('.execution-stack-summary')?.textContent).toBe(planSummary);
        expect(stack.querySelector('.execution-stack-recovery-summary')?.textContent).toBe(
          recoverySummary,
        );
        expect(stateModule.state.terminalExecutionStack).toBeNull();
        expect(stateModule.state.activeExecutionRunId).toBe(0);
      }
    },
  );

  it.each([true, false])(
    'round21 updates a waiting Plan after done (ReAct visible: %s)',
    (reactVisible) => {
      const currentSocket = FakeWebSocket.instances.at(-1)!;
      currentSocket.receive({ type: 'history', messages: [] });
      stateModule.state.pendingPlanExecutionId = 'round21-discard';
      currentSocket.receive({
        type: 'start',
        react_visible: reactVisible,
        phase: 'analyze',
        cycle: 1,
      });
      const plan = {
        ...terminalPlanPayload('round21-discard', 'failed', 'in_progress'),
        status: 'needs_input',
        approved_at: null,
        execution_attempt: 0,
        updated_at: 200,
        questions: [{ id: 'choice', question: 'Choose the output?', blocking: true }],
      };
      currentSocket.receive({ type: 'plan_state', plan });
      currentSocket.receive({ type: 'done', phase: 'waiting_user', reason: 'needs_input' });
      const stack = document.querySelector<HTMLElement>('.execution-stack')!;
      expect(stateModule.state.activeExecutionStack).toBeNull();
      expect(stateModule.state.terminalExecutionStack).toBeNull();
      expect(stack.dataset.executionStatus).toBe('waiting_user');
      currentSocket.receive({
        type: 'plan_state',
        plan: { ...plan, status: 'discarded', updated_at: 201 },
      });
      expect(stack.dataset.executionStatus).toBe('discarded');
      expect(stack.querySelector<HTMLElement>('.execution-stack-recovery')?.hidden).toBe(true);
      expect(stack.textContent).not.toContain('Answer Plan');
      currentSocket.receive({ type: 'plan_state', plan });
      expect(stateModule.state.activePlan?.status).toBe('discarded');
      expect(stack.dataset.executionStatus).toBe('discarded');
      // A later unrelated run cannot claim the old Plan's completed stack.
      currentSocket.receive({ type: 'start', react_visible: true, phase: 'analyze', cycle: 1 });
      const newer = stateModule.state.activeExecutionStack!;
      currentSocket.receive({
        type: 'plan_state',
        plan: { ...plan, status: 'discarded', updated_at: 201 },
      });
      expect(newer).not.toBe(stack);
      expect(newer.dataset.executionStatus).toBe('running');
      expect(newer.dataset.executionPlanId).toBeUndefined();
      currentSocket.receive({ type: 'done', phase: 'finish', reason: 'complete' });
    },
  );

  it('round21 counts partial Orchestration tasks once and keeps recovery after the collapse delay', async () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    currentSocket.receive({ type: 'history', messages: [] });
    currentSocket.receive({ type: 'start', react_visible: true, phase: 'analyze', cycle: 1 });
    const args = JSON.stringify({
      tasks: [
        { id: 'ok', agent: 'explore', prompt: 'inspect' },
        { id: 'fail', agent: 'explore', prompt: 'fail' },
      ],
    });
    currentSocket.receive({
      type: 'tool_call',
      name: 'orchestrate',
      id: 'r21-outer',
      arguments: args,
    });
    currentSocket.receive({
      type: 'orchestrate_started',
      orchestrate_id: 'r21-orch',
      parent_tool_call_id: 'r21-outer',
      task_count: 2,
      layer_count: 1,
      tasks: [
        { id: 'ok', agent: 'explore' },
        { id: 'fail', agent: 'explore' },
      ],
    });
    currentSocket.receive({
      type: 'orchestrate_task_completed',
      orchestrate_id: 'r21-orch',
      id: 'ok',
    });
    currentSocket.receive({
      type: 'orchestrate_task_failed',
      orchestrate_id: 'r21-orch',
      id: 'fail',
      error: 'controlled failure',
    });
    currentSocket.receive({
      type: 'orchestrate_completed',
      orchestrate_id: 'r21-orch',
      completed: 1,
      failed: 1,
      skipped: 0,
    });
    currentSocket.receive({
      type: 'tool_result',
      name: 'orchestrate',
      id: 'r21-outer',
      result: 'One task failed',
      is_error: true,
    });
    const stack = stateModule.state.activeExecutionStack!;
    currentSocket.receive({ type: 'done', phase: 'partial', reason: 'unresolved_tool_failures' });
    await new Promise((resolve) => setTimeout(resolve, 650));
    expect(stack.dataset.executionStatus).toBe('partial');
    expect(stack.querySelector('.execution-stack-meta')?.textContent).toContain('1/2 complete');
    expect(stack.querySelector('.execution-stack-meta')?.textContent).toContain('1 unresolved');
    expect(stack.querySelector('.execution-stack-header')?.getAttribute('aria-expanded')).toBe(
      'true',
    );
    stack.querySelector<HTMLButtonElement>('.execution-stack-header')?.click();
    expect(stack.querySelector('.execution-stack-header')?.getAttribute('aria-expanded')).toBe(
      'false',
    );
  });

  it('round21 rejects an old revision discard without changing a later Plan run', () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    currentSocket.receive({ type: 'history', messages: [] });
    const id = 'r21-revisions';
    const stacks: HTMLElement[] = [];
    for (const revision of [2, 3]) {
      stateModule.state.pendingPlanExecutionId = id;
      currentSocket.receive({ type: 'start', react_visible: true, phase: 'analyze', cycle: 1 });
      const plan = {
        ...terminalPlanPayload(id, 'failed', 'in_progress'),
        revision,
        status: 'needs_input',
        execution_attempt: 0,
        approved_at: null,
      };
      currentSocket.receive({ type: 'plan_state', plan });
      stacks.push(document.querySelectorAll<HTMLElement>('.execution-stack')[revision - 2]);
      currentSocket.receive({ type: 'done', phase: 'waiting_user', reason: 'needs_input' });
    }
    currentSocket.receive({
      type: 'plan_state',
      plan: {
        ...terminalPlanPayload(id, 'failed', 'in_progress'),
        status: 'discarded',
        revision: 2,
      },
    });
    expect(stateModule.state.activePlan?.revision).toBe(3);
    expect(stateModule.state.activePlan?.status).toBe('needs_input');
    expect(stacks[1].dataset.executionStatus).toBe('waiting_user');
    currentSocket.receive({
      type: 'plan_state',
      plan: {
        ...terminalPlanPayload(id, 'failed', 'in_progress'),
        status: 'discarded',
        revision: 3,
      },
    });
    expect(stacks[1].dataset.executionStatus).toBe('discarded');
    expect(stacks[0].dataset.executionPlanRevision).toBe('2');
    expect(stacks[0].dataset.executionStatus).toBe('waiting_user');
  });

  it.each([
    ['A.txt', 'completed'],
    ['B.txt', 'partial'],
  ] as const)(
    'round22 keeps intermediate assistant text and same-index tool calls in one %s history run',
    (retryPath, status) => {
      const currentSocket = FakeWebSocket.instances.at(-1)!;
      const messages = [
        { role: 'user', content: 'Inspect files', message_index: 0 },
        {
          role: 'tool_call',
          name: 'read_file',
          arguments: '{"path":"A.txt","end_line":0}',
          id: 'r22-a',
          message_index: 1,
        },
        {
          role: 'tool_result',
          id: 'r22-a',
          result: 'Invalid read range',
          is_error: true,
          message_index: 2,
        },
        { role: 'assistant', content: 'Next I will read the requested file.', message_index: 3 },
        {
          role: 'tool_call',
          name: 'read_file',
          arguments: JSON.stringify({ path: retryPath }),
          id: 'r22-b',
          message_index: 3,
        },
        { role: 'tool_result', id: 'r22-b', result: 'file contents', message_index: 4 },
        {
          role: 'assistant',
          content: 'The checks ended.',
          message_index: 5,
          run_outcomes: [
            {
              session_id: 'main',
              run_id: 'r22-run',
              run_connection_id: 'r22-connection',
              status,
              phase: status === 'completed' ? 'finish' : 'partial',
              reason: status === 'completed' ? 'complete' : 'unresolved_tool_failures',
              duration_ms: 640,
              start_message_index: 0,
              end_message_index: 5,
              started_at: 100,
              finished_at: 101,
            },
          ],
        },
      ];
      for (let refresh = 0; refresh < 2; refresh += 1) {
        currentSocket.receive({ type: 'history', messages });
        const stacks = document.querySelectorAll<HTMLElement>('.execution-stack');
        expect(stacks).toHaveLength(1);
        expect(stacks[0].dataset.executionPersistedRunId).toBe('r22-run');
        expect(stacks[0].dataset.executionStatus).toBe(status);
        expect(stacks[0].querySelectorAll('.tool-panel')).toHaveLength(2);
        expect(stacks[0].textContent).not.toContain('no persisted terminal result');
        expect(stacks[0].querySelectorAll('[data-execution-recovered="true"]')).toHaveLength(
          status === 'completed' ? 1 : 0,
        );
      }
    },
  );

  it('round22 keeps adjacent run scopes separate through load-earlier, in-run user text and truncation', () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    const messages: Record<string, unknown>[] = [];
    for (let run = 0; run < 16; run += 1) {
      const base = run * 6;
      messages.push(
        { role: 'user', content: 'Run ' + run, message_index: base },
        {
          role: 'tool_call',
          name: 'read_file',
          arguments: '{"path":"A.txt"}',
          id: 'page-' + run + '-a',
          message_index: base + 1,
        },
        { role: 'tool_result', id: 'page-' + run + '-a', result: 'A', message_index: base + 2 },
        { role: 'assistant', content: 'Next file', message_index: base + 3 },
        { role: 'user', content: 'Keep going in this run', message_index: base + 4 },
        {
          role: 'tool_call',
          name: 'read_file',
          arguments: '{"path":"B.txt"}',
          id: 'page-' + run + '-b',
          message_index: base + 4,
        },
        {
          role: 'tool_result',
          id: 'page-' + run + '-b',
          result: 'B',
          message_index: base + 5,
          run_outcomes: [
            {
              session_id: 'main',
              run_id: 'page-run-' + run,
              run_connection_id: 'page-connection',
              status: 'completed',
              phase: 'finish',
              reason: 'complete',
              duration_ms: 400,
              start_message_index: base,
              end_message_index: base + 5,
              started_at: 100,
              finished_at: 101,
            },
          ],
        },
      );
    }
    currentSocket.receive({ type: 'history', messages });
    expect(stateModule.state.deferredHistory.length).toBeGreaterThan(0);
    while (stateModule.state.deferredHistory.length) {
      document.querySelector<HTMLButtonElement>('[data-action="load-earlier"]')!.click();
    }
    const stacks = Array.from(document.querySelectorAll<HTMLElement>('.execution-stack'));
    expect(stacks).toHaveLength(16);
    expect(new Set(stacks.map((stack) => stack.dataset.executionPersistedRunId)).size).toBe(16);
    for (const stack of stacks) {
      expect(stack.dataset.executionStatus).toBe('completed');
      expect(stack.querySelectorAll('.tool-panel')).toHaveLength(2);
    }
    // A right-truncated response carries no terminal fact. Never infer success
    // from the intermediate assistant text or successful tool results.
    currentSocket.receive({ type: 'history', messages: messages.slice(0, 4) });
    expect(document.querySelectorAll('.execution-stack')).toHaveLength(1);
    expect(document.querySelector<HTMLElement>('.execution-stack')!.dataset.executionStatus).toBe(
      'incomplete',
    );
    // A server-truncated prefix may still carry an authoritative suffix outcome.
    currentSocket.receive({ type: 'history', messages: messages.slice(3, 7) });
    expect(document.querySelectorAll('.execution-stack')).toHaveLength(1);
    expect(document.querySelector<HTMLElement>('.execution-stack')!.dataset.executionStatus).toBe(
      'completed',
    );
  });

  it('does not let an error-only terminal stack leak into the next run', () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    currentSocket.receive({ type: 'history', messages: [] });
    currentSocket.receive({ type: 'start', react_visible: true, phase: 'analyze', cycle: 1 });
    currentSocket.receive({
      type: 'tool_call',
      name: 'read_file',
      arguments: '{"path":"old.txt"}',
      id: 'old-run-tool',
    });
    const oldStack = stateModule.state.activeExecutionStack!;
    currentSocket.receive({ type: 'error', run_terminal: true, content: 'Old run failed.' });
    const oldSummary = oldStack.querySelector('.execution-stack-summary')?.textContent;

    currentSocket.receive({ type: 'start', react_visible: true, phase: 'analyze', cycle: 1 });
    currentSocket.receive({
      type: 'tool_call',
      name: 'write_file',
      arguments: '{"path":"new.txt"}',
      id: 'new-run-tool',
    });
    const newStack = stateModule.state.activeExecutionStack!;

    expect(newStack).not.toBe(oldStack);
    expect(document.querySelectorAll('.execution-stack')).toHaveLength(2);
    expect(oldStack.dataset.executionStatus).toBe('failed');
    expect(oldStack.querySelector('.execution-stack-summary')?.textContent).toBe(oldSummary);
    expect(oldStack.querySelector('.execution-stack-summary')?.textContent).not.toContain(
      'interrupted',
    );
    expect(stateModule.state.terminalExecutionStack).toBeNull();
    expect(stateModule.state.activeExecutionRunId).toBeGreaterThan(0);
    expect(newStack.dataset.executionClientRunId).not.toBe(oldStack.dataset.executionClientRunId);

    currentSocket.receive({ type: 'done', phase: 'finish', reason: 'complete' });
  });

  it('does not let a mismatched late Plan claim a generic terminal or a newer run', () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    const unrelatedPlan = terminalPlanPayload('unrelated-late-plan', 'failed', 'in_progress');
    currentSocket.receive({ type: 'history', messages: [] });
    currentSocket.receive({ type: 'start', react_visible: true, phase: 'analyze', cycle: 1 });
    currentSocket.receive({
      type: 'tool_call',
      name: 'read_file',
      arguments: '{"path":"ordinary.txt"}',
      id: 'ordinary-mismatch-tool',
    });
    const ordinaryStack = stateModule.state.activeExecutionStack!;
    currentSocket.receive({ type: 'error', run_terminal: true, content: 'Ordinary failure.' });
    const ordinarySummary = ordinaryStack.querySelector('.execution-stack-summary')?.textContent;

    currentSocket.receive({ type: 'plan_state', plan: unrelatedPlan });

    expect(stateModule.state.terminalExecutionStack).toBe(ordinaryStack);
    expect(ordinaryStack.dataset.executionTerminalSource).toBe('error');
    expect(ordinaryStack.dataset.executionPlanId).toBeUndefined();
    expect(ordinaryStack.querySelector('.execution-stack-summary')?.textContent).toBe(
      ordinarySummary,
    );

    currentSocket.receive({ type: 'start', react_visible: true, phase: 'analyze', cycle: 1 });
    currentSocket.receive({
      type: 'tool_call',
      name: 'write_file',
      arguments: '{"path":"new-run.txt"}',
      id: 'new-run-mismatch-tool',
    });
    const newStack = stateModule.state.activeExecutionStack!;
    currentSocket.receive({ type: 'plan_state', plan: unrelatedPlan });

    expect(newStack.dataset.executionStatus).toBe('running');
    expect(newStack.dataset.executionPlanId).toBeUndefined();
    expect(stateModule.state.activeExecutionStack).toBe(newStack);
    expect(document.querySelectorAll('.execution-stack')).toHaveLength(2);

    currentSocket.receive({ type: 'done', phase: 'finish', reason: 'complete' });
  });

  it('drops an error-only terminal association when history replaces the Session view', () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    currentSocket.receive({ type: 'history', messages: [] });
    currentSocket.receive({ type: 'start', react_visible: true, phase: 'analyze', cycle: 1 });
    currentSocket.receive({
      type: 'error',
      run_terminal: true,
      content: 'Session-local failure.',
    });
    expect(stateModule.state.terminalExecutionStack).not.toBeNull();

    currentSocket.receive({ type: 'history', messages: [] });

    expect(stateModule.state.terminalExecutionStack).toBeNull();
    expect(stateModule.state.activeExecutionRunId).toBe(0);
    expect(stateModule.state.activeExecutionPlanId).toBe('');
    expect(document.querySelector('.execution-stack')).toBeNull();
  });

  it('rebuilds a completed historical execution stack from the persisted server outcome', () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    currentSocket.receive({
      type: 'history',
      messages: [
        { role: 'user', content: 'Read the project rules', message_index: 0 },
        {
          role: 'tool_call',
          name: 'read_file',
          arguments: '{"path":"AGENTS.md"}',
          id: 'persisted-read',
          message_index: 1,
        },
        {
          role: 'tool_result',
          id: 'persisted-read',
          result: 'Project rules',
          duration_ms: 12,
          message_index: 2,
        },
        {
          role: 'assistant',
          content: 'The rules were read.',
          message_index: 3,
          run_outcomes: [
            {
              session_id: 'main',
              run_id: 'persisted-complete-run',
              run_connection_id: 'persisted-connection',
              status: 'completed',
              phase: 'finish',
              reason: 'complete',
              duration_ms: 640,
              start_message_index: 0,
              end_message_index: 4,
              started_at: 100,
              finished_at: 101,
            },
          ],
        },
      ],
    });

    const stack = document.querySelector<HTMLElement>('.execution-stack');
    expect(stack?.dataset.executionStatus).toBe('completed');
    expect(stack?.dataset.executionPersistedRunId).toBe('persisted-complete-run');
    expect(stack?.dataset.executionDuration).toBe('640');
    expect(stack?.querySelector('.execution-stack-summary')?.textContent).not.toContain(
      'no persisted terminal result',
    );
    expect(stack?.querySelector('.execution-stack-header')?.getAttribute('aria-expanded')).toBe(
      'false',
    );
  });

  it.each(['failed', 'blocked', 'waiting_user', 'stopped', 'partial', 'incomplete'] as const)(
    'rebuilds a no-step %s historical outcome as a recoverable attention stack',
    (status) => {
      const currentSocket = FakeWebSocket.instances.at(-1)!;
      currentSocket.receive({
        type: 'history',
        messages: [
          {
            role: 'user',
            content: `Historical ${status} run`,
            message_index: 0,
            run_outcomes: [
              {
                session_id: 'main',
                run_id: `persisted-${status}-run`,
                run_connection_id: `persisted-${status}-connection`,
                status,
                phase: status,
                reason: status,
                duration_ms: 25,
                start_message_index: 0,
                end_message_index: 1,
                started_at: 100,
                finished_at: 101,
              },
            ],
          },
        ],
      });

      const stack = document.querySelector<HTMLElement>('.execution-stack');
      expect(stack?.dataset.executionStatus).toBe(status);
      expect(stack?.dataset.executionPersistedRunId).toBe(`persisted-${status}-run`);
      expect(stack?.querySelector<HTMLElement>('.execution-stack-recovery')?.hidden).toBe(false);
      expect(stack?.querySelector('.execution-stack-header')?.getAttribute('aria-expanded')).toBe(
        'true',
      );
    },
  );

  it('does not create an empty stack for a persisted no-step success', () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    currentSocket.receive({
      type: 'history',
      messages: [
        {
          role: 'assistant',
          content: 'A direct response completed.',
          message_index: 0,
          run_outcomes: [
            {
              session_id: 'main',
              run_id: 'persisted-no-step-success',
              run_connection_id: 'persisted-no-step-connection',
              status: 'completed',
              phase: 'finish',
              reason: 'complete',
              duration_ms: 10,
              start_message_index: 0,
              end_message_index: 1,
              started_at: 100,
              finished_at: 101,
            },
          ],
        },
      ],
    });

    expect(document.querySelector('.execution-stack')).toBeNull();
  });

  it.each([
    ['provider_authentication', 'Provider authentication failed.'],
    ['provider_rate_limited', 'Provider rate limit reached.'],
    ['provider_unavailable', 'Model provider unavailable.'],
    ['model_configuration', 'Model configuration could not form a request.'],
    ['context_budget_exceeded', 'Model context budget exceeded.'],
  ])('round23 retains safe %s failure details across live and history', (code, summary) => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    const diagnostic = { code, body: '<script>PRIVATE_PROVIDER_TOKEN</script>'.repeat(1000) };
    currentSocket.receive({ type: 'history', messages: [] });
    currentSocket.receive({ type: 'start', run_id: `diagnostic-${code}`, react_visible: false });
    currentSocket.receive({
      type: 'error',
      run_terminal: true,
      code,
      diagnostic,
      content: 'PRIVATE_PROVIDER_TOKEN https://user:secret@example.test',
    });
    expect(document.querySelector('.execution-stack-summary')?.textContent).toBe(summary);
    expect(document.getElementById('chat')?.innerHTML).not.toContain('PRIVATE_PROVIDER_TOKEN');
    currentSocket.receive({
      type: 'history',
      messages: [
        {
          role: 'user',
          content: 'Try the configured model',
          message_index: 0,
          run_outcomes: [
            {
              session_id: 'main',
              run_id: `diagnostic-${code}`,
              run_connection_id: 'diagnostic-connection',
              status: 'failed',
              phase: 'failed',
              reason: code,
              diagnostic,
              start_message_index: 0,
              end_message_index: 0,
              duration_ms: 50,
            },
          ],
        },
      ],
    });
    const stack = document.querySelector<HTMLElement>('.execution-stack');
    expect(stack?.dataset.executionDiagnosticCode).toBe(code);
    expect(stack?.querySelector('.execution-stack-summary')?.textContent).toBe(summary);
    stack?.querySelector<HTMLButtonElement>('[data-action="execution-recovery"]')?.click();
    const details = stack?.querySelector<HTMLDetailsElement>('.execution-stack-diagnostic');
    expect(details?.open).toBe(true);
    expect(document.activeElement).toBe(
      details?.querySelector('.execution-stack-diagnostic-content'),
    );
    expect(
      details?.querySelector('.execution-stack-diagnostic-content')?.textContent?.length,
    ).toBeGreaterThan(20);
    expect(stack?.querySelectorAll('[aria-live="polite"]')).toHaveLength(1);
    expect(stack?.querySelector('[aria-live="polite"]')?.textContent?.split(summary)).toHaveLength(
      2,
    );
    expect(document.getElementById('chat')?.innerHTML).not.toContain('PRIVATE_PROVIDER_TOKEN');
    expect(document.querySelectorAll('.execution-stack')).toHaveLength(1);
  });

  it('round23 loads the exact deferred Plan revision when its terminal recovery is clicked', async () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    const plan = {
      ...terminalPlanPayload('round23-deferred', 'failed', 'in_progress'),
      message_index: 1,
    };
    const messages: Record<string, unknown>[] = [
      { role: 'user', content: 'Create a Plan', message_index: 0 },
      { role: 'assistant', content: 'The approved Plan', message_index: 1 },
      ...Array.from({ length: 90 }, (_, index) => ({
        role: 'user',
        content: `Other turn ${index}`,
        message_index: index + 2,
      })),
      { role: 'user', content: 'Execute', message_index: 92 },
      {
        role: 'tool_call',
        name: 'read_file',
        arguments: '{"path":"plan.txt"}',
        id: 'round23-plan-tool',
        message_index: 93,
      },
      {
        role: 'tool_result',
        id: 'round23-plan-tool',
        result: 'ok',
        message_index: 94,
        run_outcomes: [
          {
            session_id: 'main',
            run_id: 'round23-deferred-run',
            run_connection_id: 'round23-deferred-connection',
            status: 'incomplete',
            phase: 'hard_cap',
            reason: 'hard_cap',
            start_message_index: 92,
            end_message_index: 94,
            plan_id: plan.plan_id,
            plan_revision: 2,
          },
        ],
      },
    ];
    currentSocket.receive({ type: 'history', messages, plans: [plan] });
    await vi.waitFor(() => expect(stateModule.state.activePlan?.plan_id).toBe(plan.plan_id));
    expect(document.querySelector(`[data-plan-id="${plan.plan_id}"]`)).toBeNull();
    expect(stateModule.state.deferredHistory.length).toBeGreaterThan(0);
    const recovery = document.querySelector<HTMLButtonElement>('.execution-stack-recovery-action');
    recovery?.click();
    await vi.waitFor(() => {
      const card = document.querySelector<HTMLElement>(
        `.plan-artifact-card[data-plan-id="${plan.plan_id}"][data-plan-revision="2"]`,
      );
      expect(card).not.toBeNull();
      expect(card === document.activeElement || card?.contains(document.activeElement)).toBe(true);
    });
    expect(document.querySelectorAll('.execution-stack')).toHaveLength(1);
  });

  it('round23 reconciles a discarded historical Plan after another Plan and paged replay', async () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    const oldPlan = {
      ...terminalPlanPayload('round23-old-discarded', 'failed', 'in_progress'),
      status: 'discarded',
      historical: true,
      message_index: 1,
    };
    const basePlan = terminalPlanPayload('round23-new-plan', 'failed', 'in_progress');
    const currentPlan = {
      ...basePlan,
      artifact: { ...basePlan.artifact, acceptance_criteria: ['Keep this Plan separate'] },
      message_index: 93,
    };
    const messages: Record<string, unknown>[] = [
      { role: 'user', content: 'Old Plan', message_index: 0 },
      {
        role: 'assistant',
        content: 'Old questions',
        message_index: 1,
        run_outcomes: [
          {
            session_id: 'main',
            run_id: 'round23-old-run',
            run_connection_id: 'round23-old-connection',
            status: 'waiting_user',
            phase: 'waiting_user',
            reason: 'needs_input',
            start_message_index: 0,
            end_message_index: 1,
            plan_id: oldPlan.plan_id,
            plan_revision: 2,
          },
        ],
      },
      ...Array.from({ length: 90 }, (_, index) => ({
        role: 'user',
        content: `Other turn ${index}`,
        message_index: index + 2,
      })),
      { role: 'user', content: 'New Plan', message_index: 92 },
      {
        role: 'assistant',
        content: 'New Plan result',
        message_index: 93,
        run_outcomes: [
          {
            session_id: 'main',
            run_id: 'round23-new-run',
            run_connection_id: 'round23-new-connection',
            status: 'failed',
            phase: 'failed',
            reason: 'incomplete_plan',
            start_message_index: 92,
            end_message_index: 93,
            plan_id: currentPlan.plan_id,
            plan_revision: 2,
          },
        ],
      },
    ];
    currentSocket.receive({ type: 'history', messages, plans: [oldPlan, currentPlan] });
    await vi.waitFor(() => expect(stateModule.state.activePlan?.plan_id).toBe(currentPlan.plan_id));
    const contract = document.querySelector<HTMLDetailsElement>(
      `.plan-artifact-card[data-plan-id="${currentPlan.plan_id}"] .plan-contract-details`,
    );
    if (!contract) throw new Error('Current Plan contract is missing');
    contract.open = false;
    document
      .querySelector<HTMLButtonElement>(
        '[data-execution-persisted-run-id="round23-new-run"] [data-action="toggle-execution-stack"]',
      )
      ?.click();
    while (stateModule.state.deferredHistory.length)
      document.querySelector<HTMLButtonElement>('[data-action="load-earlier"]')?.click();
    await vi.waitFor(() => {
      const old = document.querySelector<HTMLElement>(
        '[data-execution-persisted-run-id="round23-old-run"]',
      );
      expect(old?.dataset.executionStatus).toBe('discarded');
      expect(old?.textContent).not.toMatch(/Your input is required|Answer Plan|Review details/);
    });
    expect(
      document.querySelector<HTMLElement>('[data-execution-persisted-run-id="round23-new-run"]')
        ?.dataset.executionStatus,
    ).toBe('failed');
    expect(
      document
        .querySelector<HTMLElement>('[data-execution-persisted-run-id="round23-new-run"]')
        ?.classList.contains('is-expanded'),
    ).toBe(false);
    expect(
      document.querySelector<HTMLDetailsElement>(
        `.plan-artifact-card[data-plan-id="${currentPlan.plan_id}"] .plan-contract-details`,
      )?.open,
    ).toBe(false);
  });

  it('round23 counts a task once when its random task identity differs from its parent call', () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    currentSocket.receive({ type: 'history', messages: [] });
    currentSocket.receive({ type: 'start', run_id: 'round23-task', react_visible: false });
    currentSocket.receive({
      type: 'tool_call',
      name: 'task',
      id: 'parent-call-23',
      arguments: '{"agent":"explore","prompt":"Inspect"}',
    });
    currentSocket.receive({
      type: 'task_started',
      task_id: 'random-task-23',
      parent_tool_call_id: 'parent-call-23',
      agent: 'explore',
      prompt: 'Inspect',
    });
    currentSocket.receive({
      type: 'task_completed',
      task_id: 'random-task-23',
      agent: 'explore',
      result_preview: 'Done',
      tool_calls: 0,
      cycles: 1,
    });
    currentSocket.receive({
      type: 'tool_result',
      name: 'task',
      id: 'parent-call-23',
      result: 'Done',
    });
    currentSocket.receive({ type: 'done', phase: 'finish', reason: 'complete', tool_calls: 1 });
    const stack = document.querySelector<HTMLElement>('.execution-stack');
    expect(stack?.querySelector('.execution-stack-meta')?.textContent).toContain('1/1');
  });

  it('round23 keeps concurrent same-Agent task parents separate through failure and recovery', () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    currentSocket.receive({ type: 'history', messages: [] });
    currentSocket.receive({ type: 'start', run_id: 'round23-concurrent', react_visible: false });
    const start = (index: number, target: number) => {
      currentSocket.receive({
        type: 'tool_call',
        name: 'task',
        id: `parent-${index}`,
        arguments: JSON.stringify({ agent: 'explore', prompt: `Inspect target ${target}` }),
      });
      currentSocket.receive({
        type: 'task_started',
        task_id: `random-${index}`,
        parent_tool_call_id: `parent-${index}`,
        agent: 'explore',
        prompt: `Inspect target ${target}`,
      });
    };
    const finish = (index: number, success: boolean) => {
      currentSocket.receive({
        type: success ? 'task_completed' : 'task_failed',
        task_id: `random-${index}`,
        agent: 'explore',
        cycles: 1,
        tool_calls: 0,
        error: success ? undefined : 'Synthetic task failure',
      });
      currentSocket.receive({
        type: 'tool_result',
        id: `parent-${index}`,
        name: 'task',
        result: success ? 'Done' : 'Synthetic task failure',
        is_error: !success,
      });
    };
    [0, 1, 2].forEach((index) => start(index, index));
    finish(2, true);
    finish(0, false);
    finish(1, true);
    const stack = document.querySelector<HTMLElement>('.execution-stack');
    for (const index of [0, 1, 2]) {
      expect(
        stack?.querySelector<HTMLElement>(`.subagent-panel[data-task-id="random-${index}"]`)
          ?.dataset.executionDelegateToolId,
      ).toBe(`parent-${index}`);
    }
    expect(stack?.querySelector('.execution-stack-meta')?.textContent).toContain('2/3');
    start(3, 0);
    finish(3, true);
    currentSocket.receive({ type: 'done', phase: 'finish', reason: 'complete', tool_calls: 4 });
    expect(stack?.querySelector('.execution-stack-meta')?.textContent).toContain('4/4');
    expect(stack?.dataset.executionStatus).toBe('completed');
    expect(stack?.querySelectorAll('.tool-panel')).toHaveLength(4);
    expect(stack?.querySelectorAll('.subagent-panel')).toHaveLength(4);
    expect(stack?.querySelector('.execution-stack-meta')?.textContent).toContain(
      'Recovered after 1 retry',
    );
  });

  it('lets persisted Plan history enrich the exact historical terminal stack', async () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    const planId = 'persisted-plan-failure';
    currentSocket.receive({
      type: 'history',
      messages: [
        { role: 'user', content: 'Execute the Plan', message_index: 0 },
        {
          role: 'tool_call',
          name: 'update_plan',
          arguments: '{}',
          id: 'persisted-plan-tool',
          message_index: 1,
        },
        {
          role: 'tool_result',
          id: 'persisted-plan-tool',
          result: 'The step remained blocked.',
          is_error: true,
          message_index: 2,
          run_outcomes: [
            {
              session_id: 'main',
              run_id: 'persisted-plan-run',
              run_connection_id: 'persisted-plan-connection',
              status: 'failed',
              phase: 'failed',
              reason: 'incomplete_plan',
              duration_ms: 90,
              start_message_index: 0,
              end_message_index: 2,
              plan_id: planId,
              plan_revision: 2,
              started_at: 100,
              finished_at: 101,
            },
          ],
        },
      ],
      plans: [terminalPlanPayload(planId, 'failed', 'blocked')],
    });

    await vi.waitFor(() => {
      const stack = document.querySelector<HTMLElement>(
        `.execution-stack[data-execution-plan-id="${planId}"]`,
      );
      expect(stack?.dataset.executionTerminalSource).toBe('plan');
      expect(stack?.dataset.executionStatus).toBe('blocked');
      expect(stack?.querySelector('.execution-stack-summary')?.textContent).toContain('blocked');
      expect(stack?.querySelector('.execution-stack-recovery-action')?.textContent).toBe(
        'Continue remaining steps',
      );
    });
  });

  it.each([
    { phase: 'failed', reason: 'provider_error' },
    { phase: 'hard_cap', reason: 'hard_cap' },
  ])(
    'ignores a late $phase done after terminal error and history reset while retaining usage totals',
    ({ phase, reason }) => {
      const currentSocket = FakeWebSocket.instances.at(-1)!;
      const oldRunIdentity = `history-reset-${phase}`;
      currentSocket.receive({ type: 'history', messages: [] });
      currentSocket.receive({
        type: 'start',
        run_connection_id: oldRunIdentity,
        react_visible: true,
        phase: 'analyze',
        cycle: 1,
      });
      currentSocket.receive({
        type: 'tool_call',
        name: 'read_file',
        arguments: '{"path":"old.txt"}',
        id: `${oldRunIdentity}-tool`,
      });
      currentSocket.receive({
        type: 'error',
        run_terminal: true,
        run_connection_id: oldRunIdentity,
        content: 'The old run ended before the reconnect.',
      });
      expect(stateModule.state.terminalExecutionStack).not.toBeNull();

      currentSocket.receive({ type: 'history', messages: [] });
      expect(stateModule.state.activeExecutionRunId).toBe(0);
      expect(stateModule.state.terminalExecutionStack).toBeNull();
      expect(document.querySelectorAll('.execution-stack')).toHaveLength(0);

      currentSocket.receive({
        type: 'done',
        run_connection_id: oldRunIdentity,
        phase,
        reason,
        daily_input_tokens: 401,
        daily_output_tokens: 402,
        total_input_tokens: 403,
        total_output_tokens: 404,
      });

      expect(document.querySelectorAll('.execution-stack')).toHaveLength(0);
      expect(stateModule.state.activeExecutionRunId).toBe(0);
      expect(stateModule.state.terminalExecutionStack).toBeNull();
      expect(stateModule.state.busy).toBe(false);
      expect(stateModule.state.dailyInputTokens).toBe(401);
      expect(stateModule.state.dailyOutputTokens).toBe(402);
      expect(stateModule.state.totalInputTokens).toBe(403);
      expect(stateModule.state.totalOutputTokens).toBe(404);
    },
  );

  it('does not let an old connection done complete a new run after reconnect', () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    const oldRunIdentity = 'reconnected-old-run';
    const newRunIdentity = 'reconnected-new-run';
    currentSocket.receive({ type: 'history', messages: [] });
    currentSocket.receive({
      type: 'start',
      run_connection_id: oldRunIdentity,
      react_visible: true,
      phase: 'analyze',
      cycle: 1,
    });
    currentSocket.receive({
      type: 'error',
      run_terminal: true,
      run_connection_id: oldRunIdentity,
      content: 'The previous connection failed.',
    });
    currentSocket.receive({ type: 'history', messages: [] });

    currentSocket.receive({
      type: 'start',
      run_connection_id: newRunIdentity,
      react_visible: true,
      phase: 'analyze',
      cycle: 1,
    });
    currentSocket.receive({
      type: 'tool_call',
      name: 'write_file',
      arguments: '{"path":"new.txt"}',
      id: 'new-connection-tool',
    });
    const newStack = stateModule.state.activeExecutionStack!;
    const newClientRunId = stateModule.state.activeExecutionRunId;

    currentSocket.receive({
      type: 'done',
      run_connection_id: oldRunIdentity,
      phase: 'failed',
      reason: 'provider_error',
      daily_input_tokens: 501,
      daily_output_tokens: 502,
      total_input_tokens: 503,
      total_output_tokens: 504,
    });

    expect(stateModule.state.activeExecutionStack).toBe(newStack);
    expect(stateModule.state.activeExecutionRunId).toBe(newClientRunId);
    expect(stateModule.state.activeExecutionServerRunId).toBe(newRunIdentity);
    expect(stateModule.state.busy).toBe(true);
    expect(newStack.dataset.executionStatus).toBe('running');
    expect(stateModule.state.dailyInputTokens).toBe(501);

    currentSocket.receive({
      type: 'done',
      run_connection_id: newRunIdentity,
      phase: 'finish',
      reason: 'complete',
    });
    expect(newStack.dataset.executionStatus).toBe('completed');
    expect(stateModule.state.activeExecutionRunId).toBe(0);
    expect(stateModule.state.activeExecutionServerRunId).toBe('');
    expect(stateModule.state.busy).toBe(false);
  });

  it('keeps a stopped Plan in the same stack through the final done event', () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    const planId = 'plan-terminal-stopped';
    currentSocket.receive({ type: 'history', messages: [] });
    stateModule.state.pendingPlanExecutionId = planId;
    currentSocket.receive({ type: 'start', react_visible: true, phase: 'analyze', cycle: 1 });
    currentSocket.receive({
      type: 'tool_call',
      name: 'write_file',
      arguments: '{"path":"result.txt"}',
      id: `${planId}-tool`,
    });
    const stack = stateModule.state.activeExecutionStack!;
    const stackCount = document.querySelectorAll('.execution-stack').length;

    currentSocket.receive({
      type: 'plan_state',
      plan: terminalPlanPayload(planId, 'stopped', 'in_progress'),
    });
    const planSummary = stack.querySelector('.execution-stack-summary')?.textContent;
    expect(stateModule.state.terminalExecutionStack).toBe(stack);
    expect(stack.dataset.executionStatus).toBe('stopped');

    currentSocket.receive({ type: 'done', phase: 'stopped', reason: 'user_stop' });

    expect(document.querySelectorAll('.execution-stack')).toHaveLength(stackCount);
    expect(stack.dataset.executionStatus).toBe('stopped');
    expect(stack.querySelector('.execution-stack-summary')?.textContent).toBe(planSummary);
    expect(stack.querySelector<HTMLElement>('.execution-stack-recovery')?.hidden).toBe(false);
    expect(stateModule.state.activeExecutionRunId).toBe(0);
  });

  it('marks a replayed process without persisted done metadata as incomplete', async () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    currentSocket.receive({
      type: 'history',
      messages: [
        {
          role: 'tool_call',
          name: 'read_file',
          arguments: '{"path":"README.md"}',
          id: 'history-tool',
        },
        {
          role: 'tool_result',
          id: 'history-tool',
          result: 'README contents',
          is_error: false,
        },
        {
          role: 'assistant',
          content: 'Historical response',
          message_index: 3,
        },
      ],
    });

    await vi.waitFor(() =>
      expect(
        document.querySelector('.execution-stack')?.getAttribute('data-execution-status'),
      ).toBe('incomplete'),
    );
    expect(document.querySelectorAll('.execution-stack')).toHaveLength(1);
    expect(document.querySelector('.execution-stack-summary')?.textContent).toContain(
      'no persisted terminal result',
    );
  });

  it('applies the sticky storage protection event to workspace write controls', () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    currentSocket.onopen?.();
    currentSocket.receive({
      type: 'group',
      id: 'storage-group',
      name: 'Storage group',
      members: ['worker-a'],
      model_configured_members: ['worker-a'],
      model_member_ids: ['worker-a'],
      configRevision: 100,
    });
    currentSocket.receive({ type: 'group_history', messages: [], runs: [] });
    stateModule.state.groupsEnabled = true;
    stateModule.state.activeGroupId = 'storage-group';
    stateModule.state.activeGroupMembers = ['worker-a'];
    stateModule.state.activeGroupMemberDetails = [
      { id: 'main', name: 'Main', role: 'owner' },
      { id: 'worker-a', name: 'Worker A', role: 'member' },
    ];
    stateModule.state.composerModelAvailability = 'ready';
    stateModule.state.composerEffectiveModelConfigured = true;
    stateModule.state.composerSessionIdentityPending = false;
    stateModule.state.sessionSwitchInFlight = false;
    stateModule.state.sessionIdentityMutationInFlight = false;
    stateModule.state.busy = true;
    stateModule.state.pendingDeleteSessionId = 'worker-a';
    stateModule.state.activeGroupRunIds.add('run-storage-protected');
    if (stateModule.dom.stopBtn) {
      stateModule.dom.stopBtn.style.display = 'flex';
      stateModule.dom.stopBtn.disabled = false;
    }
    const previousStackCount = document.querySelectorAll('.execution-stack').length;
    const previousTerminalStack = stateModule.state.terminalExecutionStack;
    expect(stateModule.state.activeExecutionRunId).toBe(0);

    currentSocket.receive({
      type: 'storage_status',
      storage: { mode: 'protected', code: 'storage_protected' },
    });

    expect(stateModule.state.storageMode).toBe('protected');
    expect(document.documentElement.dataset.storageMode).toBe('protected');
    expect(stateModule.state.busy).toBe(false);
    expect(stateModule.state.pendingDeleteSessionId).toBe('');
    expect(stateModule.state.activeGroupRunIds.size).toBe(0);
    expect(document.querySelectorAll('.execution-stack')).toHaveLength(previousStackCount);
    expect(stateModule.state.terminalExecutionStack).toBe(previousTerminalStack);
    expect(stateModule.dom.stopBtn?.disabled).toBe(true);
    expect(stateModule.dom.sendBtn?.disabled).toBe(true);
    expect(stateModule.dom.sessionDrawerNewBtn?.disabled).toBe(true);
    expect(stateModule.dom.input?.placeholder).toContain('protected mode');
    expect(stateModule.dom.sendBtn?.getAttribute('aria-disabled')).toBe('true');
    expect(
      document.querySelector<HTMLButtonElement>(
        'button[data-action="cmd-close-menu"][data-cmd="/clear"]',
      )?.disabled,
    ).toBe(true);
    expect(
      document.querySelector<HTMLButtonElement>(
        '.group-member-menu-trigger[data-session-id="worker-a"]',
      )?.disabled,
    ).toBe(true);
  });

  it('uses the exact active client run when storage protection arrives before any process step', () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    currentSocket.receive({ type: 'storage_status', storage: { mode: 'healthy' } });
    stateModule.state.activeGroupId = '';
    currentSocket.receive({ type: 'history', messages: [] });
    currentSocket.receive({ type: 'start', react_visible: false, phase: 'analyze', cycle: 1 });
    const clientRunId = stateModule.state.activeExecutionRunId;
    expect(clientRunId).toBeGreaterThan(0);
    expect(document.querySelector('.execution-stack')).toBeNull();

    currentSocket.receive({
      type: 'storage_status',
      storage: { mode: 'protected', code: 'storage_protected' },
    });

    const stack = stateModule.state.terminalExecutionStack;
    expect(document.querySelectorAll('.execution-stack')).toHaveLength(1);
    expect(stack?.dataset.executionClientRunId).toBe(String(clientRunId));
    expect(stack?.dataset.executionStatus).toBe('failed');
    expect(stack?.dataset.executionTerminalSource).toBe('error');
    expect(stack?.querySelector('.execution-stack-summary')?.textContent).toContain(
      'protected mode',
    );
    expect(stateModule.state.activeExecutionRunId).toBe(0);
    expect(stateModule.state.activeExecutionPlanId).toBe('');
    expect(stateModule.state.busy).toBe(false);
    expect(stateModule.state.reactStatusRow).toBeNull();
    expect(stateModule.state.currentRoundStartedAt).toBe(0);
  });

  it('keeps a protected Plan run in one stack through late Plan, error, and done events', () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    const planId = 'storage-protected-plan';
    currentSocket.receive({ type: 'storage_status', storage: { mode: 'healthy' } });
    currentSocket.receive({ type: 'history', messages: [] });
    stateModule.state.pendingPlanExecutionId = planId;
    currentSocket.receive({ type: 'start', react_visible: true, phase: 'analyze', cycle: 1 });
    currentSocket.receive({
      type: 'tool_call',
      name: 'write_file',
      arguments: '{"path":"result.txt"}',
      id: 'storage-protected-plan-tool',
    });
    const stack = stateModule.state.activeExecutionStack!;
    const clientRunId = stateModule.state.activeExecutionRunId;
    const stackCount = document.querySelectorAll('.execution-stack').length;

    currentSocket.receive({
      type: 'storage_status',
      storage: { mode: 'protected', code: 'storage_protected' },
    });

    expect(stateModule.state.terminalExecutionStack).toBe(stack);
    expect(stack.dataset.executionClientRunId).toBe(String(clientRunId));
    expect(stack.dataset.executionStatus).toBe('failed');
    expect(stateModule.state.activeExecutionRunId).toBe(0);

    currentSocket.receive({
      type: 'plan_state',
      plan: terminalPlanPayload(planId, 'failed', 'blocked'),
    });
    const planSummary = stack.querySelector('.execution-stack-summary')?.textContent;
    expect(stack.dataset.executionStatus).toBe('blocked');
    expect(stack.dataset.executionTerminalSource).toBe('plan');
    expect(stateModule.state.terminalExecutionStack).toBe(stack);
    expect(document.querySelectorAll('.execution-stack')).toHaveLength(stackCount);

    currentSocket.receive({
      type: 'error',
      run_terminal: true,
      code: 'storage_error',
      content: 'The run also reported a terminal storage failure.',
    });
    currentSocket.receive({ type: 'done', phase: 'failed', reason: 'incomplete_plan' });

    expect(document.querySelectorAll('.execution-stack')).toHaveLength(stackCount);
    expect(stack.dataset.executionStatus).toBe('blocked');
    expect(stack.dataset.executionTerminalSource).toBe('plan');
    expect(stack.querySelector('.execution-stack-summary')?.textContent).toBe(planSummary);
    expect(stack.querySelector('.execution-stack-recovery-action')?.textContent).toBe(
      'Continue remaining steps',
    );
    expect(stack.querySelector('.execution-stack-header')?.getAttribute('aria-label')).toContain(
      planSummary,
    );
    expect(stateModule.state.activeExecutionRunId).toBe(0);
    expect(stateModule.state.terminalExecutionStack).toBeNull();
  });

  it.each(['Plan action', 'Session transition', 'Group run'])(
    'does not create or rewrite a stack for non-run busy state: %s',
    (busyKind) => {
      const currentSocket = FakeWebSocket.instances.at(-1)!;
      currentSocket.receive({ type: 'storage_status', storage: { mode: 'healthy' } });
      stateModule.state.activeGroupId = '';
      stateModule.state.sessionSwitchInFlight = false;
      stateModule.state.pendingPlanExecutionId = '';
      stateModule.state.activeGroupRunIds.clear();
      currentSocket.receive({ type: 'history', messages: [] });
      currentSocket.receive({ type: 'start', react_visible: true, phase: 'analyze', cycle: 1 });
      currentSocket.receive({
        type: 'error',
        run_terminal: true,
        code: 'provider_error',
        content: 'Prior run terminal outcome.',
      });
      const terminalStack = stateModule.state.terminalExecutionStack!;
      const terminalSummary = terminalStack.querySelector('.execution-stack-summary')?.textContent;
      const terminalAria = terminalStack
        .querySelector('.execution-stack-header')
        ?.getAttribute('aria-label');
      const stackCount = document.querySelectorAll('.execution-stack').length;

      stateModule.state.busy = true;
      if (busyKind === 'Plan action') stateModule.state.pendingPlanExecutionId = 'pending-action';
      if (busyKind === 'Session transition') stateModule.state.sessionSwitchInFlight = true;
      if (busyKind === 'Group run') {
        stateModule.state.activeGroupId = 'storage-group';
        stateModule.state.activeGroupRunIds.add('group-run');
      }

      currentSocket.receive({
        type: 'storage_status',
        storage: { mode: 'protected', code: 'storage_protected' },
      });

      expect(stateModule.state.activeExecutionRunId).toBe(0);
      expect(stateModule.state.busy).toBe(false);
      expect(document.querySelectorAll('.execution-stack')).toHaveLength(stackCount);
      expect(stateModule.state.terminalExecutionStack).toBe(terminalStack);
      expect(terminalStack.dataset.executionStatus).toBe('failed');
      expect(terminalStack.querySelector('.execution-stack-summary')?.textContent).toBe(
        terminalSummary,
      );
      expect(
        terminalStack.querySelector('.execution-stack-header')?.getAttribute('aria-label'),
      ).toBe(terminalAria);
    },
  );

  it('does not restore plans from an obsolete deferred history render', () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    const originalRequestAnimationFrame = globalThis.requestAnimationFrame;
    const pendingFrames: FrameRequestCallback[] = [];
    globalThis.requestAnimationFrame = ((callback: FrameRequestCallback) => {
      pendingFrames.push(callback);
      return pendingFrames.length;
    }) as typeof requestAnimationFrame;

    try {
      stateModule.state.activeGroupId = '';
      stateModule.state.activeSessionId = 'main';
      currentSocket.receive({
        type: 'history',
        messages: [
          {
            role: 'assistant',
            content: 'Obsolete plan body',
            message_index: 2,
            timestamp: 1710000000,
          },
        ],
        plans: [
          {
            plan_id: 'obsolete-plan',
            revision: 1,
            status: 'ready',
            message_index: 2,
            created_at: 1710000000,
            updated_at: 1710000001,
            artifact: {
              title: 'Obsolete plan',
              goal: 'Must not be restored',
              steps: [{ id: 'inspect', title: 'Inspect' }],
            },
            progress: [{ id: 'inspect', title: 'Inspect', status: 'pending' }],
          },
        ],
      });
      expect(pendingFrames.length).toBeGreaterThan(0);

      currentSocket.receive({ type: 'history', messages: [], plans: [] });
      for (const callback of pendingFrames.splice(0)) callback(0);

      expect(stateModule.state.activePlan).toBeNull();
      expect(stateModule.state.planHistory).toEqual([]);
      expect(document.querySelector('.plan-artifact-card')).toBeNull();
      expect(stateModule.state.bulkRenderingChat).toBe(false);
    } finally {
      globalThis.requestAnimationFrame = originalRequestAnimationFrame;
    }
  });

  it('does not let deferred history overwrite a newer live plan state', () => {
    const currentSocket = FakeWebSocket.instances.at(-1)!;
    const originalRequestAnimationFrame = globalThis.requestAnimationFrame;
    const pendingFrames: FrameRequestCallback[] = [];
    globalThis.requestAnimationFrame = ((callback: FrameRequestCallback) => {
      pendingFrames.push(callback);
      return pendingFrames.length;
    }) as typeof requestAnimationFrame;

    try {
      stateModule.state.activeGroupId = '';
      stateModule.state.activeSessionId = 'main';
      currentSocket.receive({
        type: 'history',
        messages: [
          {
            role: 'assistant',
            content: 'Plan body',
            message_index: 2,
            timestamp: 1710000000,
          },
        ],
        plans: [
          {
            plan_id: 'live-plan',
            revision: 2,
            status: 'ready',
            message_index: 2,
            created_at: 1710000000,
            updated_at: 1710000001,
            artifact: {
              title: 'Older history plan',
              goal: 'Do not restore this state',
              steps: [{ id: 'implement', title: 'Implement' }],
            },
            progress: [{ id: 'implement', title: 'Implement', status: 'pending' }],
          },
        ],
      });
      expect(pendingFrames.length).toBeGreaterThan(0);

      currentSocket.receive({
        type: 'plan_state',
        plan: {
          plan_id: 'live-plan',
          revision: 2,
          status: 'executing',
          message_index: 2,
          created_at: 1710000000,
          updated_at: 1710000002,
          approved_at: 1710000002,
          execution_attempt: 1,
          artifact: {
            title: 'Current live plan',
            goal: 'Keep the live state',
            steps: [{ id: 'implement', title: 'Implement' }],
          },
          progress: [{ id: 'implement', title: 'Implement', status: 'in_progress' }],
        },
      });
      for (const callback of pendingFrames.splice(0)) callback(0);

      expect(stateModule.state.activePlan?.status).toBe('executing');
      expect(stateModule.state.activePlan?.artifact.title).toBe('Current live plan');
      expect(stateModule.state.activePlan?.progress[0]?.status).toBe('in_progress');
      expect(document.querySelector('.plan-artifact-card')?.textContent).toContain(
        'Current live plan',
      );
      expect(stateModule.state.bulkRenderingChat).toBe(false);
    } finally {
      globalThis.requestAnimationFrame = originalRequestAnimationFrame;
    }
  });
});
