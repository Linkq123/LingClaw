// Lazy bridge for the unified full-screen Console. Settings and Usage share a
// single React root so switching between them keeps visited forms, filters,
// and unsaved drafts alive.

import React from 'react';
import { createRoot } from 'react-dom/client';

type ConsoleModule = typeof import('./SettingsPage.js');
export type SettingsSection = import('./SettingsPage.js').SettingsSection;
export type ConsoleRoute = import('./SettingsPage.js').ConsoleRoute;

let consoleChunk: Promise<ConsoleModule> | null = null;
let consoleMounted = false;
let consoleIntentGeneration = 0;

function getConsoleChunk(): Promise<ConsoleModule> {
  if (!consoleChunk) {
    consoleChunk = import('./SettingsPage.js').catch((error: unknown) => {
      consoleChunk = null;
      throw error;
    });
  }
  return consoleChunk;
}

function loadConsole(): Promise<ConsoleModule> {
  return getConsoleChunk().then((module) => {
    if (!consoleMounted) {
      const host = document.getElementById('console-page');
      if (!host) throw new Error('Console host #console-page is not available');
      createRoot(host).render(React.createElement(module.SettingsPage));
      consoleMounted = true;
    }
    return module;
  });
}

function runOpenIntent(open: (module: ConsoleModule) => void): void {
  const generation = ++consoleIntentGeneration;
  void loadConsole()
    .then((module) => {
      if (consoleIntentGeneration !== generation) return;
      open(module);
    })
    .catch(() => {
      // A failed dynamic import clears the chunk promise. The next user action
      // retries without leaving a permanently broken Console entry point.
    });
}

export function openSettingsPage(sessionId?: string, initialSection?: SettingsSection): void {
  runOpenIntent((module) => {
    if (initialSection === undefined) {
      module.openSettingsPage(sessionId);
      return;
    }
    module.openSettingsPage(sessionId, initialSection);
  });
}

export function openUsagePage(sessionId?: string): void {
  runOpenIntent((module) => module.openUsageConsolePage(sessionId));
}

export function closeConsolePage(): void {
  consoleIntentGeneration += 1;
  if (consoleChunk) void consoleChunk.then((module) => module.closeConsolePage()).catch(() => {});
}

// Compatibility exports used by existing action handlers and tests.
export const closeSettingsPage = closeConsolePage;
export const closeUsagePage = closeConsolePage;

type IdleCallback = (callback: () => void) => number;
const idle: IdleCallback =
  typeof (globalThis as { requestIdleCallback?: IdleCallback }).requestIdleCallback === 'function'
    ? (globalThis as { requestIdleCallback: IdleCallback }).requestIdleCallback
    : (callback) => setTimeout(callback, 200) as unknown as number;

export function prefetchPageChunks(): void {
  idle(() => {
    void getConsoleChunk().catch(() => {});
  });
}
