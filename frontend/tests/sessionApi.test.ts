import { beforeEach, describe, expect, it, vi } from 'vitest';

describe('session API', () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it('creates a session without sending a user-provided id', async () => {
    const fetchMock = vi.fn(async () => ({
      ok: true,
      json: async () => ({
        ok: true,
        session: {
          id: 'a1b2c3',
          name: 'Session a1b2c3',
          updated_at: 10,
        },
      }),
    }));
    vi.stubGlobal('fetch', fetchMock);

    const { createSession } = await import('../src/sessionApi.js');
    const session = await createSession();

    expect(fetchMock).toHaveBeenCalledWith('/api/session', { method: 'POST' });
    expect(session).toEqual({
      id: 'a1b2c3',
      name: 'Session a1b2c3',
      updated_at: 10,
      corrupt: false,
    });
  });

  it('surfaces create errors from the backend', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => ({
        ok: false,
        status: 500,
        json: async () => ({ error: 'failed to create' }),
      })),
    );

    const { createSession } = await import('../src/sessionApi.js');

    await expect(createSession()).rejects.toThrow('failed to create');
  });
});
