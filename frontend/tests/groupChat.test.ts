import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import { renderMarkdown } from '../src/markdown.js';
import {
  refreshGroupMessages,
  renderGroupMessage,
  syncGroupEmptyState,
} from '../src/renderers/group-chat.js';
import { dom, state } from '../src/state.js';
import { setLanguage } from '../src/i18n.js';

describe('group chat rendering', () => {
  beforeEach(() => {
    document.body.innerHTML = '<main id="chat"></main>';
    dom.chat = document.getElementById('chat') as HTMLElement;
    state.activeGroupId = 'review-group';
    state.activeGroupMembers = ['worker-a'];
    state.activeGroupMemberDetails = [
      { id: 'main', name: 'Main', role: 'owner' },
      { id: 'worker-a', name: '<Ops> **Team**', role: 'admin' },
    ];
    state.sessions = [];
    state.sessionGroups = [{ id: 'review-group', name: 'Review Group' }];
    state.composerModelAvailability = 'ready';
    state.composerConfigRevision = 3;
    state.composerGroupModelRevision = 3;
    state.groupModelConfiguredMembers = new Set(['worker-a']);
    state.reactStatusRow = null;
    state.autoFollowChat = false;
    setLanguage('en');
  });

  afterEach(() => {
    setLanguage('en');
    state.activeGroupId = '';
    state.activeGroupMembers = [];
    state.activeGroupMemberDetails = [];
    state.markdownRenderQueue = [];
    dom.chat = null;
    document.body.innerHTML = '';
  });

  it('keeps the speaker outside the raw Markdown and highlights protocol mentions safely', async () => {
    const row = renderGroupMessage({
      role: 'session',
      session_id: 'worker-a',
      content:
        '# Review\n\n**Ready** @worker-a\n\n`@worker-a`\n\n<img src="x" onerror="alert(1)"><script>alert(2)</script>',
      timestamp: 1,
    })!;
    const body = row.querySelector<HTMLElement>('.group-message-body')!;
    await renderMarkdown(body);

    expect(row.querySelector('.group-message-speaker-name')?.textContent).toBe('<Ops> **Team**');
    expect(body._rawText).toContain('# Review\n\n**Ready** @worker-a');
    expect(body.querySelector('h1')?.textContent).toBe('Review');
    expect(body.querySelector('strong')?.textContent).toBe('Ready');
    expect(body.querySelectorAll('.group-mention')).toHaveLength(1);
    expect(body.querySelector('.group-mention')?.textContent).toBe('@<Ops> **Team**');
    expect(body.querySelector('code')?.textContent).toBe('@worker-a');
    expect(row.textContent).not.toContain('[<Ops> **Team**]');
    expect(body.querySelector('script')).toBeNull();
    expect(body.querySelector('img')?.getAttribute('onerror')).toBeNull();
  });

  it('refreshes localized speakers and @all without changing the protocol raw text', async () => {
    const row = renderGroupMessage({ role: 'user', content: 'Hello @all' })!;
    const body = row.querySelector<HTMLElement>('.group-message-body')!;
    await renderMarkdown(body);
    expect(row.querySelector('.group-message-speaker-name')?.textContent).toBe('You');
    expect(body.querySelector('.group-mention')?.textContent).toBe('@All');

    setLanguage('zh-CN');
    refreshGroupMessages();

    expect(row.querySelector('.group-message-speaker-name')?.textContent).toBe('你');
    expect(body.querySelector('.group-mention')?.textContent).toBe('@全部');
    expect(body._rawText).toBe('Hello @all');
  });

  it('renders a localized Group-specific empty state and removes it for the first message', () => {
    syncGroupEmptyState();

    const empty = document.querySelector<HTMLElement>('.group-empty-state')!;
    expect(empty.querySelector('h1')?.textContent).toBe('Review Group is ready');
    expect(empty.textContent).toContain('1 dispatch members');
    expect(empty.querySelector('[data-action="open-group-members"]')).not.toBeNull();

    setLanguage('zh-CN');
    refreshGroupMessages();
    expect(empty.querySelector('h1')?.textContent).toBe('Review Group 已就绪');

    renderGroupMessage({ role: 'user', content: '开始评审' });
    expect(document.querySelector('.group-empty-state')).toBeNull();
  });

  it('does not present missing models as known while configuration is unavailable', () => {
    state.composerModelAvailability = 'config-unavailable';
    state.groupModelConfiguredMembers = new Set();

    syncGroupEmptyState();

    expect(
      document.querySelector('.group-empty-state [data-action="open-group-agent-settings"]'),
    ).toBeNull();
  });
});
