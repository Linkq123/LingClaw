import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  CONSOLE_TRANSITION_CLASSES,
  createConsoleTransitionController,
  prefersReducedConsoleMotion,
  supportsConsoleViewTransition,
} from '../src/pages/consoleTransition.js';
import {
  invalidateChatScrollCache,
  resumeChatScrollTracking,
  scrollDown,
  suspendChatScrollTracking,
} from '../src/scroll.js';
import { dom, state } from '../src/state.js';
import { isConsoleSurfaceActive } from '../src/workspacePortal.js';

interface Deferred<T> {
  promise: Promise<T>;
  resolve(value: T): void;
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

function setReducedMotion(matches: boolean): void {
  Object.defineProperty(window, 'matchMedia', {
    configurable: true,
    value: vi.fn().mockReturnValue({ matches }),
  });
}

function elements(): {
  workspace: HTMLElement;
  workspacePortalRoot: HTMLElement;
  consolePage: HTMLElement;
  opener: HTMLButtonElement;
  portalButton: HTMLButtonElement;
  title: HTMLHeadingElement;
  chat: HTMLElement;
} {
  const workspace = document.getElementById('app-workspace') as HTMLElement;
  const workspacePortalRoot = document.getElementById('workspace-portal-root') as HTMLElement;
  const consolePage = document.getElementById('console-page') as HTMLElement;
  const opener = document.getElementById('open-console') as HTMLButtonElement;
  const portalButton = document.getElementById('workspace-portal-button') as HTMLButtonElement;
  const title = document.getElementById('console-title') as HTMLHeadingElement;
  const chat = document.getElementById('chat') as HTMLElement;
  return { workspace, workspacePortalRoot, consolePage, opener, portalButton, title, chat };
}

describe('console transition controller', () => {
  beforeEach(() => {
    document.documentElement.className = '';
    document.body.innerHTML = `
      <main id="app-workspace">
        <button id="open-console">Open console</button>
        <div id="chat"></div>
      </main>
      <div id="workspace-portal-root">
        <button id="workspace-portal-button">Workspace portal action</button>
      </div>
      <section id="console-page" hidden>
        <h1 id="console-title">Console</h1>
      </section>
    `;
    dom.chat = document.getElementById('chat') as HTMLElement;
    state.autoFollowChat = true;
    state.hasBufferedChatUpdates = false;
    state.unreadMessageCount = 0;
    state.bulkRenderingChat = false;
    setReducedMotion(false);
    delete (document as Document & { startViewTransition?: unknown }).startViewTransition;
  });

  afterEach(() => {
    resumeChatScrollTracking();
    vi.useRealTimers();
  });

  it('synchronizes hidden, inert and aria-hidden state on creation', () => {
    const { workspace, consolePage } = elements();

    createConsoleTransitionController({ workspace, consolePage }, { fallbackDurationMs: 0 });

    expect(workspace.hidden).toBe(false);
    expect(workspace.inert).toBe(false);
    expect(workspace.hasAttribute('aria-hidden')).toBe(false);
    expect(consolePage.hidden).toBe(true);
    expect(consolePage.inert).toBe(true);
    expect(consolePage.getAttribute('aria-hidden')).toBe('true');
  });

  it('reports whether the Console surface is currently exposed', () => {
    const { consolePage } = elements();

    expect(isConsoleSurfaceActive()).toBe(false);
    consolePage.hidden = false;
    expect(isConsoleSurfaceActive()).toBe(true);
  });

  it('bypasses all animation for reduced motion and restores captured focus', async () => {
    setReducedMotion(true);
    const { workspace, consolePage, opener, title } = elements();
    const startViewTransition = vi.fn();
    (
      document as Document & { startViewTransition: typeof startViewTransition }
    ).startViewTransition = startViewTransition;
    const controller = createConsoleTransitionController({ workspace, consolePage });
    opener.focus();

    await expect(controller.showConsole({ focusTarget: title })).resolves.toBe(true);

    expect(startViewTransition).not.toHaveBeenCalled();
    expect(workspace.hidden).toBe(true);
    expect(workspace.inert).toBe(true);
    expect(workspace.getAttribute('aria-hidden')).toBe('true');
    expect(consolePage.hidden).toBe(false);
    expect(consolePage.inert).toBe(false);
    expect(document.activeElement).toBe(title);
    expect(document.documentElement.classList.contains(CONSOLE_TRANSITION_CLASSES.active)).toBe(
      true,
    );
    expect(
      document.documentElement.classList.contains(CONSOLE_TRANSITION_CLASSES.transitioning),
    ).toBe(false);

    await expect(controller.showWorkspace()).resolves.toBe(true);

    expect(document.activeElement).toBe(opener);
    expect(workspace.hidden).toBe(false);
    expect(consolePage.hidden).toBe(true);
    expect(document.documentElement.classList.contains(CONSOLE_TRANSITION_CLASSES.active)).toBe(
      false,
    );
  });

  it('isolates workspace portals and restores focus captured inside them', async () => {
    setReducedMotion(true);
    const { workspace, workspacePortalRoot, consolePage, portalButton, title } = elements();
    const controller = createConsoleTransitionController({
      workspace,
      workspacePortalRoot,
      consolePage,
    });
    portalButton.focus();

    await controller.showConsole({ focusTarget: title });

    expect(workspacePortalRoot.hidden).toBe(true);
    expect(workspacePortalRoot.inert).toBe(true);
    expect(workspacePortalRoot.getAttribute('aria-hidden')).toBe('true');
    expect(document.activeElement).toBe(title);

    await controller.showWorkspace();

    expect(workspacePortalRoot.hidden).toBe(false);
    expect(workspacePortalRoot.inert).toBe(false);
    expect(workspacePortalRoot.hasAttribute('aria-hidden')).toBe(false);
    expect(document.activeElement).toBe(portalButton);
  });

  it('uses the native View Transition API when it is available', async () => {
    const { workspace, consolePage, title } = elements();
    let classesDuringUpdate: string[] = [];
    const startViewTransition = vi.fn((update: () => void) => {
      classesDuringUpdate = Array.from(document.documentElement.classList);
      update();
      return {
        updateCallbackDone: Promise.resolve(),
        finished: Promise.resolve(),
      };
    });
    (
      document as Document & { startViewTransition: typeof startViewTransition }
    ).startViewTransition = startViewTransition;
    const controller = createConsoleTransitionController({ workspace, consolePage });

    await expect(controller.showConsole({ focusTarget: title })).resolves.toBe(true);

    expect(startViewTransition).toHaveBeenCalledTimes(1);
    expect(classesDuringUpdate).toContain(CONSOLE_TRANSITION_CLASSES.transitioning);
    expect(classesDuringUpdate).toContain(CONSOLE_TRANSITION_CLASSES.entering);
    expect(classesDuringUpdate).not.toContain(CONSOLE_TRANSITION_CLASSES.fallback);
    expect(document.activeElement).toBe(title);
    expect(
      document.documentElement.classList.contains(CONSOLE_TRANSITION_CLASSES.transitioning),
    ).toBe(false);
  });

  it('uses directional CSS fallback classes when native transitions are unavailable', async () => {
    vi.useFakeTimers();
    const { workspace, consolePage } = elements();
    const controller = createConsoleTransitionController(
      { workspace, consolePage },
      { fallbackDurationMs: 220 },
    );

    const transition = controller.showConsole();

    expect(controller.surface).toBe('console');
    expect(document.documentElement.classList.contains(CONSOLE_TRANSITION_CLASSES.fallback)).toBe(
      true,
    );
    expect(document.documentElement.classList.contains(CONSOLE_TRANSITION_CLASSES.entering)).toBe(
      true,
    );
    expect(document.documentElement.classList.contains(CONSOLE_TRANSITION_CLASSES.leaving)).toBe(
      false,
    );

    await vi.advanceTimersByTimeAsync(220);
    await expect(transition).resolves.toBe(true);

    expect(document.documentElement.classList.contains(CONSOLE_TRANSITION_CLASSES.fallback)).toBe(
      false,
    );
    expect(
      document.documentElement.classList.contains(CONSOLE_TRANSITION_CLASSES.transitioning),
    ).toBe(false);
  });

  it('focuses the console heading by default and makes it programmatically focusable', async () => {
    setReducedMotion(true);
    const { workspace, consolePage, title } = elements();
    const controller = createConsoleTransitionController({ workspace, consolePage });

    await expect(controller.showConsole()).resolves.toBe(true);

    expect(title.getAttribute('tabindex')).toBe('-1');
    expect(document.activeElement).toBe(title);
  });

  it('restores workspace scroll containers after they are mutated while hidden', async () => {
    setReducedMotion(true);
    const { workspace, consolePage, chat } = elements();
    Object.defineProperty(chat, 'scrollHeight', { configurable: true, value: 1_000 });
    Object.defineProperty(chat, 'clientHeight', { configurable: true, value: 400 });
    chat.scrollTop = 184;
    chat.scrollLeft = 12;
    state.autoFollowChat = false;
    const controller = createConsoleTransitionController({
      workspace,
      consolePage,
      scrollTargets: [chat],
      onBeforeWorkspaceHide: suspendChatScrollTracking,
      onAfterWorkspaceShow: resumeChatScrollTracking,
    });

    await controller.showConsole();
    chat.scrollTop = 0;
    chat.scrollLeft = 0;
    invalidateChatScrollCache();
    expect(scrollDown()).toBe(false);
    expect(state.autoFollowChat).toBe(false);
    await controller.showWorkspace();

    expect(chat.scrollTop).toBe(184);
    expect(chat.scrollLeft).toBe(12);
  });

  it('keeps auto-follow suspended and resumes at the latest content', async () => {
    setReducedMotion(true);
    const { workspace, consolePage, chat } = elements();
    let scrollHeight = 1_000;
    Object.defineProperty(chat, 'scrollHeight', {
      configurable: true,
      get: () => scrollHeight,
    });
    Object.defineProperty(chat, 'clientHeight', { configurable: true, value: 400 });
    chat.scrollTop = 600;
    state.autoFollowChat = true;
    const controller = createConsoleTransitionController({
      workspace,
      consolePage,
      scrollTargets: [chat],
      onBeforeWorkspaceHide: suspendChatScrollTracking,
      onAfterWorkspaceShow: resumeChatScrollTracking,
    });

    await controller.showConsole();
    scrollHeight = 1_200;
    invalidateChatScrollCache();
    expect(scrollDown()).toBe(true);
    expect(chat.scrollTop).toBe(600);
    await controller.showWorkspace();

    expect(state.autoFollowChat).toBe(true);
    expect(chat.scrollTop).toBe(1_200);
  });

  it('keeps following when the scroll state lags behind near-bottom geometry', async () => {
    setReducedMotion(true);
    const { workspace, consolePage, chat } = elements();
    let scrollHeight = 1_000;
    Object.defineProperty(chat, 'scrollHeight', {
      configurable: true,
      get: () => scrollHeight,
    });
    Object.defineProperty(chat, 'clientHeight', { configurable: true, value: 400 });
    chat.scrollTop = 600;
    state.autoFollowChat = false;
    const controller = createConsoleTransitionController({
      workspace,
      consolePage,
      scrollTargets: [chat],
      onBeforeWorkspaceHide: suspendChatScrollTracking,
      onAfterWorkspaceShow: resumeChatScrollTracking,
    });

    await controller.showConsole();
    scrollHeight = 1_200;
    expect(scrollDown()).toBe(true);
    await controller.showWorkspace();

    expect(state.autoFollowChat).toBe(true);
    expect(chat.scrollTop).toBe(1_200);
  });

  it('ignores a stale native transition update after a newer navigation intent', async () => {
    const { workspace, consolePage, opener } = elements();
    const updateDone = deferred<void>();
    const finished = deferred<void>();
    let pendingUpdate: (() => void) | null = null;
    (
      document as Document & {
        startViewTransition: (update: () => void) => {
          updateCallbackDone: Promise<void>;
          finished: Promise<void>;
        };
      }
    ).startViewTransition = (update) => {
      pendingUpdate = update;
      return { updateCallbackDone: updateDone.promise, finished: finished.promise };
    };
    const controller = createConsoleTransitionController({ workspace, consolePage });
    opener.focus();

    const staleIntent = controller.showConsole();
    const latestIntent = controller.showWorkspace();
    pendingUpdate?.();
    updateDone.resolve();
    finished.resolve();

    await expect(latestIntent).resolves.toBe(true);
    await expect(staleIntent).resolves.toBe(false);
    expect(controller.surface).toBe('workspace');
    expect(controller.desiredSurface).toBe('workspace');
    expect(workspace.hidden).toBe(false);
    expect(consolePage.hidden).toBe(true);
    expect(document.activeElement).toBe(opener);
    expect(document.documentElement.className).toBe('');
  });

  it('does not restore focus to an opener that was removed while the console was open', async () => {
    setReducedMotion(true);
    const { workspace, consolePage, opener, title } = elements();
    const controller = createConsoleTransitionController({ workspace, consolePage });
    opener.focus();
    await controller.showConsole({ focusTarget: title });
    opener.remove();

    await expect(controller.showWorkspace()).resolves.toBe(true);

    expect(document.activeElement).toBe(workspace);
    expect(workspace.getAttribute('tabindex')).toBe('-1');
    expect(workspace.inert).toBe(false);
  });

  it('falls back to the composer when the captured opener is disabled while hidden', async () => {
    setReducedMotion(true);
    const { workspace, consolePage, opener, title } = elements();
    const composer = document.createElement('textarea');
    composer.id = 'input';
    workspace.append(composer);
    const controller = createConsoleTransitionController({ workspace, consolePage });
    opener.focus();
    await controller.showConsole({ focusTarget: title });
    opener.disabled = true;

    await expect(controller.showWorkspace()).resolves.toBe(true);

    expect(document.activeElement).toBe(composer);
  });

  it('invalidates pending work and removes owned classes when disposed', async () => {
    vi.useFakeTimers();
    const { workspace, consolePage } = elements();
    const controller = createConsoleTransitionController(
      { workspace, consolePage },
      { fallbackDurationMs: 220 },
    );
    const transition = controller.showConsole();

    controller.dispose();
    await vi.advanceTimersByTimeAsync(220);

    await expect(transition).resolves.toBe(false);
    expect(document.documentElement.className).toBe('');
    expect(workspace.hidden).toBe(false);
    expect(consolePage.hidden).toBe(true);
    await expect(controller.showWorkspace()).resolves.toBe(false);
  });

  it('exposes feature detection helpers', () => {
    expect(prefersReducedConsoleMotion(window)).toBe(false);
    expect(supportsConsoleViewTransition(document)).toBe(false);

    (document as Document & { startViewTransition: () => object }).startViewTransition = () => ({});
    expect(supportsConsoleViewTransition(document)).toBe(true);
  });
});
