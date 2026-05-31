import type { SessionSummary } from './types.js';

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
