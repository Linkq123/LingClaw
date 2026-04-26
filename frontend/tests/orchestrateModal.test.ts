import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  closeOrchestrateTaskModal,
  createOrchestratePanel,
  markOrchestrateTask,
  openOrchestrateTaskModal,
} from '../src/renderers/orchestrate.js';
import { dom, state } from '../src/state.js';
import { applyToolsVisibility } from '../src/viewState.js';

let originalScrollIntoView: typeof Element.prototype.scrollIntoView | undefined;

describe('orchestrate task modal hosting', () => {
  beforeEach(() => {
    document.body.innerHTML = '<div id="chat"></div>';
    dom.chat = document.getElementById('chat') as HTMLElement;
    state.currentMsg = null;
    state.activeOrchestrations.clear();
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
    closeOrchestrateTaskModal();
    state.activeOrchestrations.clear();
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

  function mountOrchestration() {
    createOrchestratePanel({
      orchestrate_id: 'orch-1',
      task_count: 1,
      layer_count: 1,
      tasks: [
        {
          id: 'task-a',
          agent: 'frontend-coder',
          prompt_preview: 'Fix the hidden footer in the expanded card.',
          depends_on: [],
        },
      ],
    });
  }

  it('moves the task card to body while open and restores it on close', () => {
    mountOrchestration();

    const row = dom.chat?.querySelector('.orchestrate-task') as HTMLElement | null;
    const originalParent = row?.parentElement;
    const summary = row?.querySelector('.orchestrate-task-summary') as HTMLElement | null;
    const details = row?.querySelector('.orchestrate-task-details') as HTMLElement | null;
    const scrollIntoViewSpy = Element.prototype.scrollIntoView as unknown as ReturnType<typeof vi.fn>;

    expect(originalParent).not.toBeNull();
    expect(details?.classList.contains('show')).toBe(false);

    openOrchestrateTaskModal(summary);

    expect(row?.parentElement).toBe(document.body);
    expect(document.querySelector('.orchestrate-task-modal-placeholder')).not.toBeNull();
    expect(row?.classList.contains('orchestrate-task-modal-open')).toBe(true);
    expect(details?.classList.contains('show')).toBe(true);
    expect(document.getElementById('orchestrate-task-modal-backdrop')?.hidden).toBe(false);
    expect(row?.querySelector('.orchestrate-task-modal-close')).toBe(document.activeElement);
    expect(scrollIntoViewSpy).not.toHaveBeenCalled();

    closeOrchestrateTaskModal();

    expect(row?.parentElement).toBe(originalParent);
    expect(document.querySelector('.orchestrate-task-modal-placeholder')).toBeNull();
    expect(row?.classList.contains('orchestrate-task-modal-open')).toBe(false);
    expect(details?.classList.contains('show')).toBe(false);
    expect(document.getElementById('orchestrate-task-modal-backdrop')?.hidden).toBe(true);
  });

  it('closes the task modal when tools are hidden', () => {
    mountOrchestration();

    const row = dom.chat?.querySelector('.orchestrate-task') as HTMLElement | null;
    const summary = row?.querySelector('.orchestrate-task-summary') as HTMLElement | null;
    const closeToolDrawer = vi.fn();
    const closeSubagentModal = vi.fn();

    openOrchestrateTaskModal(summary);
    expect(row?.parentElement).toBe(document.body);

    applyToolsVisibility(false, {
      state,
      chat: dom.chat,
      closeToolDrawer,
      closeSubagentModal,
      closeOrchestrateTaskModal,
    });

    expect(closeToolDrawer).toHaveBeenCalledTimes(1);
    expect(closeSubagentModal).toHaveBeenCalledTimes(1);
    expect(row?.classList.contains('orchestrate-task-modal-open')).toBe(false);
    expect(row?.parentElement).not.toBe(document.body);
    expect(dom.chat?.classList.contains('hide-tools')).toBe(true);
    expect(document.getElementById('orchestrate-task-modal-backdrop')?.hidden).toBe(true);
  });

  it('keeps failure-triggered details expanded after closing the modal', () => {
    mountOrchestration();

    const row = dom.chat?.querySelector('.orchestrate-task') as HTMLElement | null;
    const summary = row?.querySelector('.orchestrate-task-summary') as HTMLElement | null;
    const details = row?.querySelector('.orchestrate-task-details') as HTMLElement | null;

    openOrchestrateTaskModal(summary);
    markOrchestrateTask(
      {
        orchestrate_id: 'orch-1',
        id: 'task-a',
        error: 'Task failed while the modal was open.',
      },
      'failed',
    );

    closeOrchestrateTaskModal();

    expect(row?.classList.contains('orchestrate-task-modal-open')).toBe(false);
    expect(row?.classList.contains('orchestrate-task-failed')).toBe(true);
    expect(details?.classList.contains('show')).toBe(true);
    expect(details?.classList.contains('is-open')).toBe(true);
  });
  it('strips delegated runtime context from task prompts shown in the card', () => {
    mountOrchestration();

    const row = dom.chat?.querySelector('.orchestrate-task') as HTMLElement | null;

    markOrchestrateTask(
      {
        orchestrate_id: 'orch-1',
        id: 'task-a',
        prompt:
          '## Delegated Task Context\n- Current system local time: 2026-04-27 09:30:00 +08:00\n\n## Delegated Task\nFix the hidden footer in the expanded card.',
      },
      'running',
    );

    const previewEl = row?.querySelector('.orchestrate-task-preview');
    const promptSection = row?.querySelector('[data-orchestrate-section="prompt"]');
    expect(previewEl?.textContent).toContain('Fix the hidden footer in the expanded card.');
    expect(previewEl?.textContent).not.toContain('Delegated Task Context');
    expect(promptSection?.textContent).toContain('Fix the hidden footer in the expanded card.');
    expect(promptSection?.textContent).not.toContain('Delegated Task Context');
  });
});
