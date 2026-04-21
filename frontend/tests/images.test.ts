import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ensureUploadTokenInternal } from '../src/images.js';
import { state } from '../src/state.js';

function jsonResponse(payload: unknown): Response {
  return new Response(JSON.stringify(payload), {
    status: 200,
    headers: { 'Content-Type': 'application/json' },
  });
}

function deferredResponse() {
  let resolve!: (response: Response) => void;
  const promise = new Promise<Response>((resolveResponse) => {
    resolve = resolveResponse;
  });
  return { promise, resolve };
}

describe('ensureUploadTokenInternal', () => {
  beforeEach(() => {
    state.uploadToken = '';
    state.uploadTokenPromise = null;
    state.uploadTokenRequestSeq = 0;
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('keeps the newest forced refresh token when an older request resolves later', async () => {
    const first = deferredResponse();
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockReturnValueOnce(first.promise)
      .mockResolvedValueOnce(jsonResponse({ upload_token: 'fresh-token' }));
    vi.stubGlobal('fetch', fetchMock);

    const firstRequest = ensureUploadTokenInternal(false);
    const refreshedRequest = ensureUploadTokenInternal(true);

    expect(fetchMock).toHaveBeenCalledTimes(2);

    await expect(refreshedRequest).resolves.toBe('fresh-token');
    expect(state.uploadToken).toBe('fresh-token');

    first.resolve(jsonResponse({ upload_token: 'stale-token' }));

    await expect(firstRequest).resolves.toBe('stale-token');
    expect(state.uploadToken).toBe('fresh-token');
    expect(state.uploadTokenPromise).toBeNull();
  });
});
