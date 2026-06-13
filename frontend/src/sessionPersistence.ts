export const ACTIVE_SESSION_STORAGE_KEY = 'lingclaw.activeSessionId';
export const ACTIVE_GROUP_STORAGE_KEY = 'lingclaw.activeGroupId';

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

export function loadActiveGroupId(): string {
  try {
    return globalThis.localStorage?.getItem(ACTIVE_GROUP_STORAGE_KEY)?.trim() || '';
  } catch {
    return '';
  }
}

export function persistActiveGroupId(groupId: string): void {
  const normalized = groupId.trim();
  try {
    if (normalized) {
      globalThis.localStorage?.setItem(ACTIVE_GROUP_STORAGE_KEY, normalized);
    } else {
      globalThis.localStorage?.removeItem(ACTIVE_GROUP_STORAGE_KEY);
    }
  } catch {
    // ignore local persistence failures
  }
}

export function isRecoverableActiveGroupConnectionError(
  content: string,
  activeGroupId: string,
): boolean {
  if (!activeGroupId) return false;
  return (
    content.includes(`Group '${activeGroupId}' not found`) ||
    content.includes('Invalid group id') ||
    content.includes('Invalid session id')
  );
}
