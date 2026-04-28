import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  addSubagentTool,
  closeSubagentModal,
  createSubagentPanel,
  finishSubagentPanel,
  openSubagentModal,
  restoreSubagentHistorySnapshot,
  updateSubagentToolResult,
} from '../src/renderers/subagent.js';
import { dom, state } from '../src/state.js';
import { applyToolsVisibility } from '../src/viewState.js';

let originalScrollIntoView: typeof Element.prototype.scrollIntoView | undefined;

describe('subagent modal hosting', () => {
  beforeEach(() => {
    document.body.innerHTML = '<div id="chat"></div>';
    dom.chat = document.getElementById('chat') as HTMLElement;
    dom.toolDrawer = null;
    dom.toolDrawerBackdrop = null;
    dom.toolDrawerTitle = null;
    dom.toolDrawerMeta = null;
    dom.toolDrawerArgs = null;
    dom.toolDrawerResult = null;
    dom.toolDrawerResultSection = null;
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
    dom.toolDrawer = null;
    dom.toolDrawerBackdrop = null;
    dom.toolDrawerTitle = null;
    dom.toolDrawerMeta = null;
    dom.toolDrawerArgs = null;
    dom.toolDrawerResult = null;
    dom.toolDrawerResultSection = null;
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
    createSubagentPanel('explore', 'Inspect the current service status.', 'task-1');

    const panel = dom.chat?.querySelector('.subagent-panel') as HTMLElement | null;
    expect(panel).not.toBeNull();
    const wrapper = panel?.closest('.timeline-node') as HTMLElement | null;
    const header = panel?.querySelector('.subagent-header') as HTMLElement | null;
    const scrollIntoViewSpy = Element.prototype.scrollIntoView as unknown as ReturnType<typeof vi.fn>;

    expect(wrapper?.parentElement).toBe(dom.chat);

    openSubagentModal(header);

    const placeholder = dom.chat?.querySelector('.subagent-modal-placeholder') as HTMLElement | null;

    expect(wrapper?.classList.contains('subagent-modal-host')).toBe(true);
    expect(wrapper?.parentElement).toBe(document.body);
    expect(placeholder).not.toBeNull();
    expect(placeholder?.style.minHeight).not.toBe('');
    expect(placeholder?.querySelector('.subagent-panel')).not.toBeNull();
    expect(placeholder?.textContent).toContain('explore');
    expect(placeholder?.querySelector('.subagent-status')).toBeNull();
    expect(placeholder?.querySelector('.subagent-body')).toBeNull();
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
    createSubagentPanel('explore', 'Inspect the current service status.', 'task-2');

    finishSubagentPanel(
      { task_id: 'task-2', agent: 'explore' },
      true,
      { tool_calls: 0, result_excerpt: 'Service is already online.' },
      { immediate: true },
    );

    const panel = dom.chat?.querySelector('.subagent-panel') as HTMLElement | null;
    const copyBtn = panel?.querySelector(
      '[data-action="subagent-copy-summary"]',
    ) as HTMLButtonElement | null;

    expect(panel?.querySelector('[data-action="subagent-toggle-all"]')).toBeNull();
    expect(panel?.querySelector('[data-action="subagent-focus-current"]')).toBeNull();
    expect(copyBtn?.disabled).toBe(false);
  });

  it('closes the modal when tools are hidden', () => {
    createSubagentPanel('explore', 'Inspect the current service status.', 'task-3');

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

  it('closes any stale tool drawer before opening the modal', () => {
    createSubagentPanel('explore', 'Inspect the current service status.', 'task-3b');

    const drawer = document.createElement('div');
    drawer.className = 'tool-drawer open';
    drawer.setAttribute('aria-hidden', 'false');
    const drawerBackdrop = document.createElement('div');
    drawerBackdrop.className = 'tool-drawer-backdrop open';
    const staleToolPanel = document.createElement('button');
    staleToolPanel.className = 'tool-panel-active';
    document.body.append(drawer, drawerBackdrop, staleToolPanel);
    dom.toolDrawer = drawer;
    dom.toolDrawerBackdrop = drawerBackdrop;
    state.activeToolPanel = staleToolPanel;

    const panel = dom.chat?.querySelector('.subagent-panel') as HTMLElement | null;
    const header = panel?.querySelector('.subagent-header') as HTMLElement | null;

    openSubagentModal(header);

    expect(drawer.classList.contains('open')).toBe(false);
    expect(drawerBackdrop.classList.contains('open')).toBe(false);
    expect(drawer.getAttribute('aria-hidden')).toBe('true');
    expect(staleToolPanel.classList.contains('tool-panel-active')).toBe(false);
    expect(state.activeToolPanel).toBeNull();
  });

  it('keeps the visible placeholder in sync when the task finishes while open', () => {
    createSubagentPanel('explore', 'Inspect the current service status.', 'task-3c');

    const panel = dom.chat?.querySelector('.subagent-panel') as HTMLElement | null;
    const header = panel?.querySelector('.subagent-header') as HTMLElement | null;

    openSubagentModal(header);
    const initialPlaceholder = dom.chat?.querySelector('.subagent-modal-placeholder') as HTMLElement | null;
    const initialPlaceholderHeight = initialPlaceholder?.style.minHeight;
    finishSubagentPanel(
      { task_id: 'task-3c', agent: 'explore' },
      true,
      { cycles: 2, tool_calls: 1, result_excerpt: 'Service recovered after a restart.' },
      { immediate: false },
    );

    const placeholderPanel = dom.chat?.querySelector(
      '.subagent-modal-placeholder .subagent-panel',
    ) as HTMLElement | null;

    expect(placeholderPanel?.classList.contains('subagent-active')).toBe(false);
    expect(placeholderPanel?.classList.contains('subagent-done')).toBe(true);
    expect(
      (dom.chat?.querySelector('.subagent-modal-placeholder') as HTMLElement | null)?.style.minHeight,
    ).toBe(initialPlaceholderHeight);
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

  it('uses streamlined labels that match the orchestrate card style', () => {
    createSubagentPanel('explore', 'Inspect the logs and summarize the failure.', 'task-4b');

    const panel = dom.chat?.querySelector('.subagent-panel') as HTMLElement | null;
    const actionButtons = Array.from(
      panel?.querySelectorAll('.panel-action-btn') || [],
    ).map((button) => (button as HTMLButtonElement).textContent?.trim());
    const sectionTitles = Array.from(
      panel?.querySelectorAll('.subagent-section-title') || [],
    ).map((title) => (title as HTMLElement).textContent?.trim());

    expect(panel?.querySelector('.subagent-status')?.textContent).toBe('Running');
    expect(panel?.querySelector('.subagent-icon')?.textContent).toBe('✦');
    expect(actionButtons).toEqual(['Copy summary']);
    expect(sectionTitles).toEqual(['Task prompt', 'Tool chain']);
  });

  it('keeps history replay empty state consistent when only tool counts were saved', () => {
    createSubagentPanel('reviewer', 'Inspect the logs and summarize the failure.', 'task-4c');

    finishSubagentPanel(
      { task_id: 'task-4c', agent: 'reviewer' },
      true,
      {
        tool_calls: 3,
        result_excerpt: 'Replay restored only summary data.',
      },
      { immediate: true },
    );

    const panel = dom.chat?.querySelector('.subagent-panel') as HTMLElement | null;
    const meta = panel?.querySelector('[data-subagent-tools-meta]') as HTMLElement | null;
    const empty = panel?.querySelector('[data-subagent-tool-empty]') as HTMLElement | null;

    expect(meta?.textContent).toBe('History replay preserved 3 tool calls.');
    expect(empty?.textContent).toBe('Tool details were not saved for this history replay.');
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
    const toolBadges = panel?.querySelectorAll('.subagent-tool-pill') || [];
    const summary = panel?.querySelector('.subagent-summary') as HTMLElement | null;

    expect(reasoningBody?.textContent).toContain('Check the log file');
    expect(panel?.querySelectorAll('.subagent-tool-row') || []).toHaveLength(0);
    expect(toolBadges).toHaveLength(1);
    expect((toolBadges[0].querySelector('.subagent-tool-pill-name') as HTMLElement | null)?.textContent).toBe(
      'read_file',
    );
    expect((toolBadges[0] as HTMLButtonElement).dataset.toolResult).toContain(
      'startup config missing',
    );
    expect(summary?.classList.contains('hidden')).toBe(false);
    expect(summary?.textContent).toContain('Found the root cause in the startup logs.');
  });

  it('matches empty tool ids to the earliest running badge', () => {
    createSubagentPanel('reviewer', 'Inspect the logs and summarize the failure.', 'task-6');

    addSubagentTool({ task_id: 'task-6', agent: 'reviewer' }, 'read_file', '', '{"path":"a.log"}');
    addSubagentTool({ task_id: 'task-6', agent: 'reviewer' }, 'grep', '', '{"pattern":"panic"}');

    updateSubagentToolResult(
      { task_id: 'task-6', agent: 'reviewer' },
      '',
      18,
      'first result',
      false,
      'read_file',
    );
    updateSubagentToolResult(
      { task_id: 'task-6', agent: 'reviewer' },
      '',
      24,
      'second result',
      true,
      'grep',
    );

    const panel = dom.chat?.querySelector('.subagent-panel') as HTMLElement | null;
    const badges = Array.from(panel?.querySelectorAll('.subagent-tool-pill') || []) as HTMLButtonElement[];

    expect(badges).toHaveLength(2);
    expect(badges[0].classList.contains('is-done')).toBe(true);
    expect(badges[0].dataset.toolResult).toContain('first result');
    expect(badges[1].classList.contains('is-failed')).toBe(true);
    expect(badges[1].dataset.toolResult).toContain('second result');
  });
});
