import React from 'react';
import { act } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createRoot, type Root } from 'react-dom/client';

import { setLanguage } from '../src/i18n.js';
import {
  UsagePage,
  closeUsagePage,
  hasAnyUsageData,
  openUsagePage,
} from '../src/pages/UsagePage.js';

function jsonResponse(payload: unknown, status = 200): Response {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

async function flushMicrotasks(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

function deferred<T>(): {
  promise: Promise<T>;
  resolve: (value: T) => void;
} {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((accept) => {
    resolve = accept;
  });
  return { promise, resolve };
}

describe('UsagePage information states', () => {
  let root: Root | null = null;

  beforeEach(() => {
    (
      globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }
    ).IS_REACT_ACT_ENVIRONMENT = true;
    document.body.innerHTML = '<div id="usage-page"></div>';
    setLanguage('en');
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
  });

  afterEach(async () => {
    if (root) {
      await act(async () => {
        root?.unmount();
        await flushMicrotasks();
      });
      root = null;
    }
    document.body.innerHTML = '';
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean })
      .IS_REACT_ACT_ENVIRONMENT;
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  async function renderAndOpen(): Promise<void> {
    const container = document.getElementById('usage-page');
    if (!container) throw new Error('Usage root not found');
    root = createRoot(container);
    await act(async () => {
      root?.render(React.createElement(UsagePage));
      await flushMicrotasks();
      openUsagePage('main');
      await flushMicrotasks();
    });
  }

  it('shows one compact empty state instead of zero cards and blank charts', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(() => Promise.resolve(jsonResponse({}))),
    );

    await renderAndOpen();
    await vi.waitFor(() => expect(document.body.textContent).toContain('No usage recorded yet'));

    expect(document.querySelectorAll('.usage-stat-card')).toHaveLength(0);
    expect(document.querySelectorAll('.usage-chart-wrap')).toHaveLength(0);
    expect(hasAnyUsageData({})).toBe(false);
  });

  it('renders two aggregate cards, flat role rows, and per-chart empty states', async () => {
    const payload = {
      daily_input: 12,
      daily_output: 8,
      total_input: 120,
      total_output: 80,
      daily_roles: { primary: [12, 8] as [number, number] },
      total_roles: { primary: [120, 80] as [number, number] },
      usage_history: [],
      daily_providers: {},
    };
    vi.stubGlobal(
      'fetch',
      vi.fn(() => Promise.resolve(jsonResponse(payload))),
    );

    await renderAndOpen();
    await vi.waitFor(() => expect(document.querySelectorAll('.usage-stat-card')).toHaveLength(2));

    expect(hasAnyUsageData(payload)).toBe(true);
    expect(document.querySelectorAll('.usage-role-card')).toHaveLength(1);
    expect(document.querySelectorAll('.usage-chart-wrap')).toHaveLength(2);
    expect(document.querySelectorAll('.usage-chart-container canvas')).toHaveLength(1);
    expect(document.querySelector('select[aria-label="Daily usage range"]')).not.toBeNull();
    expect(document.querySelector('select[aria-label="Provider usage range"]')).not.toBeNull();
    expect(document.querySelector('canvas[role="img"][aria-label="Daily Usage"]')).not.toBeNull();
    expect(document.body.textContent).toContain('No per-provider data available yet');
  });

  it('keeps a compact error state and retries the request', async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        jsonResponse({
          daily_input: 4,
          daily_output: 2,
          total_input: 4,
          total_output: 2,
        }),
      )
      .mockResolvedValueOnce(new Response('', { status: 503 }))
      .mockResolvedValueOnce(jsonResponse({}));
    vi.stubGlobal('fetch', fetchMock);

    await renderAndOpen();
    await vi.waitFor(() => expect(document.querySelectorAll('.usage-stat-card')).toHaveLength(2));

    const refresh = Array.from(document.querySelectorAll<HTMLButtonElement>('button')).find(
      (button) => button.textContent?.trim() === 'Refresh',
    );
    await act(async () => {
      refresh?.click();
      await flushMicrotasks();
    });
    await vi.waitFor(() =>
      expect(document.body.textContent).toContain('Usage data is unavailable'),
    );
    expect(document.querySelectorAll('.usage-stat-card')).toHaveLength(0);

    await act(async () => {
      setLanguage('zh-CN');
      await flushMicrotasks();
    });
    expect(document.body.textContent).toContain('加载用量数据失败：HTTP 503');

    await act(async () => {
      setLanguage('en');
      await flushMicrotasks();
    });

    const retry = Array.from(document.querySelectorAll<HTMLButtonElement>('button')).find(
      (button) => button.textContent?.trim() === 'Try again',
    );
    await act(async () => {
      retry?.click();
      await flushMicrotasks();
    });
    await vi.waitFor(() => expect(document.body.textContent).toContain('No usage recorded yet'));
    expect(fetchMock).toHaveBeenCalledTimes(3);
  });

  it('ignores a late refresh response after opening usage for another session', async () => {
    const staleRefresh = deferred<Response>();
    const currentLoad = deferred<Response>();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        jsonResponse({ daily_input: 1, daily_output: 0, total_input: 1, total_output: 0 }),
      )
      .mockReturnValueOnce(staleRefresh.promise)
      .mockReturnValueOnce(currentLoad.promise);
    vi.stubGlobal('fetch', fetchMock);

    await renderAndOpen();
    await vi.waitFor(() => expect(document.querySelectorAll('.usage-stat-card')).toHaveLength(2));

    const refresh = Array.from(document.querySelectorAll<HTMLButtonElement>('button')).find(
      (button) => button.textContent?.trim() === 'Refresh',
    );
    await act(async () => {
      refresh?.click();
      await flushMicrotasks();
      closeUsagePage();
      openUsagePage('other');
      await flushMicrotasks();
    });
    expect(fetchMock).toHaveBeenCalledTimes(3);

    await act(async () => {
      currentLoad.resolve(
        jsonResponse({ daily_input: 20, daily_output: 0, total_input: 20, total_output: 0 }),
      );
      await currentLoad.promise;
      await flushMicrotasks();
    });
    await vi.waitFor(() =>
      expect(document.querySelector('.usage-summary')?.textContent).toContain('20'),
    );

    await act(async () => {
      staleRefresh.resolve(
        jsonResponse({ daily_input: 999, daily_output: 0, total_input: 999, total_output: 0 }),
      );
      await staleRefresh.promise;
      await flushMicrotasks();
    });
    expect(document.querySelector('.usage-summary')?.textContent).toContain('20');
    expect(document.querySelector('.usage-summary')?.textContent).not.toContain('999');
  });
});
