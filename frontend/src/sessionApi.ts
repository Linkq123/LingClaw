import type { SessionGroupDetail, SessionGroupSummary, SessionSummary } from './types.js';

function normalizeSessionSummary(session: unknown): SessionSummary {
  const raw = (session ?? {}) as Record<string, unknown>;
  const id = String(raw.id ?? '').trim();
  if (!id) {
    throw new Error('Created session response did not include an id.');
  }
  return {
    id,
    name: String(raw.name ?? id),
    updated_at: typeof raw.updated_at === 'number' ? raw.updated_at : Number(raw.updated_at ?? 0),
    corrupt: raw.corrupt === true,
  };
}

export async function createSession(): Promise<SessionSummary> {
  const response = await fetch('/api/session', { method: 'POST' });
  const payload = await response.json().catch(() => ({}));
  if (!response.ok) {
    throw new Error(String(payload?.error || `HTTP ${response.status}`));
  }
  return normalizeSessionSummary(payload?.session);
}

function normalizeGroupSummary(group: unknown): SessionGroupSummary {
  const raw = (group ?? {}) as Record<string, unknown>;
  const id = String(raw.id ?? '').trim();
  if (!id) {
    throw new Error('Group response did not include an id.');
  }
  return {
    id,
    name: String(raw.name ?? id),
    members: typeof raw.members === 'number' ? raw.members : Number(raw.members ?? 0),
    messages: typeof raw.messages === 'number' ? raw.messages : Number(raw.messages ?? 0),
    running: typeof raw.running === 'number' ? raw.running : Number(raw.running ?? 0),
    updated_at: typeof raw.updated_at === 'number' ? raw.updated_at : Number(raw.updated_at ?? 0),
    corrupt: raw.corrupt === true,
  };
}

function normalizeGroupDetail(group: unknown): SessionGroupDetail {
  const raw = (group ?? {}) as Record<string, unknown>;
  const id = String(raw.id ?? '').trim();
  if (!id) {
    throw new Error('Group response did not include an id.');
  }
  return {
    id,
    name: String(raw.name ?? id),
    members: Array.isArray(raw.members)
      ? raw.members.map((member) => String(member).trim()).filter(Boolean)
      : [],
    messages: Array.isArray(raw.messages) ? raw.messages : [],
    runs: Array.isArray(raw.runs) ? raw.runs : [],
    created_at: typeof raw.created_at === 'number' ? raw.created_at : Number(raw.created_at ?? 0),
    updated_at: typeof raw.updated_at === 'number' ? raw.updated_at : Number(raw.updated_at ?? 0),
    version: typeof raw.version === 'number' ? raw.version : Number(raw.version ?? 0),
  };
}

export async function getSessionGroup(groupId: string): Promise<SessionGroupDetail> {
  const response = await fetch(`/api/session-group?group=${encodeURIComponent(groupId)}`, {
    cache: 'no-store',
  });
  const payload = await response.json().catch(() => ({}));
  if (!response.ok) {
    throw new Error(String(payload?.error || `HTTP ${response.status}`));
  }
  return normalizeGroupDetail(payload?.group);
}

export async function createSessionGroup(
  name?: string,
  members: string[] = [],
): Promise<SessionGroupSummary> {
  const response = await fetch('/api/session-group', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ name, members }),
  });
  const payload = await response.json().catch(() => ({}));
  if (!response.ok) {
    throw new Error(String(payload?.error || `HTTP ${response.status}`));
  }
  return normalizeGroupSummary(payload?.group);
}

export async function updateSessionGroup(
  groupId: string,
  name: string,
  members: string[],
): Promise<SessionGroupSummary> {
  const response = await fetch(`/api/session-group?group=${encodeURIComponent(groupId)}`, {
    method: 'PUT',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ name, members }),
  });
  const payload = await response.json().catch(() => ({}));
  if (!response.ok) {
    throw new Error(String(payload?.error || `HTTP ${response.status}`));
  }
  return normalizeGroupSummary(payload?.group);
}

export async function deleteSessionGroup(groupId: string): Promise<void> {
  const response = await fetch(`/api/session-group?group=${encodeURIComponent(groupId)}`, {
    method: 'DELETE',
  });
  const payload = await response.json().catch(() => ({}));
  if (!response.ok) {
    throw new Error(String(payload?.error || `HTTP ${response.status}`));
  }
}
