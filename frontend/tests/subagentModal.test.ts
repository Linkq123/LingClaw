import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  closeSubagentModal,
  createSubagentPanel,
  finishSubagentPanel,
  openSubagentModal,
} from '../src/renderers/subagent.js';
import { dom, state } from '../src/state.js';
import { applyToolsVisibility } from '../src/viewState.js';

let originalScrollIntoView: typeof Element.prototype.scrollIntoView | undefined;

describe('subagent modal hosting', () => {
  beforeEach(() => {
    document.body.innerHTML = '<div id="chat"></div>';
    dom.chat = document.getElementById('chat') as HTMLElement;
    state.currentMsg = null;
    state.activeSubagentPanels.clear();
    state.activeToolPanel = null;
    state.autoFollowChat = true;
    state.showTools = true;
    originalScrollIntoView = Element.prototype.scrollIntoView;
    Object.defineProperty(Element.prototype, 'scrollIntoView', {
      configurable: true,
      value: vi.fn(),
    });
  });

  afterEach(() => {
    closeSubagentModal();
    state.activeSubagentPanels.clear();
    state.activeToolPanel = null;
    state.showTools = true;
    document.body.innerHTML = '';
    dom.chat = null;
    if (originalScrollIntoView) {
      Object.defineProperty(Element.prototype, 'scrollIntoView', {
        configurable: true,
        value: originalScrollIntoView,
      });
    } else {
      delete (Element.prototype as { scrollIntoView?: unknown }).scrollIntoView;
    }
    vi.restoreAllMocks();
  });

  it('moves the modal host to body while open and restores it on close', () => {
    createSubagentPanel('explore', '检查服务状态', 'task-1');

    const panel = dom.chat?.querySelector('.subagent-panel') as HTMLElement | null;
    expect(panel).not.toBeNull();
    const wrapper = panel?.closest('.timeline-node') as HTMLElement | null;
    const header = panel?.querySelector('.subagent-header') as HTMLElement | null;
    const scrollIntoViewSpy = Element.prototype.scrollIntoView as unknown as ReturnType<typeof vi.fn>;

    expect(wrapper?.parentElement).toBe(dom.chat);

    openSubagentModal(header);

    expect(wrapper?.classList.contains('subagent-modal-host')).toBe(true);
    expect(wrapper?.parentElement).toBe(document.body);
    expect(dom.chat?.querySelector('.subagent-modal-placeholder')).not.toBeNull();
    expect(panel?.classList.contains('subagent-modal-open')).toBe(true);
    expect(
      (panel?.querySelector('.subagent-body') as HTMLElement | null)?.hasAttribute('inert'),
    ).toBe(false);
    expect(document.getElementById('subagent-modal-backdrop')?.hidden).toBe(false);
    expect(panel?.querySelector('.subagent-modal-close')).toBe(document.activeElement);
    expect(scrollIntoViewSpy).not.toHaveBeenCalled();

    closeSubagentModal();

    expect(wrapper?.classList.contains('subagent-modal-host')).toBe(false);
    expect(wrapper?.parentElement).toBe(dom.chat);
    expect(dom.chat?.querySelector('.subagent-modal-placeholder')).toBeNull();
    expect(panel?.classList.contains('subagent-modal-open')).toBe(false);
    expect(
      (panel?.querySelector('.subagent-body') as HTMLElement | null)?.hasAttribute('inert'),
    ).toBe(true);
    expect(document.getElementById('subagent-modal-backdrop')?.hidden).toBe(true);
  });

  it('keeps summary copy enabled for finished panels without tools', () => {
    createSubagentPanel('explore', '检查服务状态', 'task-2');

    finishSubagentPanel(
      { task_id: 'task-2', agent: 'explore' },
      true,
      { tool_calls: 0, result_excerpt: '服务已启动' },
      { immediate: true },
    );

    const panel = dom.chat?.querySelector('.subagent-panel') as HTMLElement | null;
    const expandBtn = panel?.querySelector(
      '[data-action="subagent-toggle-all"]',
    ) as HTMLButtonElement | null;
    const focusBtn = panel?.querySelector(
      '[data-action="subagent-focus-current"]',
    ) as HTMLButtonElement | null;
    const copyBtn = panel?.querySelector(
      '[data-action="subagent-copy-summary"]',
    ) as HTMLButtonElement | null;

    expect(expandBtn?.disabled).toBe(true);
    expect(focusBtn?.disabled).toBe(true);
    expect(copyBtn?.disabled).toBe(false);
  });

  it('closes the modal when tools are hidden', () => {
    createSubagentPanel('explore', '检查服务状态', 'task-3');

    const panel = dom.chat?.querySelector('.subagent-panel') as HTMLElement | null;
    const wrapper = panel?.closest('.timeline-node') as HTMLElement | null;
    const header = panel?.querySelector('.subagent-header') as HTMLElement | null;
    const closeToolDrawer = vi.fn();

    openSubagentModal(header);
    expect(wrapper?.parentElement).toBe(document.body);

    applyToolsVisibility(false, {
      state,
      chat: dom.chat,
      closeToolDrawer,
      closeSubagentModal,
    });

    expect(closeToolDrawer).toHaveBeenCalledTimes(1);
    expect(panel?.classList.contains('subagent-modal-open')).toBe(false);
    expect(wrapper?.parentElement).toBe(dom.chat);
    expect(dom.chat?.classList.contains('hide-tools')).toBe(true);
    expect(document.getElementById('subagent-modal-backdrop')?.hidden).toBe(true);
  });
});
