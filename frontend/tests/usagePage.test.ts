import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import React from 'react';
import { act } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createRoot, type Root } from 'react-dom/client';

import { setLanguage } from '../src/i18n.js';
import type { UsageData } from '../src/types/config.js';
import {
  UsagePage,
  UsageView,
  buildUsageDashboard,
  buildUsageTimeline,
  closeUsagePage,
  hasAnyUsageData,
  openUsagePage,
  sortRoles,
} from '../src/pages/UsagePage.js';

const NOW = new Date(2026, 6, 19, 12, 0, 0);
const usageCss = readFileSync(resolve(process.cwd(), 'src/css/usage-console.css'), 'utf8');

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

describe('usage aggregation', () => {
  it.each([7, 14, 30])('builds and zero-fills a %i-day local timeline', (days) => {
    const timeline = buildUsageTimeline(
      {
        daily_input: 30,
        daily_output: 10,
        usage_history: [
          { date: '2026-07-18', input: 12, output: 3 },
          // Today's snapshot must not override the live daily counters.
          { date: '2026-07-19', input: 999, output: 999 },
        ],
      },
      days,
      NOW,
    );

    expect(timeline).toHaveLength(days);
    expect(timeline.at(-1)).toMatchObject({
      date: '2026-07-19',
      input: 30,
      output: 10,
      total: 40,
    });
    expect(timeline.at(-2)).toMatchObject({ date: '2026-07-18', input: 12, output: 3 });
    expect(timeline.filter((point) => point.total === 0)).toHaveLength(days - 2);
  });

  it('calculates totals, average, active days and peak for the selected range', () => {
    const aggregate = buildUsageDashboard(
      {
        daily_input: 50,
        daily_output: 10,
        total_input: 1_000,
        total_output: 200,
        total: 1_200,
        usage_history: [{ date: '2026-07-18', input: 20, output: 5 }],
      },
      7,
      NOW,
    );

    expect(aggregate.today).toEqual({ input: 50, output: 10, total: 60 });
    expect(aggregate.allTime).toEqual({ input: 1_000, output: 200, total: 1_200 });
    expect(aggregate.range).toEqual({ input: 70, output: 15, total: 85 });
    expect(aggregate.average.total).toBeCloseTo(85 / 7);
    expect(aggregate.activeDays).toBe(2);
    expect(aggregate.peak).toMatchObject({ date: '2026-07-19', total: 60 });
  });

  it('aggregates provider and role history and appends only the missing split', () => {
    const aggregate = buildUsageDashboard(
      {
        daily_input: 50,
        daily_output: 10,
        total_input: 1_000,
        total_output: 200,
        daily_providers: { openai: [30, 6] },
        daily_roles: { Primary: [30, 6] },
        total_providers: { openai: [700, 100] },
        total_roles: { Primary: [700, 100] },
        usage_history: [
          {
            date: '2026-07-18',
            input: 20,
            output: 5,
            providers: { openai: [15, 3] },
            roles: { Primary: [15, 3] },
          },
        ],
      },
      7,
      NOW,
    );

    expect(aggregate.providers.range[0]).toMatchObject({
      name: 'openai',
      input: 45,
      output: 9,
      total: 54,
    });
    expect(aggregate.providers.range[1]).toMatchObject({
      name: '__unattributed__',
      input: 25,
      output: 6,
      unattributed: true,
    });
    expect(aggregate.roles.range[0]).toMatchObject({
      name: 'Primary',
      input: 45,
      output: 9,
      total: 54,
    });
    expect(aggregate.roles.range[1]).toMatchObject({
      name: '__unattributed__',
      input: 25,
      output: 6,
    });
    expect(aggregate.providers.total.at(-1)).toMatchObject({
      name: '__unattributed__',
      input: 300,
      output: 100,
    });
  });

  it('distinguishes zero, missing dimensions, and partial dimensions', () => {
    expect(hasAnyUsageData({})).toBe(false);
    expect(hasAnyUsageData({ usage_history: [{ date: '2020-01-01', input: 1, output: 0 }] })).toBe(
      true,
    );
    expect(buildUsageDashboard({}, 7, NOW).providers.range).toEqual([]);

    const missing = buildUsageDashboard({ daily_input: 10, total_input: 10 }, 7, NOW);
    expect(missing.hasUsage).toBe(true);
    expect(missing.providers.range).toEqual([]);

    const partial = buildUsageDashboard(
      { daily_input: 10, total_input: 10, daily_providers: { openai: [4, 0] } },
      7,
      NOW,
    );
    expect(partial.providers.range).toHaveLength(2);
    expect(partial.providers.range[1]).toMatchObject({ name: '__unattributed__', input: 6 });
  });

  it('sorts every role by usage and uses the built-in role order only for ties', () => {
    const entries = [
      { name: 'Primary', input: 10, output: 0, total: 10 },
      { name: 'Fast', input: 10, output: 0, total: 10 },
      { name: 'Custom Reviewer', input: 30, output: 0, total: 30 },
      { name: 'Another Custom Role', input: 20, output: 0, total: 20 },
      { name: '__unattributed__', input: 5, output: 0, total: 5, unattributed: true },
    ];

    expect(sortRoles(entries).map((entry) => entry.name)).toEqual([
      'Custom Reviewer',
      'Another Custom Role',
      'Primary',
      'Fast',
      '__unattributed__',
    ]);
  });
});

describe('Usage responsive controls', () => {
  it('keeps range and breakdown controls at least 44px tall on mobile', () => {
    expect(usageCss).toMatch(
      /@media \(max-width: 768px\)[\s\S]*?\.usage-console-range button,[\s\S]*?\.usage-console-scope button,[\s\S]*?min-height: 44px;/,
    );
  });
});

describe('UsageView dashboard states', () => {
  let root: Root | null = null;

  beforeEach(() => {
    (
      globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }
    ).IS_REACT_ACT_ENVIRONMENT = true;
    document.body.innerHTML = '<div id="usage-test-root"></div>';
    setLanguage('en');
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

  async function renderView(props: React.ComponentProps<typeof UsageView> = {}): Promise<void> {
    const container = document.getElementById('usage-test-root');
    if (!container) throw new Error('Usage root not found');
    root = createRoot(container);
    await act(async () => {
      root?.render(React.createElement(UsageView, { sessionId: 'main', ...props }));
      await flushMicrotasks();
    });
  }

  it('renders one empty state instead of zero charts', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(() => Promise.resolve(jsonResponse({}))),
    );
    await renderView();

    await vi.waitFor(() => expect(document.body.textContent).toContain('No usage recorded yet'));
    expect(document.querySelectorAll('.usage-console-metric')).toHaveLength(0);
    expect(document.querySelectorAll('.usage-console-panel')).toHaveLength(0);
  });

  it('renders metrics, accessible SVG charts, rankings, and an equivalent table', async () => {
    const payload: UsageData = {
      daily_input: 12,
      daily_output: 8,
      total_input: 120,
      total_output: 80,
      daily_roles: { Primary: [12, 8] },
      total_roles: { Primary: [120, 80] },
      daily_providers: { openai: [12, 8] },
      total_providers: { openai: [120, 80] },
    };
    vi.stubGlobal(
      'fetch',
      vi.fn(() => Promise.resolve(jsonResponse(payload))),
    );
    await renderView();

    await vi.waitFor(() =>
      expect(document.querySelectorAll('.usage-console-metric')).toHaveLength(4),
    );
    const trend = document.querySelector<SVGElement>('svg.usage-console-trend[role="group"]');
    expect(trend).not.toBeNull();
    expect(trend?.querySelector('title')?.textContent).toBe('Daily Usage');
    expect(trend?.getAttribute('role')).not.toBe('img');
    const trendPoint = trend?.querySelector<SVGGElement>(
      '.usage-console-trend-point[tabindex="0"]',
    );
    const descriptionId = trendPoint?.getAttribute('aria-describedby');
    expect(descriptionId).toBeTruthy();
    expect(document.getElementById(descriptionId || '')?.textContent).toContain('2026-07');
    expect(trendPoint?.querySelector('.usage-console-trend-tooltip')).not.toBeNull();
    expect(document.querySelector('svg[role="img"][aria-label="Input / Output"]')).not.toBeNull();
    expect(document.querySelectorAll('.usage-console-ranking')).toHaveLength(2);
    expect(document.querySelector('.usage-console-data-table table')).not.toBeNull();
    expect(document.body.textContent).toContain('openai');
    expect(document.body.textContent).toContain('Primary');
  });

  it('uses one range control for the whole dashboard', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(() => Promise.resolve(jsonResponse({ daily_input: 7, total_input: 7 }))),
    );
    await renderView();
    await vi.waitFor(() =>
      expect(document.querySelectorAll('.usage-console-metric')).toHaveLength(4),
    );

    const thirtyDays = Array.from(document.querySelectorAll<HTMLButtonElement>('button')).find(
      (button) => button.textContent?.trim() === '30 days',
    );
    await act(async () => {
      thirtyDays?.click();
      await flushMicrotasks();
    });

    expect(thirtyDays?.getAttribute('aria-pressed')).toBe('true');
    expect(document.querySelectorAll('.usage-console-data-table tbody tr')).toHaveLength(30);
  });

  it('keeps stale data visible during refresh and after a refresh failure', async () => {
    const refreshResponse = deferred<Response>();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        jsonResponse({ daily_input: 9, daily_output: 1, total_input: 9, total_output: 1 }),
      )
      .mockReturnValueOnce(refreshResponse.promise);
    vi.stubGlobal('fetch', fetchMock);
    await renderView();
    await vi.waitFor(() =>
      expect(document.querySelectorAll('.usage-console-metric')).toHaveLength(4),
    );

    const refresh = Array.from(document.querySelectorAll<HTMLButtonElement>('button')).find(
      (button) => button.textContent?.trim() === 'Refresh',
    );
    await act(async () => {
      refresh?.click();
      await flushMicrotasks();
    });

    expect(document.querySelectorAll('.usage-console-metric')).toHaveLength(4);
    expect(document.body.textContent).toContain('Refreshing usage data');

    await act(async () => {
      refreshResponse.resolve(new Response('', { status: 503 }));
      await refreshResponse.promise;
      await flushMicrotasks();
    });
    await vi.waitFor(() => expect(document.body.textContent).toContain('HTTP 503'));
    expect(document.querySelectorAll('.usage-console-metric')).toHaveLength(4);
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it('clears a pending refresh state when Usage becomes inactive', async () => {
    const refreshResponse = deferred<Response>();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse({ daily_input: 9, total_input: 9 }))
      .mockReturnValueOnce(refreshResponse.promise);
    vi.stubGlobal('fetch', fetchMock);
    await renderView();
    await vi.waitFor(() =>
      expect(document.querySelectorAll('.usage-console-metric')).toHaveLength(4),
    );

    const refresh = Array.from(document.querySelectorAll<HTMLButtonElement>('button')).find(
      (button) => button.textContent?.trim() === 'Refresh',
    )!;
    await act(async () => {
      refresh.click();
      await flushMicrotasks();
      root?.render(React.createElement(UsageView, { sessionId: 'main', active: false }));
      await flushMicrotasks();
    });

    expect(refresh.disabled).toBe(false);
    expect(document.body.textContent).not.toContain('Refreshing usage data');

    await act(async () => {
      root?.render(React.createElement(UsageView, { sessionId: 'main', active: true }));
      await flushMicrotasks();
    });
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(
      Array.from(document.querySelectorAll<HTMLButtonElement>('button')).find(
        (button) => button.textContent?.trim() === 'Refresh',
      )?.disabled,
    ).toBe(false);
  });

  it('never labels the previous Session data as the newly requested Session', async () => {
    const current = deferred<Response>();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse({ daily_input: 111, total_input: 111 }))
      .mockReturnValueOnce(current.promise);
    vi.stubGlobal('fetch', fetchMock);
    await renderView({ sessionId: 'first' });
    await vi.waitFor(() => expect(document.body.textContent).toContain('111'));

    await act(async () => {
      root?.render(React.createElement(UsageView, { sessionId: 'second' }));
      await flushMicrotasks();
    });

    expect(document.body.textContent).toContain('second');
    expect(document.querySelector('.usage-console-metrics')?.textContent || '').not.toContain(
      '111',
    );

    await act(async () => {
      current.resolve(jsonResponse({ daily_input: 22, total_input: 22 }));
      await current.promise;
      await flushMicrotasks();
    });
    await vi.waitFor(() => expect(document.body.textContent).toContain('22'));
  });

  it('ignores a late response after the controlled session changes', async () => {
    const stale = deferred<Response>();
    const current = deferred<Response>();
    const fetchMock = vi
      .fn()
      .mockReturnValueOnce(stale.promise)
      .mockReturnValueOnce(current.promise);
    vi.stubGlobal('fetch', fetchMock);
    await renderView({ sessionId: 'first' });

    await act(async () => {
      root?.render(React.createElement(UsageView, { sessionId: 'second' }));
      await flushMicrotasks();
    });
    expect(fetchMock).toHaveBeenCalledTimes(2);

    await act(async () => {
      current.resolve(jsonResponse({ daily_input: 20, total_input: 20 }));
      await current.promise;
      await flushMicrotasks();
    });
    await vi.waitFor(() => expect(document.body.textContent).toContain('second'));
    expect(document.querySelector('.usage-console-metrics')?.textContent).toContain('20');

    await act(async () => {
      stale.resolve(jsonResponse({ daily_input: 999, total_input: 999 }));
      await stale.promise;
      await flushMicrotasks();
    });
    expect(document.querySelector('.usage-console-metrics')?.textContent).not.toContain('999');
  });

  it('keeps the compatibility bridge available without dialog semantics', async () => {
    document.body.innerHTML = '<div id="usage-page"></div>';
    vi.stubGlobal(
      'fetch',
      vi.fn(() => Promise.resolve(jsonResponse({}))),
    );
    root = createRoot(document.getElementById('usage-page')!);
    await act(async () => {
      root?.render(React.createElement(UsagePage));
      openUsagePage('main');
      await flushMicrotasks();
    });
    await vi.waitFor(() => expect(document.querySelector('.usage-console-view')).not.toBeNull());
    expect(document.querySelector('[role="dialog"]')).toBeNull();

    await act(async () => {
      closeUsagePage();
      await flushMicrotasks();
    });
    expect(document.querySelector('.usage-console-view')).toBeNull();
  });
});
