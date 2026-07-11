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

  it('loads a session group detail response', async () => {
    const fetchMock = vi.fn(async () => ({
      ok: true,
      json: async () => ({
        group: {
          id: 'reviewers',
          name: 'Reviewers',
          members: ['worker-a', ' worker-b ', ''],
          admins: ['worker-b'],
          pending_votes: [
            {
              id: 'vote-1',
              action: 'remove_member',
              target_session_id: 'worker-a',
              requester_session_id: 'worker-b',
              approvals: ['worker-b'],
              threshold: 2,
              created_at: 13,
              updated_at: 14,
            },
          ],
          member_details: [
            { id: 'main', name: 'Main', role: 'owner' },
            { id: 'worker-a', name: 'Worker A', role: 'member' },
            { id: 'worker-b', name: 'Worker B', role: 'admin' },
          ],
          messages: [{ role: 'user', content: 'check' }],
          runs: [],
          created_at: 11,
          updated_at: 12,
          version: 1,
        },
      }),
    }));
    vi.stubGlobal('fetch', fetchMock);

    const { getSessionGroup } = await import('../src/sessionApi.js');
    const group = await getSessionGroup('reviewers');

    expect(fetchMock).toHaveBeenCalledWith('/api/session-group?group=reviewers', {
      cache: 'no-store',
    });
    expect(group).toEqual({
      id: 'reviewers',
      name: 'Reviewers',
      members: ['worker-a', 'worker-b'],
      admins: ['worker-b'],
      pending_votes: [
        {
          id: 'vote-1',
          action: 'remove_member',
          target_session_id: 'worker-a',
          requester_session_id: 'worker-b',
          approvals: ['worker-b'],
          threshold: 2,
          created_at: 13,
          updated_at: 14,
        },
      ],
      member_details: [
        { id: 'main', name: 'Main', role: 'owner' },
        { id: 'worker-a', name: 'Worker A', role: 'member' },
        { id: 'worker-b', name: 'Worker B', role: 'admin' },
      ],
      messages: [{ role: 'user', content: 'check' }],
      runs: [],
      created_at: 11,
      updated_at: 12,
      version: 1,
    });
  });

  it('normalizes invalid group vote thresholds to finite positive values', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => ({
        ok: true,
        json: async () => ({
          group: {
            id: 'reviewers',
            name: 'Reviewers',
            members: ['worker-a'],
            admins: ['worker-a'],
            pending_votes: [
              {
                id: 'vote-bad',
                action: 'remove_member',
                target_session_id: 'worker-a',
                requester_session_id: 'worker-a',
                approvals: ['worker-a'],
                threshold: 'not-a-number',
                created_at: 1,
                updated_at: 1,
              },
              {
                id: 'vote-zero',
                action: 'remove_member',
                target_session_id: 'worker-a',
                requester_session_id: 'worker-a',
                approvals: [],
                threshold: 0,
                created_at: 1,
                updated_at: 1,
              },
            ],
            member_details: [],
            messages: [],
            runs: [],
            created_at: 1,
            updated_at: 1,
            version: 2,
          },
        }),
      })),
    );

    const { getSessionGroup } = await import('../src/sessionApi.js');
    const group = await getSessionGroup('reviewers');

    expect(group.pending_votes.map((vote) => vote.threshold)).toEqual([1, 1]);
  });

  it('creates, updates, and deletes session groups through the group API', async () => {
    const fetchMock = vi.fn(async (url: string, init?: RequestInit) => {
      if (url === '/api/session-group' && init?.method === 'POST') {
        return {
          ok: true,
          json: async () => ({
            group: {
              id: 'group-a',
              name: 'Group A',
              members: 2,
              messages: 0,
              running: 0,
              updated_at: 20,
            },
          }),
        };
      }
      if (url === '/api/session-group?group=group-a' && init?.method === 'PUT') {
        return {
          ok: true,
          json: async () => ({
            group: {
              id: 'group-a',
              name: 'Group B',
              members: 1,
              messages: 3,
              running: 1,
              updated_at: 30,
            },
          }),
        };
      }
      if (url === '/api/session-group?group=group-a' && init?.method === 'DELETE') {
        return { ok: true, json: async () => ({ ok: true }) };
      }
      return { ok: false, status: 404, json: async () => ({ error: 'unexpected call' }) };
    });
    vi.stubGlobal('fetch', fetchMock);

    const { createSessionGroup, deleteSessionGroup, updateSessionGroup } =
      await import('../src/sessionApi.js');

    await expect(createSessionGroup('Group A', ['worker-a', 'worker-b'])).resolves.toEqual({
      id: 'group-a',
      name: 'Group A',
      members: 2,
      messages: 0,
      running: 0,
      updated_at: 20,
      corrupt: false,
    });
    await expect(updateSessionGroup('group-a', 'Group B', ['worker-a'])).resolves.toEqual({
      id: 'group-a',
      name: 'Group B',
      members: 1,
      messages: 3,
      running: 1,
      updated_at: 30,
      corrupt: false,
    });
    await expect(deleteSessionGroup('group-a')).resolves.toBeUndefined();

    expect(fetchMock).toHaveBeenCalledWith('/api/session-group', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ name: 'Group A', members: ['worker-a', 'worker-b'] }),
    });
    expect(fetchMock).toHaveBeenCalledWith('/api/session-group?group=group-a', {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ name: 'Group B', members: ['worker-a'] }),
    });
    expect(fetchMock).toHaveBeenCalledWith('/api/session-group?group=group-a', {
      method: 'DELETE',
    });
  });

  it('promotes and removes group members through the member API', async () => {
    const fetchMock = vi.fn(async (url: string, init?: RequestInit) => {
      if (
        url === '/api/session-group/member?group=group-a&session=worker-a' &&
        init?.method === 'PUT'
      ) {
        return {
          ok: true,
          json: async () => ({
            group: {
              id: 'group-a',
              name: 'Group A',
              members: ['worker-a'],
              admins: ['worker-a'],
              pending_votes: [],
              member_details: [
                { id: 'main', name: 'Main', role: 'owner' },
                { id: 'worker-a', name: 'Worker A', role: 'admin' },
              ],
            },
          }),
        };
      }
      if (
        url === '/api/session-group/member?group=group-a&session=worker-a' &&
        init?.method === 'DELETE'
      ) {
        return {
          ok: true,
          json: async () => ({
            group: {
              id: 'group-a',
              name: 'Group A',
              members: [],
              admins: [],
              pending_votes: [],
              member_details: [{ id: 'main', name: 'Main', role: 'owner' }],
            },
          }),
        };
      }
      return { ok: false, status: 404, json: async () => ({ error: 'unexpected call' }) };
    });
    vi.stubGlobal('fetch', fetchMock);

    const { promoteSessionGroupAdmin, removeSessionGroupMember } =
      await import('../src/sessionApi.js');

    await expect(promoteSessionGroupAdmin('group-a', 'worker-a')).resolves.toMatchObject({
      id: 'group-a',
      admins: ['worker-a'],
      member_details: [{ id: 'main', name: 'Main', role: 'owner' }, expect.any(Object)],
    });
    await expect(removeSessionGroupMember('group-a', 'worker-a')).resolves.toMatchObject({
      id: 'group-a',
      members: [],
      admins: [],
    });
  });
});
