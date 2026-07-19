import { beforeEach, describe, expect, it, vi } from 'vitest';

describe('group mention menu', () => {
  beforeEach(() => {
    vi.resetModules();
    document.body.innerHTML = `
      <main id="chat"></main>
      <div id="input-area">
        <div id="slash-command-menu" class="slash-command-menu" hidden></div>
        <textarea id="input"></textarea>
        <button id="send"></button>
        <button id="stop"></button>
        <span id="send-icon"></span>
      </div>
    `;
  });

  it('filters by display name and inserts the exact session id with keyboard or mouse', async () => {
    const stateModule = await import('../src/state.js');
    stateModule.initDomRefs();
    stateModule.state.activeGroupId = 'review-group';
    stateModule.state.activeGroupMembers = ['worker-a', 'worker-b'];
    stateModule.state.groupTargetMode = 'all';
    stateModule.state.groupTargetPickerOpen = true;
    stateModule.state.groupTargetSearchQuery = '前端';
    stateModule.state.activeGroupMemberDetails = [
      { id: 'worker-a', name: '前端助手', role: 'member' },
      { id: 'worker-b', name: '后端助手', role: 'admin' },
    ];
    stateModule.state.pendingImages = [];
    const { initInputListeners } = await import('../src/input.js');
    initInputListeners();
    const input = stateModule.dom.input!;
    const menu = stateModule.dom.slashCommandMenu!;

    input.focus();
    input.value = '请问，@前端';
    input.setSelectionRange(input.value.length, input.value.length);
    input.dispatchEvent(new Event('input', { bubbles: true }));

    expect(menu.hidden).toBe(false);
    expect(menu.getAttribute('role')).toBe('listbox');
    expect(menu.getAttribute('aria-label')).toBe('Mention a group member');
    expect(input.getAttribute('aria-expanded')).toBe('true');
    expect(input.getAttribute('aria-activedescendant')).toBe('group-mention-option-0');
    expect(menu.querySelectorAll('.mention-menu-item')).toHaveLength(1);
    expect(menu.querySelector('.mention-menu-name')?.textContent).toBe('前端助手');

    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    expect(input.value).toBe('请问，@worker-a ');
    expect(menu.hidden).toBe(true);
    expect(stateModule.state.groupTargetMode).toBe('mentions');
    expect(stateModule.state.groupTargetPickerOpen).toBe(false);
    expect(stateModule.state.groupTargetSearchQuery).toBe('');
    expect(document.activeElement).toBe(input);

    stateModule.state.groupTargetMode = 'all';
    input.value = 'Ask @';
    input.setSelectionRange(input.value.length, input.value.length);
    input.dispatchEvent(new Event('input', { bubbles: true }));
    const workerB = menu.querySelector<HTMLButtonElement>('[data-mention-id="worker-b"]');
    expect(workerB).not.toBeNull();
    workerB?.click();
    expect(input.value).toBe('Ask @worker-b ');
    expect(stateModule.state.groupTargetMode).toBe('mentions');
  });

  it('dispatches an inserted mention only to mentioned group members', async () => {
    const stateModule = await import('../src/state.js');
    stateModule.initDomRefs();
    const sendMock = vi.fn();
    stateModule.state.ws = { readyState: 1, send: sendMock } as unknown as WebSocket;
    stateModule.state.activeGroupId = 'review-group';
    stateModule.state.activeGroupMembers = ['worker-a', 'worker-b'];
    stateModule.state.activeGroupMemberDetails = [
      { id: 'worker-a', name: 'Worker A', role: 'member' },
      { id: 'worker-b', name: 'Worker B', role: 'member' },
    ];
    stateModule.state.groupModelConfiguredMembers = new Set(['worker-a', 'worker-b']);
    stateModule.state.groupTargetMode = 'all';
    stateModule.state.pendingImages = [];
    stateModule.state.planModeEnabled = false;

    const { initInputListeners, send } = await import('../src/input.js');
    initInputListeners();
    const input = stateModule.dom.input!;
    input.value = '@Work';
    input.setSelectionRange(input.value.length, input.value.length);
    input.dispatchEvent(new Event('input', { bubbles: true }));
    stateModule.dom.slashCommandMenu
      ?.querySelector<HTMLButtonElement>('[data-mention-id="worker-a"]')
      ?.click();

    send();

    expect(JSON.parse(sendMock.mock.calls[0][0])).toMatchObject({
      type: 'group_message',
      text: '@worker-a',
      targets: [],
      target_mode: 'mentions',
    });
  });

  it('closes with Escape and never opens outside group chat', async () => {
    const stateModule = await import('../src/state.js');
    stateModule.initDomRefs();
    stateModule.state.activeGroupId = 'review-group';
    stateModule.state.activeGroupMembers = ['worker-a'];
    stateModule.state.activeGroupMemberDetails = [
      { id: 'worker-a', name: 'Worker A', role: 'member' },
    ];
    const { initInputListeners } = await import('../src/input.js');
    initInputListeners();
    const input = stateModule.dom.input!;
    const menu = stateModule.dom.slashCommandMenu!;

    input.focus();
    input.value = '@';
    input.setSelectionRange(1, 1);
    input.dispatchEvent(new Event('input', { bubbles: true }));
    expect(menu.hidden).toBe(false);
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    expect(menu.hidden).toBe(true);

    input.value = '@';
    input.setSelectionRange(1, 1);
    input.dispatchEvent(new Event('input', { bubbles: true }));
    input.setSelectionRange(0, 0);
    input.dispatchEvent(new KeyboardEvent('keyup', { key: 'ArrowLeft', bubbles: true }));
    expect(menu.hidden).toBe(true);

    stateModule.state.activeGroupId = '';
    input.value = '@';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    expect(menu.hidden).toBe(true);

    input.value = '/';
    input.setSelectionRange(1, 1);
    input.dispatchEvent(new Event('input', { bubbles: true }));
    expect(menu.getAttribute('role')).toBe('listbox');
    expect(menu.getAttribute('aria-label')).toBe('Command suggestions');
    expect(menu.querySelector('.slash-command-item')?.getAttribute('role')).toBe('option');
    expect(input.getAttribute('aria-activedescendant')).toMatch(/^slash-command-option-/);
  });

  it('does not send while Enter is confirming an IME composition', async () => {
    const stateModule = await import('../src/state.js');
    stateModule.initDomRefs();
    const sendMock = vi.fn();
    stateModule.state.ws = { readyState: 1, send: sendMock } as unknown as WebSocket;
    stateModule.state.activeGroupId = '';
    stateModule.state.composerModelAvailability = 'ready';
    stateModule.state.pendingImages = [];
    stateModule.state.planModeEnabled = false;
    const { initInputListeners } = await import('../src/input.js');
    initInputListeners();
    const input = stateModule.dom.input!;
    input.value = '中文';
    const event = new KeyboardEvent('keydown', { key: 'Enter', bubbles: true });
    Object.defineProperty(event, 'isComposing', { value: true });

    input.dispatchEvent(event);

    expect(sendMock).not.toHaveBeenCalled();
    expect(input.value).toBe('中文');
  });
});
