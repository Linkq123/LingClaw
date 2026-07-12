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

  constructor(url: string) {
    this.url = url;
    FakeWebSocket.instances.push(this);
  }

  send(): void {}

  close(): void {
    this.readyState = WebSocket.CLOSED;
  }

  receive(payload: unknown): void {
    this.onmessage?.(
      new MessageEvent('message', {
        data: JSON.stringify(payload),
      }),
    );
  }
}

function jsonResponse(payload: unknown, status = 200): Response {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
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
            jsonResponse({ upload_token: 'upload-token', s3_config_id: clientS3ConfigId }),
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
    socket = FakeWebSocket.instances.at(-1)!;
    await vi.waitFor(() => expect(stateModule.state.composerConfigRevision).toBe(100));
    socket.onopen?.();
    await Promise.resolve();
  });

  it('keeps newer model state while applying older full Session and Group metadata', async () => {
    socket.receive({
      type: 'session_model_configuration',
      id: 'main',
      modelOverridePresent: false,
      modelOverrideConfigured: false,
      effectiveModelConfigured: false,
      explicitPrimaryModelConfigured: false,
      capabilities: { image: false, s3: false },
      configRevision: 101,
    });
    expect(stateModule.state.composerSessionModelRevision).toBe(101);

    stateModule.state.sessionSwitchInFlight = true;
    socket.receive({
      type: 'session',
      id: 'main',
      name: 'Main metadata from older snapshot',
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

  it('locks attachment changes for the full asynchronous Session create request', async () => {
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

    expect(stateModule.state.sessionIdentityMutationInFlight).toBe(true);
    expect(stateModule.state.sessionSwitchInFlight).toBe(false);
    expect(createButton.disabled).toBe(true);
    expect(stateModule.dom.attachBtn?.disabled).toBe(true);

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
    expect(stateModule.dom.attachBtn?.disabled).toBe(true);

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
    expect(stateModule.state.sessionIdentityMutationInFlight).toBe(true);

    composerModule.beginComposerSessionTransition(false, 'main');
    stateModule.state.sessionSwitchInFlight = true;
    resolveCreate(jsonResponse({ error: 'create failed' }, 500));
    await vi.waitFor(() => expect(stateModule.state.sessionIdentityMutationInFlight).toBe(false));

    expect(stateModule.state.sessionSwitchInFlight).toBe(true);
    expect(stateModule.state.composerSessionTransitionPending).toBe(true);

    composerModule.completeComposerSessionTransition();
    stateModule.state.sessionSwitchInFlight = false;
  });
});
