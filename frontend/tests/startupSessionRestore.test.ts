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
}

interface StartedWorkspace {
  events: string[];
  socketUrl: URL;
  stateModule: typeof import('../src/state.js');
}

function jsonResponse(payload: unknown, status = 200): Response {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

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

  class FakeWebSocket {
    static readonly OPEN = 1;
    static readonly CLOSED = 3;

    readyState = FakeWebSocket.OPEN;
    onopen: (() => void) | null = null;
    onclose: (() => void) | null = null;
    onerror: (() => void) | null = null;
    onmessage: ((event: MessageEvent<string>) => void) | null = null;

    constructor(url: string) {
      events.push(`websocket:${url}`);
      socketUrls.push(url);
    }

    send(): void {}

    close(): void {
      this.readyState = FakeWebSocket.CLOSED;
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
        return Promise.resolve(
          jsonResponse({
            upload_token: 'upload-token',
            s3_config_id: '',
            features: { groups: scenario.groupsEnabled === true },
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

  await import('../src/main.js');
  const stateModule = await import('../src/state.js');
  await vi.waitFor(() => expect(socketUrls).toHaveLength(1));
  return { events, socketUrl: new URL(socketUrls[0]), stateModule };
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

describe('startup Session restoration', () => {
  beforeEach(() => {
    vi.resetModules();
    vi.unstubAllGlobals();
    localStorage.clear();
  });

  afterEach(() => {
    localStorage.clear();
    vi.unstubAllGlobals();
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
});
