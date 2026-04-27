import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  closeSubagentModal,
  createSubagentPanel,
  finishSubagentPanel,
  openSubagentModal,
  restoreSubagentHistorySnapshot,
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
    expect((panel?.querySelector('.subagent-body') as HTMLElement | null)?.style.height).toBe(
      'auto',
    );
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
      closeOrchestrateTaskModal: vi.fn(),
    });

    expect(closeToolDrawer).toHaveBeenCalledTimes(1);
    expect(panel?.classList.contains('subagent-modal-open')).toBe(false);
    expect(wrapper?.parentElement).toBe(dom.chat);
    expect(dom.chat?.classList.contains('hide-tools')).toBe(true);
    expect(document.getElementById('subagent-modal-backdrop')?.hidden).toBe(true);
  });
  it('strips delegated runtime context from the displayed prompt', () => {
    createSubagentPanel(
      'explore',
      '## Delegated Task Context\n- Current system local time: 2026-04-27 09:30:00 +08:00\n\n## Delegated Task\nInspect the logs and summarize the failure.',
      'task-4',
    );

    const promptEl = dom.chat?.querySelector('.subagent-prompt');
    expect(promptEl?.textContent).toBe('Inspect the logs and summarize the failure.');
    expect(promptEl?.textContent).not.toContain('Delegated Task Context');
  });

  it('restores reasoning, tools, and summary from a history snapshot', () => {
    createSubagentPanel('reviewer', 'Inspect the logs and summarize the failure.', 'task-5');

    restoreSubagentHistorySnapshot(
      { task_id: 'task-5', agent: 'reviewer' },
      {
        success: true,
        cycles: 2,
        tool_calls: 1,
        duration_ms: 480,
        input_tokens: 120,
        output_tokens: 64,
        reasoning: '[Cycle 1]\nCheck the log file and summarize the failure.',
        result_excerpt: 'Found the root cause in the startup logs.',
        tools: [
          {
            id: 'tool-1',
            name: 'read_file',
            arguments: '{"path":"logs/app.log"}',
            result: 'panic: startup config missing',
            duration_ms: 18,
            is_error: false,
          },
        ],
      },
    );

    const panel = dom.chat?.querySelector('.subagent-panel') as HTMLElement | null;
    const reasoningBody = panel?.querySelector('[data-subagent-reasoning-body]') as HTMLElement | null;
    const toolRows = panel?.querySelectorAll('.subagent-tool-row') || [];
    const summary = panel?.querySelector('.subagent-summary') as HTMLElement | null;

    expect(reasoningBody?.textContent).toContain('Check the log file');
    expect(toolRows).toHaveLength(1);
    expect((toolRows[0].querySelector('.subagent-tool-name') as HTMLElement | null)?.textContent).toBe(
      'read_file',
    );
    expect((toolRows[0].querySelector('.subagent-tool-output-code') as HTMLElement | null)?.textContent).toContain(
      'startup config missing',
    );
    expect(summary?.classList.contains('hidden')).toBe(false);
    expect(summary?.textContent).toContain('Found the root cause in the startup logs.');
  });
});
