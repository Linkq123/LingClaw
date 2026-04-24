/**
 * Tests for the lazy page bridge (src/pages/lazy.ts).
 *
 * Each test uses vi.resetModules() + vi.doMock() + dynamic re-import so that
 * the module-level state (chunk promises, mount guards) starts fresh for every
 * case. vi.doMock() (non-hoisted) is required here because vi.mock() is
 * hoisted before imports and therefore can't be placed inside beforeEach or
 * individual test bodies.
 */

import { beforeEach, describe, expect, it, vi } from 'vitest';

// ── Helpers ────────────────────────────────────────────────────────────────

/** Set up all required mocks and return a fresh import of lazy.ts. */
async function makeLazy({
  settingsOpen = vi.fn(),
  settingsClose = vi.fn(),
  usageOpen = vi.fn(),
  usageClose = vi.fn(),
  renderFn = vi.fn(),
}: {
  settingsOpen?: ReturnType<typeof vi.fn>;
  settingsClose?: ReturnType<typeof vi.fn>;
  usageOpen?: ReturnType<typeof vi.fn>;
  usageClose?: ReturnType<typeof vi.fn>;
  renderFn?: ReturnType<typeof vi.fn>;
} = {}) {
  const createRoot = vi.fn(() => ({ render: renderFn }));

  vi.doMock('../src/pages/SettingsPage.js', () => ({
    SettingsPage: vi.fn(),
    openSettingsPage: settingsOpen,
    closeSettingsPage: settingsClose,
  }));
  vi.doMock('../src/pages/UsagePage.js', () => ({
    UsagePage: vi.fn(),
    openUsagePage: usageOpen,
    closeUsagePage: usageClose,
  }));
  vi.doMock('react-dom/client', () => ({ createRoot }));
  vi.doMock('react', () => ({ default: { createElement: vi.fn(() => null) } }));

  const lazy = await import('../src/pages/lazy.js');
  return { lazy, createRoot, renderFn };
}

/** Flush pending dynamic imports, microtasks, and one macro-task tick. */
async function flush(): Promise<void> {
  await vi.dynamicImportSettled();
  await new Promise((resolve) => setTimeout(resolve, 0));
  await Promise.resolve();
}

// ── Tests ──────────────────────────────────────────────────────────────────

describe('lazy page bridge', () => {
  beforeEach(() => {
    vi.resetModules();
    vi.clearAllMocks();
  });

  // ── close-before-load is a no-op ──────────────────────────────────────────

  it('closeSettingsPage before any load is a no-op (no throw, no mount)', async () => {
    const settingsClose = vi.fn();
    const { lazy, createRoot } = await makeLazy({ settingsClose });

    expect(() => lazy.closeSettingsPage()).not.toThrow();
    await flush();

    expect(createRoot).not.toHaveBeenCalled();
    expect(settingsClose).not.toHaveBeenCalled();
  });

  it('closeUsagePage before any load is a no-op', async () => {
    const usageClose = vi.fn();
    const { lazy, createRoot } = await makeLazy({ usageClose });

    expect(() => lazy.closeUsagePage()).not.toThrow();
    await flush();

    expect(createRoot).not.toHaveBeenCalled();
    expect(usageClose).not.toHaveBeenCalled();
  });

  // ── open triggers mount exactly once ──────────────────────────────────────

  it('openSettingsPage mounts React root and calls module openSettingsPage', async () => {
    const settingsOpen = vi.fn();
    const renderFn = vi.fn();
    const el = document.createElement('div');
    el.id = 'settings-page';
    document.body.appendChild(el);

    try {
      const { lazy, createRoot } = await makeLazy({ settingsOpen, renderFn });

      lazy.openSettingsPage();
      await flush();

      expect(createRoot).toHaveBeenCalledWith(el);
      expect(renderFn).toHaveBeenCalledTimes(1);
      expect(settingsOpen).toHaveBeenCalledTimes(1);
    } finally {
      document.body.removeChild(el);
    }
  });

  it('openSettingsPage called multiple times mounts React root only once', async () => {
    const settingsOpen = vi.fn();
    const renderFn = vi.fn();
    const el = document.createElement('div');
    el.id = 'settings-page';
    document.body.appendChild(el);

    try {
      const { lazy, createRoot } = await makeLazy({ settingsOpen, renderFn });

      lazy.openSettingsPage();
      lazy.openSettingsPage();
      lazy.openSettingsPage();
      await flush();

      // createRoot / render called exactly once regardless of open count
      expect(createRoot).toHaveBeenCalledTimes(1);
      expect(renderFn).toHaveBeenCalledTimes(1);
      // Only the last open intent is forwarded (generation counter cancels earlier ones)
      expect(settingsOpen).toHaveBeenCalledTimes(1);
    } finally {
      document.body.removeChild(el);
    }
  });

  // ── prefetch does NOT mount ────────────────────────────────────────────────

  it('prefetchPageChunks downloads chunks but does not mount React roots', async () => {
    // Make requestIdleCallback fire synchronously so we don't need a real timer.
    const origRIC = (globalThis as Record<string, unknown>).requestIdleCallback;
    (globalThis as Record<string, unknown>).requestIdleCallback = (cb: () => void) => {
      cb();
      return 0;
    };

    try {
      const { lazy, createRoot } = await makeLazy();

      lazy.prefetchPageChunks();
      await flush();

      // Chunks downloaded (dynamic imports resolved), but no React root created
      expect(createRoot).not.toHaveBeenCalled();
    } finally {
      if (origRIC !== undefined) {
        (globalThis as Record<string, unknown>).requestIdleCallback = origRIC;
      } else {
        delete (globalThis as Record<string, unknown>).requestIdleCallback;
      }
    }
  });

  it('open after prefetch mounts exactly once', async () => {
    const origRIC = (globalThis as Record<string, unknown>).requestIdleCallback;
    (globalThis as Record<string, unknown>).requestIdleCallback = (cb: () => void) => {
      cb();
      return 0;
    };

    const el = document.createElement('div');
    el.id = 'settings-page';
    document.body.appendChild(el);

    try {
      const { lazy, createRoot, renderFn } = await makeLazy();

      // prefetch first — should not mount
      lazy.prefetchPageChunks();
      await flush();
      expect(createRoot).not.toHaveBeenCalled();

      // first open — should mount once
      lazy.openSettingsPage();
      await flush();
      expect(createRoot).toHaveBeenCalledTimes(1);
      expect(renderFn).toHaveBeenCalledTimes(1);

      // second open — no additional mount
      lazy.openSettingsPage();
      await flush();
      expect(createRoot).toHaveBeenCalledTimes(1);
    } finally {
      document.body.removeChild(el);
      if (origRIC !== undefined) {
        (globalThis as Record<string, unknown>).requestIdleCallback = origRIC;
      } else {
        delete (globalThis as Record<string, unknown>).requestIdleCallback;
      }
    }
  });

  // ── close after open works ────────────────────────────────────────────────

  it('closeSettingsPage after open forwards to module closeSettingsPage', async () => {
    const settingsClose = vi.fn();
    const el = document.createElement('div');
    el.id = 'settings-page';
    document.body.appendChild(el);

    try {
      const { lazy } = await makeLazy({ settingsClose });

      lazy.openSettingsPage();
      await flush();

      lazy.closeSettingsPage();
      await flush();

      expect(settingsClose).toHaveBeenCalledTimes(1);
    } finally {
      document.body.removeChild(el);
    }
  });

  // ── chunk rejection clears the cache so the next open() can retry ─────────

  it('clears settingsChunk on rejection so the next openSettingsPage retries', async () => {
    // createRoot is a static import in lazy.ts, so lazy.ts binds the mock at
    // import time. We share one createRootFn across both phases so we can
    // track the call count on the actual lazy module instance.
    const renderFn = vi.fn();
    const createRootFn = vi.fn(() => ({ render: renderFn }));

    // Phase 1: SettingsPage import fails — simulates a network error.
    vi.doMock('../src/pages/SettingsPage.js', () => {
      throw new Error('network error');
    });
    vi.doMock('react-dom/client', () => ({ createRoot: createRootFn }));
    vi.doMock('react', () => ({ default: { createElement: vi.fn(() => null) } }));
    vi.doMock('../src/pages/UsagePage.js', () => ({
      UsagePage: vi.fn(),
      openUsagePage: vi.fn(),
      closeUsagePage: vi.fn(),
    }));

    // Import ONE lazy instance — this is the same instance used in Phase 2.
    const lazy = await import('../src/pages/lazy.js');

    lazy.openSettingsPage();
    await flush();

    // Failed import → no mount, no open forwarding.
    expect(createRootFn).not.toHaveBeenCalled();

    // Phase 2: update the SettingsPage mock to succeed, WITHOUT resetting
    // modules. lazy.ts set settingsChunk = null after the rejection, so
    // the next getSettingsChunk() call retries the dynamic import and picks
    // up this new factory.
    const settingsOpen = vi.fn();
    vi.doMock('../src/pages/SettingsPage.js', () => ({
      SettingsPage: vi.fn(),
      openSettingsPage: settingsOpen,
      closeSettingsPage: vi.fn(),
    }));

    const el = document.createElement('div');
    el.id = 'settings-page';
    document.body.appendChild(el);

    try {
      // Second open on the SAME lazy instance — settingsChunk is null, so
      // getSettingsChunk() retries the import (now succeeds).
      lazy.openSettingsPage();
      await flush();

      expect(createRootFn).toHaveBeenCalledTimes(1);
      expect(renderFn).toHaveBeenCalledTimes(1);
      expect(settingsOpen).toHaveBeenCalledTimes(1);
    } finally {
      document.body.removeChild(el);
    }
  });
  // ── open-then-close race: close wins ──────────────────────────────────────
  // Regression test for the promise-ordering race: `closeSettingsPage` queues
  // on `settingsChunk` (P1), while `openSettingsPage` queues on `loadSettings()`
  // (P2, derived from P1). Without the generation counter, P2 resolves AFTER
  // P1's close callback, causing the open to re-set `pendingOpen = true` and
  // the overlay to reopen unexpectedly.

  it('close called after open but before chunk load prevents the open from firing', async () => {
    const settingsOpen = vi.fn();
    const settingsClose = vi.fn();
    const renderFn = vi.fn();
    const el = document.createElement('div');
    el.id = 'settings-page';
    document.body.appendChild(el);

    try {
      const { lazy } = await makeLazy({ settingsOpen, settingsClose, renderFn });

      lazy.openSettingsPage(); // gen=1 — queued on loadSettings() (P2)
      lazy.closeSettingsPage(); // gen incremented to 2 — queued on settingsChunk (P1)
      await flush();

      // React root is still mounted (close doesn't prevent mounting).
      // But the open callback is NOT forwarded because generation mismatches.
      expect(settingsOpen).not.toHaveBeenCalled();
      // Close IS forwarded (via settingsChunk).
      expect(settingsClose).toHaveBeenCalledTimes(1);
    } finally {
      document.body.removeChild(el);
    }
  });

  // ── openUsagePage symmetric coverage ─────────────────────────────────────

  it('openUsagePage mounts React root and calls module openUsagePage', async () => {
    const usageOpen = vi.fn();
    const renderFn = vi.fn();
    const el = document.createElement('div');
    el.id = 'usage-page';
    document.body.appendChild(el);

    try {
      const { lazy, createRoot } = await makeLazy({ usageOpen, renderFn });

      lazy.openUsagePage();
      await flush();

      expect(createRoot).toHaveBeenCalledWith(el);
      expect(renderFn).toHaveBeenCalledTimes(1);
      expect(usageOpen).toHaveBeenCalledTimes(1);
    } finally {
      document.body.removeChild(el);
    }
  });

  it('close usage called after open but before chunk load prevents the open from firing', async () => {
    const usageOpen = vi.fn();
    const usageClose = vi.fn();
    const el = document.createElement('div');
    el.id = 'usage-page';
    document.body.appendChild(el);

    try {
      const { lazy } = await makeLazy({ usageOpen, usageClose });

      lazy.openUsagePage();
      lazy.closeUsagePage();
      await flush();

      expect(usageOpen).not.toHaveBeenCalled();
      expect(usageClose).toHaveBeenCalledTimes(1);
    } finally {
      document.body.removeChild(el);
    }
  });
});
