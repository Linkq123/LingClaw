import { tr } from '../i18n.js';
import { createIcon } from '../icons.js';
import { decorateGroupMentions } from '../groupMentions.js';
import { scheduleMarkdownRender } from '../markdown.js';
import { dom, state } from '../state.js';
import { addMsg } from './chat.js';

type GroupMessage = {
  role?: string;
  session_id?: string;
  content?: string;
  timestamp?: number;
  run_id?: string;
};

export function groupMemberName(sessionId: string): string {
  const id = String(sessionId || '').trim();
  if (!id) return 'session';
  const detail = state.activeGroupMemberDetails.find((member) => member.id === id);
  if (detail?.name) return detail.name;
  const summary = state.sessions.find((session) => session.id === id);
  return summary?.name || id;
}

function groupSpeaker(role: string, sessionId: string): { name: string; roleLabel: string } {
  if (role === 'session') {
    const detail = state.activeGroupMemberDetails.find((member) => member.id === sessionId);
    const roleLabel =
      detail?.role === 'admin'
        ? tr('common.admin')
        : detail?.role === 'owner'
          ? tr('common.main')
          : tr('common.member');
    return { name: groupMemberName(sessionId), roleLabel };
  }
  if (role === 'main') return { name: groupMemberName('main'), roleLabel: '' };
  if (role === 'user') return { name: tr('common.you'), roleLabel: '' };
  return { name: tr('common.system'), roleLabel: '' };
}

function avatarTone(value: string): string {
  let hash = 0;
  for (const char of value) hash = (hash * 31 + char.codePointAt(0)!) >>> 0;
  return String(hash % 4);
}

function syncGroupMessage(row: HTMLElement): void {
  const role = row.dataset.groupRole || 'system';
  const sessionId = row.dataset.groupSessionId || '';
  const speaker = groupSpeaker(role, sessionId);
  const name = row.querySelector<HTMLElement>('.group-message-speaker-name');
  const roleLabel = row.querySelector<HTMLElement>('.group-message-speaker-role');
  if (name) name.textContent = speaker.name;
  if (roleLabel) {
    roleLabel.textContent = speaker.roleLabel;
    roleLabel.hidden = !speaker.roleLabel;
  }

  const avatar = row.querySelector<HTMLElement>('.msg-avatar');
  if (avatar && role === 'session') {
    avatar.replaceChildren();
    avatar.classList.add('group-message-avatar');
    avatar.dataset.avatarTone = avatarTone(sessionId);
    avatar.textContent = Array.from(speaker.name.trim())[0] || '?';
    avatar.title = speaker.name;
  }

  const body = row.querySelector<HTMLElement>('.group-message-body');
  if (body) {
    body.querySelectorAll<HTMLElement>('.group-mention').forEach((mention) => {
      const id = mention.dataset.sessionId || '';
      mention.textContent = `@${id === 'all' ? tr('common.all') : groupMemberName(id)}`;
      mention.title = `@${id}`;
    });
    decorateGroupMentions(body, state.activeGroupMembers, groupMemberName, tr('common.all'));
  }
}

export function renderGroupMessage(message: GroupMessage): HTMLElement | null {
  if (!dom.chat) return null;
  dom.chat.querySelector('.group-empty-state')?.remove();
  const role = String(message?.role || 'system');
  const sessionId = String(message?.session_id || '');
  const rawContent = String(message?.content || '');
  const bubbleRole = role === 'user' ? 'user' : 'assistant';
  const body = addMsg(bubbleRole, rawContent, message?.timestamp, {
    trackUnread: role === 'session' || role === 'main',
  });
  const row = body.closest<HTMLElement>('.msg-row');
  const content = body.closest<HTMLElement>('.msg-content');
  if (!row || !content) return row;

  row.classList.add('group-message', `group-message--${role}`);
  row.dataset.groupRole = role;
  row.dataset.groupSessionId = sessionId;
  body.classList.add('group-message-body');
  body._rawText = rawContent;
  body._afterMarkdownRender = () => syncGroupMessage(row);

  const speakerLine = document.createElement('div');
  speakerLine.className = 'group-message-speaker';
  const speakerName = document.createElement('span');
  speakerName.className = 'group-message-speaker-name';
  const speakerRole = document.createElement('span');
  speakerRole.className = 'group-message-speaker-role';
  speakerLine.append(speakerName, speakerRole);
  content.insertBefore(speakerLine, body);

  syncGroupMessage(row);
  scheduleMarkdownRender(body);
  return row;
}

function activeGroupName(): string {
  const summary = state.sessionGroups.find((group) => group.id === state.activeGroupId);
  return summary?.name || dom.sessionNameEl?.textContent?.trim() || tr('group.nameFallback');
}

export function syncGroupEmptyState(): void {
  if (!dom.chat) return;
  const existing = dom.chat.querySelector<HTMLElement>('.group-empty-state');
  if (!state.activeGroupId || dom.chat.querySelector('.msg-row.group-message')) {
    existing?.remove();
    return;
  }

  const section = existing || document.createElement('section');
  section.className = 'group-empty-state';
  section.id = 'group-empty-state';
  section.replaceChildren();

  const mark = document.createElement('div');
  mark.className = 'group-empty-mark';
  mark.appendChild(createIcon('users'));
  const title = document.createElement('h1');
  title.textContent = tr('group.emptyTitle', { name: activeGroupName() });
  const description = document.createElement('p');
  description.textContent = tr('group.emptyDescription', {
    count: state.activeGroupMembers.length,
  });
  const owner = document.createElement('div');
  owner.className = 'group-empty-owner';
  owner.append(createIcon('user-node'), document.createTextNode(tr('group.emptyOwner')));
  const actions = document.createElement('div');
  actions.className = 'group-empty-actions';
  const members = document.createElement('button');
  members.type = 'button';
  members.dataset.action = 'open-group-members';
  members.append(createIcon('users'), document.createTextNode(tr('group.manageMembers')));
  actions.appendChild(members);

  const modelStateKnown =
    state.composerModelAvailability !== 'checking' &&
    state.composerModelAvailability !== 'config-unavailable' &&
    state.composerGroupModelRevision === state.composerConfigRevision;
  const hasMissingModel =
    modelStateKnown &&
    state.activeGroupMembers.some((member) => !state.groupModelConfiguredMembers.has(member));
  if (hasMissingModel) {
    const configure = document.createElement('button');
    configure.type = 'button';
    configure.className = 'secondary';
    configure.dataset.action = 'open-group-agent-settings';
    configure.append(
      createIcon('settings'),
      document.createTextNode(tr('composer.configureAgent')),
    );
    actions.appendChild(configure);
  }

  section.append(mark, title, description, owner, actions);
  if (!existing) dom.chat.appendChild(section);
}

export function refreshGroupMessages(): void {
  dom.chat?.querySelectorAll<HTMLElement>('.msg-row.group-message').forEach(syncGroupMessage);
  if (dom.chat?.querySelector('.group-empty-state')) syncGroupEmptyState();
}
