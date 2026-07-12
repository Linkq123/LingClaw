import { beforeEach, describe, expect, it, vi } from 'vitest';

type StateModule = typeof import('../src/state.js');
type ToolsModule = typeof import('../src/renderers/tools.js');

describe('tool drawer focus', () => {
  let stateModule: StateModule;
  let toolsModule: ToolsModule;

  beforeEach(async () => {
    vi.resetModules();
    Object.defineProperty(window, 'innerWidth', { configurable: true, value: 1024 });
    document.body.innerHTML = `
      <aside id="session-drawer"><button>Session</button></aside>
      <main class="conversation-column">
        <div id="chat"></div>
        <div id="input-area"></div>
        <textarea id="input"></textarea>
      </main>
      <aside id="tool-drawer" aria-hidden="true">
        <button class="tool-drawer-close">Close</button>
        <h3 id="tool-drawer-title"></h3>
        <div id="tool-drawer-meta"></div>
        <pre id="tool-drawer-args"></pre>
        <section id="tool-drawer-result-section"><pre id="tool-drawer-result"></pre></section>
      </aside>
      <div id="tool-drawer-backdrop"></div>
    `;
    stateModule = await import('../src/state.js');
    stateModule.initDomRefs();
    toolsModule = await import('../src/renderers/tools.js');
  });

  it('creates a keyboard trigger and restores focus after closing', async () => {
    const panel = toolsModule.addToolCall('read_file', '{}', 'tool-1') as HTMLElement;
    const header = panel.querySelector<HTMLElement>('.tool-header');
    expect(header?.tagName).toBe('BUTTON');
    expect(header?.tabIndex).toBe(0);
    expect(header?.querySelector('.tool-icon use')?.getAttribute('href')).toBe('#icon-bolt');

    header?.focus();
    toolsModule.openToolDrawerFromHeader(header);
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    expect(document.activeElement).toBe(
      stateModule.dom.toolDrawer?.querySelector('.tool-drawer-close'),
    );
    expect(stateModule.dom.toolDrawer?.getAttribute('aria-modal')).toBe('true');
    expect(stateModule.dom.sessionDrawer?.inert).toBe(true);
    expect((document.querySelector('.conversation-column') as HTMLElement).inert).toBe(true);

    const tabEvent = new KeyboardEvent('keydown', { key: 'Tab', cancelable: true });
    expect(toolsModule.trapToolDrawerFocus(tabEvent)).toBe(true);
    expect(tabEvent.defaultPrevented).toBe(true);

    toolsModule.closeToolDrawer();
    expect(document.activeElement).toBe(header);
    expect(stateModule.dom.sessionDrawer?.inert).toBe(false);
    expect((document.querySelector('.conversation-column') as HTMLElement).inert).toBe(false);
  });

  it('updates modal state when an open drawer crosses the desktop breakpoint', () => {
    Object.defineProperty(window, 'innerWidth', { configurable: true, value: 1400 });
    const panel = toolsModule.addToolCall('read_file', '{}', 'tool-2') as HTMLElement;
    const header = panel.querySelector<HTMLElement>('.tool-header');
    toolsModule.openToolDrawerFromHeader(header);
    expect(stateModule.dom.toolDrawer?.hasAttribute('aria-modal')).toBe(false);

    Object.defineProperty(window, 'innerWidth', { configurable: true, value: 1024 });
    toolsModule.syncToolDrawerResponsiveState();
    expect(stateModule.dom.toolDrawer?.getAttribute('aria-modal')).toBe('true');
    expect(stateModule.dom.sessionDrawer?.inert).toBe(true);

    Object.defineProperty(window, 'innerWidth', { configurable: true, value: 1400 });
    toolsModule.syncToolDrawerResponsiveState();
    expect(stateModule.dom.toolDrawer?.hasAttribute('aria-modal')).toBe(false);
    expect(stateModule.dom.sessionDrawer?.inert).toBe(false);
    toolsModule.closeToolDrawer();
  });

  it('uses modal focus behavior on desktop when opened from a sub-agent dialog', () => {
    Object.defineProperty(window, 'innerWidth', { configurable: true, value: 1400 });
    document.body.classList.add('subagent-modal-visible');
    const panel = toolsModule.addToolCall('read_file', '{}', 'tool-subagent') as HTMLElement;

    toolsModule.openToolDrawerFromHeader(panel.querySelector('.tool-header'));

    expect(stateModule.dom.toolDrawer?.getAttribute('aria-modal')).toBe('true');
    const tabEvent = new KeyboardEvent('keydown', { key: 'Tab', cancelable: true });
    expect(toolsModule.trapToolDrawerFocus(tabEvent)).toBe(true);
    document.body.classList.remove('subagent-modal-visible');
    toolsModule.closeToolDrawer();
  });

  it('does not move focus into a drawer closed before its focus frame', async () => {
    const panel = toolsModule.addToolCall('read_file', '{}', 'tool-3') as HTMLElement;
    const header = panel.querySelector<HTMLElement>('.tool-header');
    header?.focus();

    toolsModule.openToolDrawerFromHeader(header);
    toolsModule.closeToolDrawer();
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));

    expect(document.activeElement).toBe(header);
    expect(stateModule.dom.toolDrawer?.getAttribute('aria-hidden')).toBe('true');
  });
});
