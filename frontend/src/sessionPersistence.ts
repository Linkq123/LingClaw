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

export interface GroupControlSessionState {
  activeSessionId: string;
  groupReturnSessionId: string;
}

export interface GroupRunStatusSnapshot {
  status: string;
  updatedAt: number;
}

export function mainSessionStateForGroupControl(
  activeSessionId: string,
  currentReturnSessionId = '',
): GroupControlSessionState {
  const previousSessionId = activeSessionId.trim();
  const existingReturnSessionId = currentReturnSessionId.trim();
  const isMainSession = previousSessionId.toLowerCase() === 'main';
  return {
    activeSessionId: 'main',
    groupReturnSessionId:
      previousSessionId && !isMainSession ? previousSessionId : existingReturnSessionId,
  };
}

export function sessionIdAfterLeavingGroup(
  groupReturnSessionId: string,
  fallbackSessionId: string,
): string {
  return groupReturnSessionId.trim() || fallbackSessionId.trim() || 'main';
}

export function normalizeGroupRunUpdatedAt(value: unknown): number {
  const updatedAt = Number(value);
  return Number.isFinite(updatedAt) && updatedAt >= 0 ? updatedAt : 0;
}

export function isActiveGroupRunStatus(status: string): boolean {
  return status === 'queued' || status === 'running';
}

export function isTerminalGroupRunStatus(status: string): boolean {
  return status === 'completed' || status === 'failed' || status === 'stopped';
}

export function shouldApplyGroupRunStatusUpdate(
  current: GroupRunStatusSnapshot | undefined,
  status: string,
  updatedAt: number,
): boolean {
  if (!current) return true;
  if (updatedAt < current.updatedAt) return false;
  return !(
    updatedAt === current.updatedAt &&
    isTerminalGroupRunStatus(current.status) &&
    isActiveGroupRunStatus(status)
  );
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
