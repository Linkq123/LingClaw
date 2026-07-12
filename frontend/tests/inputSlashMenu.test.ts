import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../src/renderers/chat.js', () => ({
  addMsg: vi.fn(() => document.createElement('div')),
  addSystem: vi.fn(),
  renderUserImageThumbnails: vi.fn(),
  setBusy: vi.fn(),
}));

vi.mock('../src/images.js', () => ({
  renderImagePreviews: vi.fn(),
  uploadLocalImages: vi.fn(),
}));

vi.mock('../src/scroll.js', () => ({
  scrollDown: vi.fn(),
  syncToolDrawerBounds: vi.fn(),
}));

describe('input slash command menu', () => {
  beforeEach(async () => {
    vi.resetModules();
    document.body.innerHTML = `
      <div id="chat"></div>
      <div id="input-area">
        <div id="slash-command-menu" hidden></div>
        <textarea id="input"></textarea>
        <button id="send"></button>
        <button id="stop"></button>
      </div>
    `;
    const { state } = await import('../src/state.js');
    state.composerModelAvailability = 'ready';
    state.composerSessionIdentityPending = false;
    state.sessionSwitchInFlight = false;
    state.sessionIdentityMutationInFlight = false;
    state.imageUploadInFlight = false;
  });

  it('renders suggestions for slash-prefixed input and inserts the highlighted command with Tab', async () => {
    const stateModule = await import('../src/state.js');
    stateModule.initDomRefs();
    stateModule.state.ws = { readyState: 0 } as WebSocket;

    const { initInputListeners } = await import('../src/input.js');
    initInputListeners();

    const input = stateModule.dom.input!;
    const menu = stateModule.dom.slashCommandMenu!;

    input.value = '/sk';
    input.dispatchEvent(new Event('input', { bubbles: true }));

    expect(menu.hidden).toBe(false);
    expect(menu.textContent).toContain('/skills');
    expect(menu.textContent).toContain('/skills-system');

    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }));
    input.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true }),
    );

    expect(input.value).toBe('/skills-system ');
    expect(menu.hidden).toBe(true);
  });

  it('applies a slash command suggestion when clicked with the mouse', async () => {
    const stateModule = await import('../src/state.js');
    stateModule.initDomRefs();
    stateModule.state.ws = { readyState: 0 } as WebSocket;

    const { initInputListeners } = await import('../src/input.js');
    initInputListeners();

    const input = stateModule.dom.input!;
    const menu = stateModule.dom.slashCommandMenu!;

    input.value = '/sk';
    input.dispatchEvent(new Event('input', { bubbles: true }));

    const target = Array.from(menu.querySelectorAll<HTMLButtonElement>('.slash-command-item')).find(
      (item) => item.textContent?.includes('/skills-system'),
    );

    expect(target).toBeDefined();

    target!.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, cancelable: true }));
    target!.click();

    expect(input.value).toBe('/skills-system ');
    expect(menu.hidden).toBe(true);
  });

  it('scrolls the active slash command into view when arrow keys change the selection', async () => {
    const scrollIntoViewMock = vi.fn();
    Object.defineProperty(HTMLElement.prototype, 'scrollIntoView', {
      configurable: true,
      value: scrollIntoViewMock,
    });

    const stateModule = await import('../src/state.js');
    stateModule.initDomRefs();
    stateModule.state.ws = { readyState: 0 } as WebSocket;

    const { initInputListeners } = await import('../src/input.js');
    initInputListeners();

    const input = stateModule.dom.input!;
    input.value = '/sk';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    scrollIntoViewMock.mockClear();

    input.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true, cancelable: true }),
    );

    expect(scrollIntoViewMock).toHaveBeenCalledWith({ block: 'nearest' });
  });

  it('sends exact slash commands with Enter instead of treating them as autocomplete picks', async () => {
    const stateModule = await import('../src/state.js');
    stateModule.initDomRefs();
    const sendMock = vi.fn();
    stateModule.state.ws = {
      readyState: 1,
      send: sendMock,
    } as unknown as WebSocket;
    stateModule.state.pendingImages = [];
    stateModule.state.busy = false;

    const { initInputListeners } = await import('../src/input.js');
    initInputListeners();

    const input = stateModule.dom.input!;
    input.value = '/help';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    input.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }),
    );

    expect(sendMock).toHaveBeenCalledWith('/help');
    expect(input.value).toBe('');
  });

  it('blocks normal messages while a slash Session switch is awaiting confirmation', async () => {
    const stateModule = await import('../src/state.js');
    stateModule.initDomRefs();
    const sendMock = vi.fn();
    stateModule.state.ws = {
      readyState: 1,
      send: sendMock,
    } as unknown as WebSocket;
    stateModule.state.pendingImages = [];
    stateModule.state.busy = false;

    const { send } = await import('../src/input.js');
    stateModule.dom.input!.value = '/switch unconfigured-session';
    send();

    expect(sendMock).toHaveBeenCalledWith('/switch unconfigured-session');
    expect(stateModule.state.composerSessionTransitionPending).toBe(true);
    expect(stateModule.state.composerModelAvailability).toBe('checking');

    stateModule.dom.input!.value = 'must wait for the Session payload';
    send();
    expect(sendMock).toHaveBeenCalledTimes(1);

    stateModule.dom.input!.value = '/switch second-session';
    send();
    expect(sendMock).toHaveBeenCalledTimes(1);
    expect(stateModule.state.composerSessionTransitionTarget).toBe('unconfigured-session');
  });

  it('does not lock the composer for a switch command without a target', async () => {
    const stateModule = await import('../src/state.js');
    stateModule.initDomRefs();
    const sendMock = vi.fn();
    stateModule.state.ws = {
      readyState: 1,
      send: sendMock,
    } as unknown as WebSocket;
    stateModule.state.pendingImages = [];
    stateModule.state.busy = false;

    const { send } = await import('../src/input.js');
    stateModule.dom.input!.value = '/switch';
    send();

    expect(sendMock).toHaveBeenCalledWith('/switch');
    expect(stateModule.state.composerSessionTransitionPending).toBe(false);
    expect(stateModule.state.composerModelAvailability).toBe('ready');
  });

  it('blocks a targeted switch until the initial Session identity is confirmed', async () => {
    const stateModule = await import('../src/state.js');
    stateModule.initDomRefs();
    const sendMock = vi.fn();
    stateModule.state.ws = {
      readyState: 1,
      send: sendMock,
    } as unknown as WebSocket;
    stateModule.state.pendingImages = [];
    stateModule.state.busy = false;
    stateModule.state.composerSessionIdentityPending = true;
    stateModule.state.composerModelAvailability = 'checking';

    const { send } = await import('../src/input.js');
    stateModule.dom.input!.value = '/switch target-session';
    send();

    expect(sendMock).not.toHaveBeenCalled();
    expect(stateModule.state.composerSessionTransitionPending).toBe(false);

    stateModule.dom.input!.value = '/switch';
    send();
    expect(sendMock).toHaveBeenCalledWith('/switch');
  });

  it('blocks a targeted switch while an image upload is still in flight', async () => {
    const stateModule = await import('../src/state.js');
    stateModule.initDomRefs();
    const sendMock = vi.fn();
    stateModule.state.ws = {
      readyState: 1,
      send: sendMock,
    } as unknown as WebSocket;
    stateModule.state.pendingImages = [];
    stateModule.state.busy = false;
    stateModule.state.imageUploadInFlight = true;

    const { send } = await import('../src/input.js');
    stateModule.dom.input!.value = '/switch target-session';
    send();

    expect(sendMock).not.toHaveBeenCalled();
    expect(stateModule.state.composerSessionTransitionPending).toBe(false);
  });

  it('does not send an ordinary message before its image upload finishes', async () => {
    const stateModule = await import('../src/state.js');
    stateModule.initDomRefs();
    const sendMock = vi.fn();
    stateModule.state.ws = {
      readyState: 1,
      send: sendMock,
    } as unknown as WebSocket;
    stateModule.state.pendingImages = [];
    stateModule.state.busy = false;
    stateModule.state.imageUploadInFlight = true;

    const { send } = await import('../src/input.js');
    stateModule.dom.input!.value = 'describe the uploaded image';
    send();

    expect(sendMock).not.toHaveBeenCalled();
    expect(stateModule.dom.input!.value).toBe('describe the uploaded image');
  });

  it('normalizes mixed-case slash commands on Enter instead of sending them unchanged', async () => {
    const stateModule = await import('../src/state.js');
    stateModule.initDomRefs();
    const sendMock = vi.fn();
    stateModule.state.ws = {
      readyState: 1,
      send: sendMock,
    } as unknown as WebSocket;
    stateModule.state.pendingImages = [];
    stateModule.state.busy = false;

    const { initInputListeners } = await import('../src/input.js');
    initInputListeners();

    const input = stateModule.dom.input!;
    input.value = '/HELP';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    input.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }),
    );

    expect(sendMock).not.toHaveBeenCalled();
    expect(input.value).toBe('/help');
  });

  it('canonicalizes mixed-case slash commands with arguments before send dispatch', async () => {
    const stateModule = await import('../src/state.js');
    stateModule.initDomRefs();
    const sendMock = vi.fn();
    stateModule.state.ws = {
      readyState: 1,
      send: sendMock,
    } as unknown as WebSocket;
    stateModule.state.pendingImages = [];
    stateModule.state.busy = false;

    const { initInputListeners } = await import('../src/input.js');
    initInputListeners();

    const input = stateModule.dom.input!;
    const sendBtn = stateModule.dom.sendBtn!;
    input.value = '/Tool on';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    sendBtn.click();

    expect(sendMock).toHaveBeenCalledWith('/tool on');
    expect(input.value).toBe('');
  });

  it('applies the highlighted slash command on Send before dispatching incomplete prefixes', async () => {
    const stateModule = await import('../src/state.js');
    stateModule.initDomRefs();
    const sendMock = vi.fn();
    stateModule.state.ws = {
      readyState: 1,
      send: sendMock,
    } as unknown as WebSocket;
    stateModule.state.pendingImages = [];
    stateModule.state.busy = false;

    const { initInputListeners } = await import('../src/input.js');
    initInputListeners();

    const input = stateModule.dom.input!;
    const sendBtn = stateModule.dom.sendBtn!;
    const menu = stateModule.dom.slashCommandMenu!;

    input.value = '/he';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    sendBtn.click();

    expect(sendMock).not.toHaveBeenCalled();
    expect(input.value).toBe('/help');
    expect(menu.hidden).toBe(true);
  });

  it('keeps slash-prefixed image captions unchanged when the message is not a command', async () => {
    const stateModule = await import('../src/state.js');
    stateModule.initDomRefs();
    const sendMock = vi.fn();
    stateModule.state.ws = {
      readyState: 1,
      send: sendMock,
    } as unknown as WebSocket;
    stateModule.state.pendingImages = [{ url: 'https://example.com/demo.png' }];
    stateModule.state.busy = false;

    const { initInputListeners } = await import('../src/input.js');
    initInputListeners();

    const input = stateModule.dom.input!;
    const sendBtn = stateModule.dom.sendBtn!;
    input.value = '/API screenshot';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    sendBtn.click();

    expect(sendMock).toHaveBeenCalledWith(
      JSON.stringify({
        text: '/API screenshot',
        plan_mode: false,
        images: [{ url: 'https://example.com/demo.png' }],
      }),
    );
    expect(input.value).toBe('');
  });

  it('sends normal messages with plan mode disabled by default', async () => {
    const stateModule = await import('../src/state.js');
    stateModule.initDomRefs();
    const sendMock = vi.fn();
    stateModule.state.ws = {
      readyState: 1,
      send: sendMock,
    } as unknown as WebSocket;
    stateModule.state.pendingImages = [];
    stateModule.state.busy = false;
    stateModule.state.planModeEnabled = false;

    const { initInputListeners } = await import('../src/input.js');
    initInputListeners();

    const input = stateModule.dom.input!;
    const sendBtn = stateModule.dom.sendBtn!;
    input.value = 'hello';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    sendBtn.click();

    expect(sendMock).toHaveBeenCalledWith(
      JSON.stringify({
        text: 'hello',
        plan_mode: false,
      }),
    );
    expect(input.value).toBe('');
  });

  it('sends normal messages with plan mode enabled when toggled on', async () => {
    const stateModule = await import('../src/state.js');
    stateModule.initDomRefs();
    const sendMock = vi.fn();
    stateModule.state.ws = {
      readyState: 1,
      send: sendMock,
    } as unknown as WebSocket;
    stateModule.state.pendingImages = [];
    stateModule.state.busy = false;
    stateModule.state.planModeEnabled = true;

    const { initInputListeners } = await import('../src/input.js');
    initInputListeners();

    const input = stateModule.dom.input!;
    const sendBtn = stateModule.dom.sendBtn!;
    input.value = 'hello with plan';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    sendBtn.click();

    expect(sendMock).toHaveBeenCalledWith(
      JSON.stringify({
        text: 'hello with plan',
        plan_mode: true,
      }),
    );
    expect(input.value).toBe('');
  });

  it('lets arrow keys fall back to input history when the slash query has no matches', async () => {
    const stateModule = await import('../src/state.js');
    stateModule.initDomRefs();
    stateModule.state.ws = { readyState: 0 } as WebSocket;
    stateModule.state.inputHistory = ['/status'];
    stateModule.state.inputHistoryIndex = -1;
    stateModule.state.inputHistoryDraft = '';

    const { initInputListeners } = await import('../src/input.js');
    initInputListeners();

    const input = stateModule.dom.input!;
    const menu = stateModule.dom.slashCommandMenu!;
    input.value = '/zzz';
    input.setSelectionRange(input.value.length, input.value.length);
    input.dispatchEvent(new Event('input', { bubbles: true }));

    expect(menu.hidden).toBe(false);
    expect(menu.textContent).toContain('No matching commands');

    input.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'ArrowUp', bubbles: true, cancelable: true }),
    );

    expect(input.value).toBe('/status');
    expect(menu.hidden).toBe(true);
  });

  it('does not dispatch composer input while the Agent model is unconfigured', async () => {
    const stateModule = await import('../src/state.js');
    stateModule.initDomRefs();
    const sendMock = vi.fn();
    stateModule.state.ws = { readyState: 1, send: sendMock } as unknown as WebSocket;
    stateModule.state.composerModelAvailability = 'agent-model-unconfigured';
    stateModule.dom.input!.value = 'should not be sent';

    const { send } = await import('../src/input.js');
    send();

    expect(sendMock).not.toHaveBeenCalled();
    expect(stateModule.dom.input?.value).toBe('should not be sent');
  });

  it('still dispatches slash commands while the Agent model is unconfigured', async () => {
    const stateModule = await import('../src/state.js');
    stateModule.initDomRefs();
    const sendMock = vi.fn();
    stateModule.state.ws = { readyState: 1, send: sendMock } as unknown as WebSocket;
    stateModule.state.composerModelAvailability = 'agent-model-unconfigured';
    stateModule.state.pendingImages = [];
    stateModule.dom.input!.value = '/help';

    const { send } = await import('../src/input.js');
    send();

    expect(sendMock).toHaveBeenCalledWith('/help');
    expect(stateModule.dom.input?.value).toBe('');
  });

  it('does not dispatch /new while the Agent model is unconfigured', async () => {
    const stateModule = await import('../src/state.js');
    stateModule.initDomRefs();
    const sendMock = vi.fn();
    stateModule.state.ws = { readyState: 1, send: sendMock } as unknown as WebSocket;
    stateModule.state.composerModelAvailability = 'agent-model-unconfigured';
    stateModule.state.pendingImages = [];
    stateModule.dom.input!.value = '/new';

    const { send } = await import('../src/input.js');
    send();

    expect(sendMock).not.toHaveBeenCalled();
    expect(stateModule.dom.input?.value).toBe('/new');
  });

  it('allows clicking Send for model-independent slash commands', async () => {
    const stateModule = await import('../src/state.js');
    stateModule.initDomRefs();
    const sendMock = vi.fn();
    stateModule.state.ws = { readyState: 1, send: sendMock } as unknown as WebSocket;
    stateModule.state.composerModelAvailability = 'agent-model-unconfigured';

    const { initInputListeners } = await import('../src/input.js');
    initInputListeners();
    stateModule.dom.input!.value = '/status';
    stateModule.dom.input!.dispatchEvent(new Event('input', { bubbles: true }));

    expect(stateModule.dom.sendBtn?.disabled).toBe(false);
    stateModule.dom.sendBtn?.click();
    expect(sendMock).toHaveBeenCalledWith('/status');
  });
});
