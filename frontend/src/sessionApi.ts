import type {
  GroupMemberDetail,
  GroupVote,
  SessionGroupDetail,
  SessionGroupSummary,
  SessionSummary,
} from './types.js';

export type SessionWorkspaceInput = { kind: 'managed' } | { kind: 'directory'; path: string };

export interface SessionMutationInput {
  name?: string;
  workspace?: SessionWorkspaceInput;
}

export interface WorkspaceBrowseEntry {
  name: string;
  path: string;
}

export interface WorkspaceBrowseResult {
  current: string;
  parent: string | null;
  home: string | null;
  roots: string[];
  directories: WorkspaceBrowseEntry[];
}

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
    workspace:
      raw.workspace && typeof raw.workspace === 'object' && !Array.isArray(raw.workspace)
        ? (() => {
            const workspace = raw.workspace as Record<string, unknown>;
            const kind = workspace.kind === 'directory' ? 'directory' : 'managed';
            return {
              kind,
              path: String(workspace.path ?? ''),
              display_name: String(workspace.display_name ?? ''),
              available: workspace.available !== false,
            };
          })()
        : undefined,
  };
}

export async function createSession(input?: SessionMutationInput): Promise<SessionSummary> {
  const response = await fetch('/api/session', {
    method: 'POST',
    ...(input
      ? {
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify(input),
        }
      : {}),
  });
  const payload = await response.json().catch(() => ({}));
  if (!response.ok) {
    throw new Error(String(payload?.error || `HTTP ${response.status}`));
  }
  return normalizeSessionSummary(payload?.session);
}

export async function updateSession(
  sessionId: string,
  input: SessionMutationInput,
): Promise<SessionSummary> {
  const response = await fetch(`/api/session?session=${encodeURIComponent(sessionId)}`, {
    method: 'PUT',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(input),
  });
  const payload = await response.json().catch(() => ({}));
  if (!response.ok) {
    const error = new Error(String(payload?.error || `HTTP ${response.status}`));
    Object.assign(error, { code: String(payload?.code || '') });
    throw error;
  }
  return normalizeSessionSummary(payload?.session);
}

export async function browseWorkspaces(path?: string): Promise<WorkspaceBrowseResult> {
  const query = path ? `?path=${encodeURIComponent(path)}` : '';
  const response = await fetch(`/api/workspaces/browse${query}`, { cache: 'no-store' });
  const payload = await response.json().catch(() => ({}));
  if (!response.ok) {
    throw new Error(String(payload?.error || `HTTP ${response.status}`));
  }
  return {
    current: String(payload?.current ?? ''),
    parent: payload?.parent == null ? null : String(payload.parent),
    home: payload?.home == null ? null : String(payload.home),
    roots: Array.isArray(payload?.roots) ? payload.roots.map(String) : [],
    directories: Array.isArray(payload?.directories)
      ? payload.directories
          .map((entry: unknown) => {
            const raw = (entry ?? {}) as Record<string, unknown>;
            const entryPath = String(raw.path ?? '');
            return entryPath ? { name: String(raw.name ?? entryPath), path: entryPath } : null;
          })
          .filter(
            (entry: WorkspaceBrowseEntry | null): entry is WorkspaceBrowseEntry => entry != null,
          )
      : [],
  };
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

function normalizeStringArray(value: unknown): string[] {
  return Array.isArray(value) ? value.map((item) => String(item).trim()).filter(Boolean) : [];
}

function normalizeGroupMemberDetails(value: unknown): GroupMemberDetail[] {
  if (!Array.isArray(value)) return [];
  return value
    .map((item) => {
      const raw = (item ?? {}) as Record<string, unknown>;
      const id = String(raw.id ?? '').trim();
      if (!id) return null;
      const role = String(raw.role ?? 'member');
      return {
        id,
        name: String(raw.name ?? id),
        role: role === 'owner' || role === 'admin' ? role : 'member',
      } satisfies GroupMemberDetail;
    })
    .filter((item): item is GroupMemberDetail => item != null);
}

export function normalizeGroupVotes(
  value: unknown,
  normalizeApprovals: (value: unknown) => string[] = normalizeStringArray,
): GroupVote[] {
  if (!Array.isArray(value)) return [];
  return value
    .map((item) => {
      const raw = (item ?? {}) as Record<string, unknown>;
      const id = String(raw.id ?? '').trim();
      const target = String(raw.target_session_id ?? '').trim();
      if (!id || !target) return null;
      const approvals = normalizeApprovals(raw.approvals);
      const rawThreshold =
        typeof raw.threshold === 'number' ? raw.threshold : Number(raw.threshold ?? 0);
      const threshold =
        Number.isFinite(rawThreshold) && rawThreshold >= 1
          ? Math.floor(rawThreshold)
          : Math.max(1, approvals.length);
      return {
        id,
        action: String(raw.action ?? ''),
        target_session_id: target,
        requester_session_id: String(raw.requester_session_id ?? '').trim(),
        approvals,
        threshold,
        created_at:
          typeof raw.created_at === 'number' ? raw.created_at : Number(raw.created_at ?? 0),
        updated_at:
          typeof raw.updated_at === 'number' ? raw.updated_at : Number(raw.updated_at ?? 0),
      } satisfies GroupVote;
    })
    .filter((item): item is GroupVote => item != null);
}

function normalizeGroupDetail(group: unknown): SessionGroupDetail {
  const raw = (group ?? {}) as Record<string, unknown>;
  const id = String(raw.id ?? '').trim();
  if (!id) {
    throw new Error('Group response did not include an id.');
  }
  const modelConfiguredMembers = Array.isArray(raw.model_configured_members)
    ? normalizeStringArray(raw.model_configured_members)
    : undefined;
  const configRevision =
    raw.configRevision === null || raw.configRevision === undefined
      ? Number.NaN
      : Number(raw.configRevision);
  const rawCapabilities =
    raw.capabilities && typeof raw.capabilities === 'object' && !Array.isArray(raw.capabilities)
      ? (raw.capabilities as Record<string, unknown>)
      : null;
  const s3Capability = typeof rawCapabilities?.s3 === 'boolean' ? rawCapabilities.s3 : undefined;
  const rawS3ConfigId = rawCapabilities?.s3_config_id;
  const s3ConfigId: string | null | undefined =
    typeof rawS3ConfigId === 'string' ? rawS3ConfigId : rawS3ConfigId === null ? null : undefined;
  const capabilities =
    s3Capability !== undefined || s3ConfigId !== undefined
      ? {
          ...(s3Capability !== undefined ? { s3: s3Capability } : {}),
          ...(s3ConfigId !== undefined ? { s3_config_id: s3ConfigId } : {}),
        }
      : undefined;
  return {
    id,
    name: String(raw.name ?? id),
    members: normalizeStringArray(raw.members),
    admins: normalizeStringArray(raw.admins),
    pending_votes: normalizeGroupVotes(raw.pending_votes),
    member_details: normalizeGroupMemberDetails(raw.member_details),
    model_override_members: normalizeStringArray(raw.model_override_members),
    ...(modelConfiguredMembers !== undefined
      ? { model_configured_members: modelConfiguredMembers }
      : {}),
    explicitPrimaryModelConfigured:
      typeof raw.explicitPrimaryModelConfigured === 'boolean'
        ? raw.explicitPrimaryModelConfigured
        : undefined,
    ...(Number.isSafeInteger(configRevision) && configRevision >= 0 ? { configRevision } : {}),
    ...(capabilities ? { capabilities } : {}),
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

export async function promoteSessionGroupAdmin(
  groupId: string,
  sessionId: string,
): Promise<SessionGroupDetail> {
  const response = await fetch(
    `/api/session-group/member?group=${encodeURIComponent(groupId)}&session=${encodeURIComponent(sessionId)}`,
    { method: 'PUT' },
  );
  const payload = await response.json().catch(() => ({}));
  if (!response.ok) {
    throw new Error(String(payload?.error || `HTTP ${response.status}`));
  }
  return normalizeGroupDetail(payload?.group);
}

export async function removeSessionGroupMember(
  groupId: string,
  sessionId: string,
): Promise<SessionGroupDetail | null> {
  const response = await fetch(
    `/api/session-group/member?group=${encodeURIComponent(groupId)}&session=${encodeURIComponent(sessionId)}`,
    { method: 'DELETE' },
  );
  const payload = await response.json().catch(() => ({}));
  if (!response.ok) {
    throw new Error(String(payload?.error || `HTTP ${response.status}`));
  }
  return payload?.group ? normalizeGroupDetail(payload.group) : null;
}
