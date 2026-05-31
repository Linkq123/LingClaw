export const ACTIVE_SESSION_STORAGE_KEY = 'lingclaw.activeSessionId';

export function loadActiveSessionId(): string {
  try {
    return globalThis.localStorage?.getItem(ACTIVE_SESSION_STORAGE_KEY)?.trim() || '';
  } catch {
    return '';
  }
}

export function persistActiveSessionId(sessionId: string): void {
  const normalized = sessionId.trim();
  try {
    if (normalized) {
      globalThis.localStorage?.setItem(ACTIVE_SESSION_STORAGE_KEY, normalized);
    } else {
      globalThis.localStorage?.removeItem(ACTIVE_SESSION_STORAGE_KEY);
    }
  } catch {
    // ignore local persistence failures
  }
}
