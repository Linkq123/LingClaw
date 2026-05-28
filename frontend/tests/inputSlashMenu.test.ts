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
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true }));

    expect(input.value).toBe('/skills-system ');
    expect(menu.hidden).toBe(true);
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
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));

    expect(sendMock).toHaveBeenCalledWith('/help');
    expect(input.value).toBe('');
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
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));

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
        images: [{ url: 'https://example.com/demo.png' }],
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

    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowUp', bubbles: true, cancelable: true }));

    expect(input.value).toBe('/status');
    expect(menu.hidden).toBe(true);
  });
});
