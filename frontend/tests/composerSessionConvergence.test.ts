import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest';

import { acceptComposerConfigRevision } from '../src/composerAvailability.js';
import { ACTIVE_SESSION_STORAGE_KEY } from '../src/sessionPersistence.js';

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
    this.readyState = FakeWebSocket.CLOSED;
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

function sessionPayload(id: string, configRevision: number) {
  return {
    type: 'session',
    id,
    name: id === 'main' ? 'Main' : id === 'target-session' ? 'Target Session' : 'Created Session',
    model: 'gateway/model',
    effort: 'auto',
    modelOverridePresent: true,
    modelOverrideConfigured: true,
    effectiveModelConfigured: true,
    explicitPrimaryModelConfigured: false,
    capabilities: { image: false, s3: false, s3_config_id: null },
    usage: {},
    configRevision,
  };
}

describe('active Session model convergence', () => {
  let stateModule: typeof import('../src/state.js');
  const fetchEvents: string[] = [];
  const configRevisions = [1, 2, 2, 2, 2, 3, 3, 4, 4];
  let lastServedConfigRevision = 1;
  let sessionModelFailuresRemaining = 0;
  let sessionModelHangsRemaining = 0;
  const sessionModelHangSignals: AbortSignal[] = [];
  const unhandledRejections: unknown[] = [];
  const onUnhandledRejection = (event: PromiseRejectionEvent): void => {
    unhandledRejections.push(event.reason);
    event.preventDefault();
  };

  beforeAll(async () => {
    const body = indexHtml.match(/<body[^>]*>([\s\S]*?)<\/body>/i)?.[1];
    if (!body) throw new Error('index.html body not found');
    document.body.innerHTML = body;
    localStorage.setItem(ACTIVE_SESSION_STORAGE_KEY, 'main');
    FakeWebSocket.instances = [];
    window.addEventListener('unhandledrejection', onUnhandledRejection);

    vi.stubGlobal('WebSocket', FakeWebSocket);
    vi.stubGlobal(
      'fetch',
      vi.fn<typeof fetch>((input, init) => {
        const url =
          typeof input === 'string' ? input : input instanceof URL ? input.toString() : input.url;
        const method = init?.method ?? (input instanceof Request ? input.method : 'GET');
        fetchEvents.push(`${method}:${url}`);

        if (url === '/api/config') {
          lastServedConfigRevision = configRevisions.shift() ?? lastServedConfigRevision;
          return Promise.resolve(
            jsonResponse({
              config: {},
              configuredModelsAvailable: true,
              explicitPrimaryModelConfigured: false,
              configRevision: lastServedConfigRevision,
            }),
          );
        }
        if (url === '/api/sessions') {
          return Promise.resolve(
            jsonResponse({
              session_ids_case_sensitive: true,
              sessions: [
                { id: 'main', name: 'Main', updated_at: 2, corrupt: false },
                {
                  id: 'target-session',
                  name: 'Target Session',
                  updated_at: 1,
                  corrupt: false,
                },
              ],
            }),
          );
        }
        if (url === '/api/client-config') {
          return Promise.resolve(
            jsonResponse({
              upload_token: 'upload-token',
              s3_config_id: '',
              features: { groups: false },
              protocols: { execution_identity: 1 },
            }),
          );
        }
        if (url.startsWith('/api/session-models?') && method === 'GET') {
          if (sessionModelHangsRemaining > 0) {
            sessionModelHangsRemaining -= 1;
            if (init?.signal) sessionModelHangSignals.push(init.signal);
            return new Promise<Response>(() => {});
          }
          if (sessionModelFailuresRemaining > 0) {
            sessionModelFailuresRemaining -= 1;
            return Promise.reject(new Error('temporary Session model lookup failure'));
          }
          const sessionId = new URL(url, 'http://localhost').searchParams.get('session') || 'main';
          return Promise.resolve(
            jsonResponse({
              session: {
                id: sessionId,
                model: 'gateway/model',
                effort: 'auto',
                modelOverridePresent: true,
                modelOverrideConfigured: true,
                effectiveModelConfigured: true,
              },
              explicitPrimaryModelConfigured: false,
              capabilities: { image: false },
              models: [
                {
                  ref: 'gateway/model',
                  provider: 'gateway',
                  id: 'model',
                  name: 'Model',
                  input: ['text'],
                  reasoning: false,
                  efforts: ['off'],
                  defaultEffort: 'off',
                },
              ],
              configRevision: lastServedConfigRevision,
            }),
          );
        }
        if (url === '/api/session' && method === 'POST') {
          return Promise.resolve(
            jsonResponse({ session: { id: 'created-session', name: 'Created Session' } }),
          );
        }
        if (url === '/api/health') return Promise.resolve(jsonResponse({ version: 'test' }));
        if (url === '/api/session-groups') {
          return Promise.reject(new Error('disabled clients must not discover Groups'));
        }
        return Promise.resolve(jsonResponse({ error: `Unexpected fetch URL: ${url}` }, 404));
      }),
    );

    await import('../src/main.js');
    stateModule = await import('../src/state.js');
    await vi.waitFor(() => expect(FakeWebSocket.instances).toHaveLength(1));
    await vi.waitFor(() => expect(stateModule.state.composerConfigRevision).toBe(1));
  });

  afterAll(() => {
    vi.useRealTimers();
    window.removeEventListener('unhandledrejection', onUnhandledRejection);
    localStorage.clear();
    vi.unstubAllGlobals();
    document.body.innerHTML = '';
  });

  it('converges on refresh, switch, and create without saving a model', async () => {
    const initialSocket = FakeWebSocket.instances[0];
    initialSocket.onopen?.();
    initialSocket.receive(sessionPayload('main', 1));
    initialSocket.receive({ type: 'history', messages: [] });

    await vi.waitFor(() => {
      expect(stateModule.state.composerConfigRevision).toBe(2);
      expect(stateModule.state.composerSessionModelRevision).toBe(2);
      expect(stateModule.state.composerModelAvailability).toBe('ready');
    });
    expect(stateModule.state.composerSessionIdentityPending).toBe(false);
    expect(stateModule.state.sessionSwitchInFlight).toBe(false);
    const recoveryRequestsAfterRefresh = fetchEvents.filter((event) =>
      event.startsWith('GET:/api/session-models?'),
    ).length;
    expect(recoveryRequestsAfterRefresh).toBeGreaterThan(0);
    expect(recoveryRequestsAfterRefresh).toBeLessThanOrEqual(2);

    document
      .querySelector<HTMLButtonElement>(
        '.session-drawer-row[data-session-id="target-session"] [data-session-action="switch"]',
      )
      ?.click();
    await vi.waitFor(() => expect(FakeWebSocket.instances).toHaveLength(2));
    const targetSocket = FakeWebSocket.instances[1];
    targetSocket.onopen?.();
    targetSocket.receive(sessionPayload('target-session', 2));
    targetSocket.receive({ type: 'history', messages: [] });

    await vi.waitFor(() => {
      expect(stateModule.state.activeSessionId).toBe('target-session');
      expect(stateModule.state.composerSessionModelRevision).toBe(2);
      expect(stateModule.state.composerModelAvailability).toBe('ready');
      expect(stateModule.state.sessionSwitchInFlight).toBe(false);
    });
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 0));
    expect(fetchEvents.filter((event) => event.startsWith('GET:/api/session-models?')).length).toBe(
      recoveryRequestsAfterRefresh,
      'the first current versioned payload should converge without HTTP recovery',
    );

    document
      .querySelector<HTMLButtonElement>(
        '.session-drawer-row[data-session-id="main"] [data-session-action="switch"]',
      )
      ?.click();
    await vi.waitFor(() => expect(FakeWebSocket.instances).toHaveLength(3));
    const returnSocket = FakeWebSocket.instances[2];
    returnSocket.onopen?.();
    returnSocket.receive(sessionPayload('main', 2));
    returnSocket.receive({ type: 'history', messages: [] });

    await vi.waitFor(() => {
      expect(stateModule.state.activeSessionId).toBe('main');
      expect(stateModule.state.composerConfigRevision).toBe(3);
      expect(stateModule.state.composerSessionModelRevision).toBe(3);
      expect(stateModule.state.composerModelAvailability).toBe('ready');
    });

    stateModule.dom.sessionDrawerNewBtn!.disabled = false;
    stateModule.dom.sessionDrawerNewBtn!.click();
    document.querySelector<HTMLButtonElement>('.action-dialog-submit')?.click();
    await vi.waitFor(() => expect(FakeWebSocket.instances).toHaveLength(4));
    const createdSocket = FakeWebSocket.instances[3];
    createdSocket.onopen?.();
    createdSocket.receive(sessionPayload('created-session', 3));
    createdSocket.receive({ type: 'history', messages: [] });

    await vi.waitFor(() => {
      expect(stateModule.state.activeSessionId).toBe('created-session');
      expect(stateModule.state.composerConfigRevision).toBe(4);
      expect(stateModule.state.composerSessionModelRevision).toBe(4);
      expect(stateModule.state.composerEffectiveModelConfigured).toBe(true);
      expect(stateModule.state.composerModelAvailability).toBe('ready');
      expect(stateModule.state.composerSessionIdentityPending).toBe(false);
      expect(stateModule.state.sessionSwitchInFlight).toBe(false);
    });

    const recoveryRequestsBeforeFailure = fetchEvents.filter((event) =>
      event.startsWith('GET:/api/session-models?'),
    ).length;
    sessionModelFailuresRemaining = 2;
    lastServedConfigRevision = 5;
    expect(acceptComposerConfigRevision(5)).toBe(true);

    await vi.waitFor(() => {
      expect(stateModule.state.composerConfigRevision).toBe(5);
      expect(stateModule.state.composerSessionModelRevision).toBe(4);
      expect(stateModule.state.composerModelAvailability).toBe('config-unavailable');
      expect(stateModule.dom.composerAvailabilityRetry?.hidden).toBe(false);
    });
    expect(
      fetchEvents.filter((event) => event.startsWith('GET:/api/session-models?')).length -
        recoveryRequestsBeforeFailure,
    ).toBe(2);

    stateModule.dom.composerAvailabilityRetry?.click();
    await vi.waitFor(() => {
      expect(stateModule.state.composerSessionModelRevision).toBe(5);
      expect(stateModule.state.composerEffectiveModelConfigured).toBe(true);
      expect(stateModule.state.composerModelAvailability).toBe('ready');
      expect(stateModule.dom.composerAvailabilityRetry?.hidden).toBe(true);
    });
    expect(
      fetchEvents.filter((event) => event.startsWith('GET:/api/session-models?')).length -
        recoveryRequestsBeforeFailure,
    ).toBe(3);

    const recoveryRequestsBeforeTimeout = fetchEvents.filter((event) =>
      event.startsWith('GET:/api/session-models?'),
    ).length;
    sessionModelHangsRemaining = 2;
    lastServedConfigRevision = 6;
    vi.useFakeTimers();
    expect(acceptComposerConfigRevision(6)).toBe(true);
    await vi.advanceTimersByTimeAsync(10_000);

    expect(stateModule.state.composerConfigRevision).toBe(6);
    expect(stateModule.state.composerSessionModelRevision).toBe(5);
    expect(stateModule.state.composerModelAvailability).toBe('config-unavailable');
    expect(stateModule.dom.composerAvailabilityRetry?.hidden).toBe(false);
    expect(
      fetchEvents.filter((event) => event.startsWith('GET:/api/session-models?')).length -
        recoveryRequestsBeforeTimeout,
    ).toBe(2);
    expect(sessionModelHangSignals).toHaveLength(2);
    expect(sessionModelHangSignals.every((signal) => signal.aborted)).toBe(true);
    expect(vi.getTimerCount()).toBe(0);

    vi.useRealTimers();
    stateModule.dom.composerAvailabilityRetry?.click();
    await vi.waitFor(() => {
      expect(stateModule.state.composerSessionModelRevision).toBe(6);
      expect(stateModule.state.composerEffectiveModelConfigured).toBe(true);
      expect(stateModule.state.composerModelAvailability).toBe('ready');
      expect(stateModule.dom.composerAvailabilityRetry?.hidden).toBe(true);
    });
    expect(
      fetchEvents.filter((event) => event.startsWith('GET:/api/session-models?')).length -
        recoveryRequestsBeforeTimeout,
    ).toBe(3);
    expect(unhandledRejections).toEqual([]);

    expect(fetchEvents).not.toContain('GET:/api/session-groups');
    expect(fetchEvents.some((event) => event.startsWith('PUT:/api/session-models?'))).toBe(false);
  });
});
