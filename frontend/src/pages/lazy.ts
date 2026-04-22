// Lazy-loaded bridges for the SettingsPage and UsagePage React islands.
// main.ts imports these thin wrappers instead of the page modules directly,
// so the (large) page bundles are code-split into their own chunks and only
// fetched the first time the user actually opens those overlays.
//
// Design:
//   - Chunk promises cache the dynamic import. On rejection the promise is
//     cleared so a transient network error doesn't permanently disable the
//     overlay — the next open() attempt retries the fetch.
//   - Mount guards (settingsMounted / usageMounted) ensure createRoot() is
//     called exactly once per page load, decoupled from chunk prefetching.
//   - prefetchPageChunks() downloads chunks during idle time but does NOT
//     mount the React root — mount is deferred to the first open() call.
//
// The page modules buffer a `pendingOpen` flag, so calling openSettingsPage()
// before the chunk has finished loading is safe: the component opens itself
// as soon as its mount effect runs.

import React from 'react';
import { createRoot } from 'react-dom/client';

type SettingsModule = typeof import('./SettingsPage.js');
type UsageModule = typeof import('./UsagePage.js');

// ── Chunk download ────────────────────────────────────────────────────────────
// Kept separate from mounting so prefetch has no side effects.
// Cleared on rejection so the next open() attempt retries.

let settingsChunk: Promise<SettingsModule> | null = null;
let usageChunk: Promise<UsageModule> | null = null;

function getSettingsChunk(): Promise<SettingsModule> {
  if (!settingsChunk) {
    settingsChunk = import('./SettingsPage.js').catch((e: unknown) => {
      settingsChunk = null; // allow retry
      throw e;
    });
  }
  return settingsChunk;
}

function getUsageChunk(): Promise<UsageModule> {
  if (!usageChunk) {
    usageChunk = import('./UsagePage.js').catch((e: unknown) => {
      usageChunk = null; // allow retry
      throw e;
    });
  }
  return usageChunk;
}

// ── React root mount (once per page load) ────────────────────────────────────
// Guards prevent double-createRoot regardless of how many open() calls race
// before the first mount completes.

let settingsMounted = false;
let usageMounted = false;

function loadSettings(): Promise<SettingsModule> {
  return getSettingsChunk().then((mod) => {
    if (!settingsMounted) {
      settingsMounted = true;
      const el = document.getElementById('settings-page');
      if (el) createRoot(el as Element).render(React.createElement(mod.SettingsPage));
    }
    return mod;
  });
}

function loadUsage(): Promise<UsageModule> {
  return getUsageChunk().then((mod) => {
    if (!usageMounted) {
      usageMounted = true;
      const el = document.getElementById('usage-page');
      if (el) createRoot(el as Element).render(React.createElement(mod.UsagePage));
    }
    return mod;
  });
}

// ── Intent generation counters ────────────────────────────────────────────────
// Promise ordering: `closeSettingsPage` queues on `settingsChunk` (P1), while
// `openSettingsPage` queues on `loadSettings()` — a *derived* promise
// (P1 → mount → P2). P2 resolves after all of P1's own callbacks, so a close
// call made *before* the chunk lands fires before the corresponding open.
// Without a guard, the open callback fires last and sets `pendingOpen = true`
// in SettingsPage.tsx — causing the overlay to reopen despite the user having
// pressed Escape.
//
// Fix: each open call captures the generation counter at the time of the call.
// The `.then` callback verifies the counter still matches before forwarding.
// Close increments the counter, invalidating any pending open.
// Concretely: the *last* open intent wins ("last intent" UX).

let settingsOpenGeneration = 0;
let usageOpenGeneration = 0;

// ── Public bridge ─────────────────────────────────────────────────────────────

export function openSettingsPage(): void {
  const gen = ++settingsOpenGeneration;
  void loadSettings()
    .then((mod) => {
      if (settingsOpenGeneration !== gen) return; // cancelled by a later close or open
      mod.openSettingsPage();
    })
    .catch(() => {
      // Chunk load failed (e.g. network error). The user's click had no effect;
      // settingsChunk was cleared by the rejection handler so the next attempt
      // will retry the fetch.
    });
}

export function closeSettingsPage(): void {
  settingsOpenGeneration++; // invalidate any pending open
  // Use the raw chunk promise (not loadSettings) so close never triggers mount.
  // .catch() prevents unhandled rejections if the chunk failed to load.
  if (settingsChunk) void settingsChunk.then((mod) => mod.closeSettingsPage()).catch(() => {});
}

export function openUsagePage(): void {
  const gen = ++usageOpenGeneration;
  void loadUsage()
    .then((mod) => {
      if (usageOpenGeneration !== gen) return;
      mod.openUsagePage();
    })
    .catch(() => {
      // Chunk load failed; usageChunk cleared for retry on next attempt.
    });
}

export function closeUsagePage(): void {
  usageOpenGeneration++;
  if (usageChunk) void usageChunk.then((mod) => mod.closeUsagePage()).catch(() => {});
}

// ── Idle prefetch ─────────────────────────────────────────────────────────────
// Downloads chunks only — React roots are NOT mounted until first open().

type IdleCb = (cb: () => void) => number;
const idle: IdleCb =
  typeof (globalThis as { requestIdleCallback?: IdleCb }).requestIdleCallback === 'function'
    ? (globalThis as { requestIdleCallback: IdleCb }).requestIdleCallback
    : (cb) => setTimeout(cb, 200) as unknown as number;

export function prefetchPageChunks(): void {
  idle(() => {
    // .catch() prevents unhandled rejections on transient network errors;
    // prefetch is fire-and-forget. The chunk cache is cleared on rejection
    // so the next open() will retry.
    void getSettingsChunk().catch(() => {});
    void getUsageChunk().catch(() => {});
  });
}
