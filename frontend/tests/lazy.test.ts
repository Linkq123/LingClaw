/** Tests for the single-root Console lazy bridge. */

import { beforeEach, describe, expect, it, vi } from 'vitest';

type MockFn = ReturnType<typeof vi.fn>;

async function makeLazy({
  settingsOpen = vi.fn(),
  usageOpen = vi.fn(),
  consoleClose = vi.fn(),
  renderFn = vi.fn(),
}: {
  settingsOpen?: MockFn;
  usageOpen?: MockFn;
  consoleClose?: MockFn;
  renderFn?: MockFn;
} = {}) {
  const createRoot = vi.fn(() => ({ render: renderFn }));

  vi.doMock('../src/pages/SettingsPage.js', () => ({
    SettingsPage: vi.fn(),
    openSettingsPage: settingsOpen,
    openUsageConsolePage: usageOpen,
    closeConsolePage: consoleClose,
  }));
  vi.doMock('react-dom/client', () => ({ createRoot }));
  vi.doMock('react', () => ({ default: { createElement: vi.fn(() => null) } }));

  const lazy = await import('../src/pages/lazy.js');
  return { lazy, createRoot, renderFn };
}

function addConsoleHost(): HTMLDivElement {
  const host = document.createElement('div');
  host.id = 'console-page';
  document.body.appendChild(host);
  return host;
}

async function flush(): Promise<void> {
  await vi.dynamicImportSettled();
  await new Promise((resolve) => setTimeout(resolve, 0));
  await Promise.resolve();
}

describe('lazy Console bridge', () => {
  beforeEach(() => {
    vi.resetModules();
    vi.clearAllMocks();
    document.body.innerHTML = '';
  });

  it('close aliases are no-ops before the Console chunk is loaded', async () => {
    const consoleClose = vi.fn();
    const { lazy, createRoot } = await makeLazy({ consoleClose });

    expect(() => lazy.closeConsolePage()).not.toThrow();
    expect(() => lazy.closeSettingsPage()).not.toThrow();
    expect(() => lazy.closeUsagePage()).not.toThrow();
    await flush();

    expect(createRoot).not.toHaveBeenCalled();
    expect(consoleClose).not.toHaveBeenCalled();
  });

  it('mounts one Console root and forwards Settings arguments', async () => {
    const host = addConsoleHost();
    const settingsOpen = vi.fn();
    const renderFn = vi.fn();
    const { lazy, createRoot } = await makeLazy({ settingsOpen, renderFn });

    lazy.openSettingsPage('research', 'tab-models');
    await flush();

    expect(createRoot).toHaveBeenCalledWith(host);
    expect(renderFn).toHaveBeenCalledTimes(1);
    expect(settingsOpen).toHaveBeenCalledWith('research', 'tab-models');
  });

  it('preserves the legacy one-argument Settings call shape', async () => {
    addConsoleHost();
    const settingsOpen = vi.fn();
    const { lazy } = await makeLazy({ settingsOpen });

    lazy.openSettingsPage('research');
    await flush();

    expect(settingsOpen).toHaveBeenCalledWith('research');
  });

  it('opens Usage through the same mounted Console root', async () => {
    const host = addConsoleHost();
    const settingsOpen = vi.fn();
    const usageOpen = vi.fn();
    const { lazy, createRoot, renderFn } = await makeLazy({ settingsOpen, usageOpen });

    lazy.openSettingsPage('main');
    await flush();
    lazy.openUsagePage('research');
    await flush();

    expect(createRoot).toHaveBeenCalledTimes(1);
    expect(createRoot).toHaveBeenCalledWith(host);
    expect(renderFn).toHaveBeenCalledTimes(1);
    expect(settingsOpen).toHaveBeenCalledWith('main');
    expect(usageOpen).toHaveBeenCalledWith('research');
  });

  it('only forwards the latest cross-page open intent', async () => {
    addConsoleHost();
    const settingsOpen = vi.fn();
    const usageOpen = vi.fn();
    const { lazy } = await makeLazy({ settingsOpen, usageOpen });

    lazy.openSettingsPage('main');
    lazy.openUsagePage('research');
    await flush();

    expect(settingsOpen).not.toHaveBeenCalled();
    expect(usageOpen).toHaveBeenCalledWith('research');
  });

  it('retries mounting when an early open happens before the host exists', async () => {
    const settingsOpen = vi.fn();
    const { lazy, createRoot } = await makeLazy({ settingsOpen });

    lazy.openSettingsPage('main');
    await flush();
    expect(createRoot).not.toHaveBeenCalled();
    expect(settingsOpen).not.toHaveBeenCalled();

    const host = addConsoleHost();
    lazy.openSettingsPage('main');
    await flush();

    expect(createRoot).toHaveBeenCalledWith(host);
    expect(settingsOpen).toHaveBeenCalledWith('main');
  });

  it('prefetches the Console chunk without mounting it', async () => {
    const original = (globalThis as { requestIdleCallback?: (callback: () => void) => number })
      .requestIdleCallback;
    (globalThis as { requestIdleCallback?: (callback: () => void) => number }).requestIdleCallback =
      (callback) => {
        callback();
        return 0;
      };

    try {
      const { lazy, createRoot } = await makeLazy();
      lazy.prefetchPageChunks();
      await flush();
      expect(createRoot).not.toHaveBeenCalled();
    } finally {
      if (original) {
        (globalThis as { requestIdleCallback?: typeof original }).requestIdleCallback = original;
      } else {
        delete (globalThis as { requestIdleCallback?: typeof original }).requestIdleCallback;
      }
    }
  });

  it('all close exports forward to the unified close operation after loading', async () => {
    addConsoleHost();
    const consoleClose = vi.fn();
    const { lazy } = await makeLazy({ consoleClose });

    lazy.openSettingsPage();
    await flush();
    lazy.closeSettingsPage();
    lazy.closeUsagePage();
    lazy.closeConsolePage();
    await flush();

    expect(consoleClose).toHaveBeenCalledTimes(3);
  });

  it('a close issued during chunk loading cancels the pending open', async () => {
    addConsoleHost();
    const settingsOpen = vi.fn();
    const consoleClose = vi.fn();
    const { lazy } = await makeLazy({ settingsOpen, consoleClose });

    lazy.openSettingsPage();
    lazy.closeConsolePage();
    await flush();

    expect(settingsOpen).not.toHaveBeenCalled();
    expect(consoleClose).toHaveBeenCalledTimes(1);
  });

  it('clears a rejected chunk so a later open can retry', async () => {
    const renderFn = vi.fn();
    const createRoot = vi.fn(() => ({ render: renderFn }));
    vi.doMock('../src/pages/SettingsPage.js', () => {
      throw new Error('network error');
    });
    vi.doMock('react-dom/client', () => ({ createRoot }));
    vi.doMock('react', () => ({ default: { createElement: vi.fn(() => null) } }));

    const lazy = await import('../src/pages/lazy.js');
    addConsoleHost();
    lazy.openSettingsPage();
    await flush();
    expect(createRoot).not.toHaveBeenCalled();

    const settingsOpen = vi.fn();
    vi.doMock('../src/pages/SettingsPage.js', () => ({
      SettingsPage: vi.fn(),
      openSettingsPage: settingsOpen,
      openUsageConsolePage: vi.fn(),
      closeConsolePage: vi.fn(),
    }));

    lazy.openSettingsPage();
    await flush();

    expect(createRoot).toHaveBeenCalledTimes(1);
    expect(renderFn).toHaveBeenCalledTimes(1);
    expect(settingsOpen).toHaveBeenCalledTimes(1);
  });
});
