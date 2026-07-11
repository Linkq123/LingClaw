import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  closeOrchestrateTaskModal,
  createOrchestratePanel,
  markOrchestrateTask,
  openOrchestrateTaskModal,
} from '../src/renderers/orchestrate.js';
import { appendSubagentToolOutput } from '../src/renderers/subagent.js';
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

  it('uses the shared workflow and task status icons', () => {
    mountOrchestration();

    expect(dom.chat?.querySelector('.orchestrate-icon use')?.getAttribute('href')).toBe(
      '#icon-workflow',
    );
    expect(dom.chat?.querySelector('.orchestrate-task-icon use')?.getAttribute('href')).toBe(
      '#icon-more',
    );
    expect(
      dom.chat?.querySelector('.orchestrate-header > .chevron use')?.getAttribute('href'),
    ).toBe('#icon-chevron-right');
    expect(
      dom.chat?.querySelector('.orchestrate-task-summary > .chevron use')?.getAttribute('href'),
    ).toBe('#icon-chevron-right');

    markOrchestrateTask(
      { orchestrate_id: 'orch-1', id: 'task-a', result_preview: 'Done' },
      'completed',
    );

    expect(dom.chat?.querySelector('.orchestrate-task-icon use')?.getAttribute('href')).toBe(
      '#icon-check',
    );
  });

  it('preserves streamed task output when a synthetic plan expands into multiple tasks', () => {
    createOrchestratePanel({
      orchestrate_id: 'orch-1',
      task_count: 1,
      layer_count: 1,
      synthetic: true,
      tasks: [
        {
          id: 'task-a',
          agent: 'frontend-coder',
          prompt_preview: 'Fix the hidden footer in the expanded card.',
          depends_on: [],
        },
      ],
    });

    appendSubagentToolOutput(
      { task_id: 'orch-1:task-a', agent: 'frontend-coder' },
      'tool-1',
      'stdout',
      'streamed output survives',
      'exec',
    );

    createOrchestratePanel({
      orchestrate_id: 'orch-1',
      task_count: 2,
      layer_count: 2,
      tasks: [
        {
          id: 'task-a',
          agent: 'frontend-coder',
          prompt_preview: 'Fix the hidden footer in the expanded card.',
          depends_on: [],
        },
        {
          id: 'task-b',
          agent: 'qa-reviewer',
          prompt_preview: 'Verify the visual regression fix.',
          depends_on: ['task-a'],
        },
      ],
    });

    const orchestratePanel = dom.chat?.querySelector('.orchestrate-panel') as HTMLElement | null;
    const rows = dom.chat?.querySelectorAll('.orchestrate-task') || [];
    const firstRow = rows[0] as HTMLElement | undefined;
    const secondRow = rows[1] as HTMLElement | undefined;
    const firstPanel = firstRow?.querySelector('.subagent-panel') as HTMLElement | null;

    expect(orchestratePanel?.dataset.synthetic).not.toBe('true');
    expect(rows).toHaveLength(2);
    expect(firstRow?.dataset.taskId).toBe('task-a');
    expect(secondRow?.dataset.taskId).toBe('task-b');
    expect(
      firstPanel?.querySelector('[data-subagent-tool-trail] .subagent-tool-pill'),
    ).not.toBeNull();
    expect(
      firstPanel
        ?.querySelector('[data-subagent-tool-trail] .subagent-tool-pill')
        ?.getAttribute('data-tool-live-output'),
    ).toContain('streamed output survives');
  });

  it('recomputes the header layer count when upgrading a synthetic panel without layer_count', () => {
    createOrchestratePanel({
      orchestrate_id: 'orch-1',
      task_count: 2,
      layer_count: 1,
      synthetic: true,
      tasks: [
        {
          id: 'task-a',
          agent: 'frontend-coder',
          prompt_preview: 'Fix the hidden footer in the expanded card.',
          depends_on: [],
        },
        {
          id: 'task-b',
          agent: 'qa-reviewer',
          prompt_preview: 'Verify the visual regression fix.',
          depends_on: ['task-a'],
        },
      ],
    });

    createOrchestratePanel({
      orchestrate_id: 'orch-1',
      task_count: 2,
      tasks: [
        {
          id: 'task-a',
          agent: 'frontend-coder',
          prompt_preview: 'Fix the hidden footer in the expanded card.',
          depends_on: [],
        },
        {
          id: 'task-b',
          agent: 'qa-reviewer',
          prompt_preview: 'Verify the visual regression fix.',
          depends_on: ['task-a'],
        },
      ],
    });

    const label = dom.chat?.querySelector('.orchestrate-label') as HTMLElement | null;
    expect(label?.textContent).toContain('2 layers');
  });

  it('rebuilds the task layout when a synthetic plan upgrades with different dependencies', () => {
    createOrchestratePanel({
      orchestrate_id: 'orch-1',
      task_count: 2,
      layer_count: 1,
      synthetic: true,
      tasks: [
        {
          id: 'task-a',
          agent: 'frontend-coder',
          prompt_preview: 'Fix the hidden footer in the expanded card.',
          depends_on: [],
        },
        {
          id: 'task-b',
          agent: 'qa-reviewer',
          prompt_preview: 'Verify the visual regression fix.',
          depends_on: [],
        },
      ],
    });

    createOrchestratePanel({
      orchestrate_id: 'orch-1',
      task_count: 2,
      tasks: [
        {
          id: 'task-a',
          agent: 'frontend-coder',
          prompt_preview: 'Fix the hidden footer in the expanded card.',
          depends_on: [],
        },
        {
          id: 'task-b',
          agent: 'qa-reviewer',
          prompt_preview: 'Verify the visual regression fix.',
          depends_on: ['task-a'],
        },
      ],
    });

    const layers = Array.from(dom.chat?.querySelectorAll('.orchestrate-layer') || []);
    const taskBRow = dom.chat?.querySelector('[data-task-id="task-b"]') as HTMLElement | null;
    const taskBLayer = taskBRow?.closest('.orchestrate-layer') as HTMLElement | null;

    expect(layers).toHaveLength(2);
    expect(taskBLayer?.dataset.layerIndex).toBe('1');
  });

  it('copies real prompt metadata into reused synthetic task rows', () => {
    createOrchestratePanel({
      orchestrate_id: 'orch-1',
      task_count: 1,
      layer_count: 1,
      synthetic: true,
      tasks: [
        {
          id: 'task-a',
          agent: 'frontend-coder',
          prompt_preview: '',
          depends_on: [],
        },
      ],
    });

    createOrchestratePanel({
      orchestrate_id: 'orch-1',
      task_count: 1,
      tasks: [
        {
          id: 'task-a',
          agent: 'frontend-coder',
          prompt_preview: 'Fix the hidden footer in the expanded card.',
          depends_on: [],
        },
      ],
    });

    const row = dom.chat?.querySelector('[data-task-id="task-a"]') as HTMLElement | null;
    const previewEl = row?.querySelector('.orchestrate-task-preview') as HTMLElement | null;
    const panel = row?.querySelector('.subagent-panel') as HTMLElement | null;

    expect(row?.dataset.promptPreview).toBe('Fix the hidden footer in the expanded card.');
    expect(previewEl?.textContent).toContain('Fix the hidden footer in the expanded card.');
    expect(panel?.dataset.taskId).toBe('orch-1:task-a');
  });

  it('keeps reused task panels registered to the visible row after a synthetic plan upgrades', () => {
    createOrchestratePanel({
      orchestrate_id: 'orch-1',
      task_count: 1,
      layer_count: 1,
      synthetic: true,
      tasks: [
        {
          id: 'task-a',
          agent: 'frontend-coder',
          prompt_preview: 'Initial synthetic prompt.',
          depends_on: [],
        },
      ],
    });

    createOrchestratePanel({
      orchestrate_id: 'orch-1',
      task_count: 1,
      tasks: [
        {
          id: 'task-a',
          agent: 'frontend-coder',
          prompt_preview: 'Real plan prompt.',
          depends_on: [],
        },
      ],
    });

    appendSubagentToolOutput(
      { task_id: 'orch-1:task-a', agent: 'frontend-coder' },
      'tool-1',
      'stdout',
      'visible panel keeps updating',
      'exec',
    );

    const row = dom.chat?.querySelector('[data-task-id="task-a"]') as HTMLElement | null;
    const visiblePanel = row?.querySelector('.subagent-panel') as HTMLElement | null;
    const registeredPanel = state.activeSubagentPanels.get('orch-1:task-a') as
      | HTMLElement
      | undefined;

    expect(registeredPanel).toBe(visiblePanel);
    expect(
      visiblePanel
        ?.querySelector('[data-subagent-tool-trail] .subagent-tool-pill')
        ?.getAttribute('data-tool-live-output'),
    ).toContain('visible panel keeps updating');
  });

  it('keeps reused task panels registered when a synthetic plan grows before turning real', () => {
    createOrchestratePanel({
      orchestrate_id: 'orch-1',
      task_count: 1,
      layer_count: 1,
      synthetic: true,
      tasks: [
        {
          id: 'task-a',
          agent: 'frontend-coder',
          prompt_preview: 'Initial synthetic prompt.',
          depends_on: [],
        },
      ],
    });

    createOrchestratePanel({
      orchestrate_id: 'orch-1',
      task_count: 2,
      layer_count: 1,
      synthetic: true,
      tasks: [
        {
          id: 'task-a',
          agent: 'frontend-coder',
          prompt_preview: 'Expanded synthetic prompt.',
          depends_on: [],
        },
        {
          id: 'task-b',
          agent: 'qa-reviewer',
          prompt_preview: 'Second synthetic task.',
          depends_on: [],
        },
      ],
    });

    appendSubagentToolOutput(
      { task_id: 'orch-1:task-a', agent: 'frontend-coder' },
      'tool-1',
      'stdout',
      'synthetic growth keeps the visible panel live',
      'exec',
    );

    const row = dom.chat?.querySelector('[data-task-id="task-a"]') as HTMLElement | null;
    const visiblePanel = row?.querySelector('.subagent-panel') as HTMLElement | null;
    const registeredPanel = state.activeSubagentPanels.get('orch-1:task-a') as
      | HTMLElement
      | undefined;

    expect(registeredPanel).toBe(visiblePanel);
    expect(
      visiblePanel
        ?.querySelector('[data-subagent-tool-trail] .subagent-tool-pill')
        ?.getAttribute('data-tool-live-output'),
    ).toContain('synthetic growth keeps the visible panel live');
  });

  it('copies real prompt metadata into reused rows when a synthetic plan expands', () => {
    createOrchestratePanel({
      orchestrate_id: 'orch-1',
      task_count: 1,
      layer_count: 1,
      synthetic: true,
      tasks: [
        {
          id: 'task-a',
          agent: 'frontend-coder',
          prompt_preview: '',
          depends_on: [],
        },
      ],
    });

    createOrchestratePanel({
      orchestrate_id: 'orch-1',
      task_count: 2,
      tasks: [
        {
          id: 'task-a',
          agent: 'frontend-coder',
          prompt_preview: 'Fix the hidden footer in the expanded card.',
          depends_on: [],
        },
        {
          id: 'task-b',
          agent: 'qa-reviewer',
          prompt_preview: 'Verify the visual regression fix.',
          depends_on: ['task-a'],
        },
      ],
    });

    const row = dom.chat?.querySelector('[data-task-id="task-a"]') as HTMLElement | null;
    const previewEl = row?.querySelector('.orchestrate-task-preview') as HTMLElement | null;
    const promptEl = row?.querySelector('.subagent-prompt') as HTMLElement | null;

    expect(row?.dataset.promptPreview).toBe('Fix the hidden footer in the expanded card.');
    expect(previewEl?.textContent).toContain('Fix the hidden footer in the expanded card.');
    expect(promptEl?.textContent || '').toContain('Fix the hidden footer in the expanded card.');
  });

  it('keeps same-task and expanding synthetic upgrades on the shared merge path', () => {
    createOrchestratePanel({
      orchestrate_id: 'orch-1',
      task_count: 1,
      layer_count: 1,
      synthetic: true,
      tasks: [
        {
          id: 'task-a',
          agent: 'frontend-coder',
          prompt_preview: '',
          depends_on: [],
        },
      ],
    });

    createOrchestratePanel({
      orchestrate_id: 'orch-1',
      task_count: 1,
      tasks: [
        {
          id: 'task-a',
          agent: 'frontend-coder',
          prompt_preview: 'Fix the hidden footer in the expanded card.',
          depends_on: [],
        },
      ],
    });

    createOrchestratePanel({
      orchestrate_id: 'orch-1',
      task_count: 2,
      tasks: [
        {
          id: 'task-a',
          agent: 'frontend-coder',
          prompt_preview: 'Fix the hidden footer in the expanded card.',
          depends_on: [],
        },
        {
          id: 'task-b',
          agent: 'qa-reviewer',
          prompt_preview: 'Verify the visual regression fix.',
          depends_on: ['task-a'],
        },
      ],
    });

    const rows = Array.from(dom.chat?.querySelectorAll('.orchestrate-task') || []);
    const taskARow = dom.chat?.querySelector('[data-task-id="task-a"]') as HTMLElement | null;
    const taskAPanel = taskARow?.querySelector('.subagent-panel') as HTMLElement | null;
    const label = dom.chat?.querySelector('.orchestrate-label') as HTMLElement | null;

    expect(rows).toHaveLength(2);
    expect(taskARow?.dataset.promptPreview).toBe('Fix the hidden footer in the expanded card.');
    expect(taskAPanel?.dataset.taskId).toBe('orch-1:task-a');
    expect(label?.textContent).toContain('2 layers');
  });

  it('does not reuse another task panel when the same agent appears twice', () => {
    createOrchestratePanel({
      orchestrate_id: 'orch-1',
      task_count: 2,
      layer_count: 1,
      tasks: [
        {
          id: 'task-a',
          agent: 'frontend-coder',
          prompt_preview: 'Fix the first issue.',
          depends_on: [],
        },
        {
          id: 'task-b',
          agent: 'frontend-coder',
          prompt_preview: 'Fix the second issue.',
          depends_on: [],
        },
      ],
    });

    appendSubagentToolOutput(
      { task_id: 'orch-1:task-b', agent: 'frontend-coder' },
      'tool-2',
      'stdout',
      'belongs to task-b',
      'exec',
    );

    const rows = dom.chat?.querySelectorAll('.orchestrate-task') || [];
    const firstPanel = (rows[0] as HTMLElement | undefined)?.querySelector(
      '.subagent-panel',
    ) as HTMLElement | null;
    const secondPanel = (rows[1] as HTMLElement | undefined)?.querySelector(
      '.subagent-panel',
    ) as HTMLElement | null;

    expect(firstPanel?.querySelector('[data-subagent-tool-trail] .subagent-tool-pill')).toBeNull();
    expect(
      secondPanel
        ?.querySelector('[data-subagent-tool-trail] .subagent-tool-pill')
        ?.getAttribute('data-tool-live-output'),
    ).toContain('belongs to task-b');
  });

  it('reuses the shared sub-agent modal while open and restores it on close', () => {
    mountOrchestration();

    const orchestratePanel = dom.chat?.querySelector('.orchestrate-panel') as HTMLElement | null;
    const row = dom.chat?.querySelector('.orchestrate-task') as HTMLElement | null;
    const summary = row?.querySelector('.orchestrate-task-summary') as HTMLElement | null;
    const sharedPanel = row?.querySelector('.subagent-panel') as HTMLElement | null;
    const sharedHost = sharedPanel?.parentElement as HTMLElement | null;
    const scrollIntoViewSpy = Element.prototype.scrollIntoView as unknown as ReturnType<
      typeof vi.fn
    >;

    expect(orchestratePanel?.querySelector('[data-action="orchestrate-toggle-all"]')).toBeNull();
    expect(orchestratePanel?.querySelector('[data-action="orchestrate-focus-active"]')).toBeNull();
    expect(orchestratePanel?.querySelector('[data-action="orchestrate-copy-summary"]')).toBeNull();
    expect(row?.querySelector('.orchestrate-task-details')).toBeNull();
    expect(sharedHost?.parentElement).toBe(row);

    openOrchestrateTaskModal(summary);

    expect(row?.parentElement?.classList.contains('orchestrate-task-grid')).toBe(true);
    expect(summary?.isConnected).toBe(true);
    expect(sharedHost?.classList.contains('subagent-modal-host')).toBe(true);
    expect(sharedHost?.parentElement).toBe(document.body);
    expect(document.querySelector('.subagent-modal-placeholder')).not.toBeNull();
    expect(
      (document.querySelector('.subagent-modal-placeholder') as HTMLElement | null)?.style
        .minHeight,
    ).not.toBe('');
    expect(document.querySelector('.subagent-modal-placeholder .subagent-panel')).not.toBeNull();
    expect(sharedPanel?.classList.contains('subagent-modal-open')).toBe(true);
    expect(document.getElementById('subagent-modal-backdrop')?.hidden).toBe(false);
    expect(sharedPanel?.querySelector('.subagent-modal-close')).toBe(document.activeElement);
    expect(summary?.getAttribute('aria-expanded')).toBe('true');
    expect(scrollIntoViewSpy).not.toHaveBeenCalled();

    closeOrchestrateTaskModal();

    expect(sharedHost?.classList.contains('subagent-modal-host')).toBe(false);
    expect(sharedHost?.parentElement).toBe(row);
    expect(document.querySelector('.subagent-modal-placeholder')).toBeNull();
    expect(sharedPanel?.classList.contains('subagent-modal-open')).toBe(false);
    expect(document.getElementById('subagent-modal-backdrop')?.hidden).toBe(true);
    expect(summary?.getAttribute('aria-expanded')).toBe('false');
  });

  it('closes the task modal when tools are hidden', () => {
    mountOrchestration();

    const row = dom.chat?.querySelector('.orchestrate-task') as HTMLElement | null;
    const summary = row?.querySelector('.orchestrate-task-summary') as HTMLElement | null;
    const sharedPanel = row?.querySelector('.subagent-panel') as HTMLElement | null;
    const sharedHost = sharedPanel?.parentElement as HTMLElement | null;
    const closeToolDrawer = vi.fn();
    const closeSubagentModal = vi.fn();

    openOrchestrateTaskModal(summary);
    expect(sharedHost?.parentElement).toBe(document.body);

    applyToolsVisibility(false, {
      state,
      chat: dom.chat,
      closeToolDrawer,
      closeSubagentModal,
      closeOrchestrateTaskModal,
    });

    expect(closeToolDrawer).toHaveBeenCalledTimes(1);
    expect(closeSubagentModal).toHaveBeenCalledTimes(1);
    expect(sharedPanel?.classList.contains('subagent-modal-open')).toBe(false);
    expect(sharedHost?.parentElement).toBe(row);
    expect(dom.chat?.classList.contains('hide-tools')).toBe(true);
    expect(document.getElementById('subagent-modal-backdrop')?.hidden).toBe(true);
  });

  it('keeps the task row visible and syncs failure details into the shared modal', () => {
    mountOrchestration();

    const row = dom.chat?.querySelector('.orchestrate-task') as HTMLElement | null;
    const summary = row?.querySelector('.orchestrate-task-summary') as HTMLElement | null;
    const sharedPanel = row?.querySelector('.subagent-panel') as HTMLElement | null;

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

    expect(row?.classList.contains('orchestrate-task-failed')).toBe(true);
    expect(summary?.isConnected).toBe(true);
    expect(sharedPanel?.querySelector('.subagent-summary')?.textContent).toContain(
      'Task failed while the modal was open.',
    );
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
    const sharedPrompt = row?.querySelector('.subagent-prompt');
    expect(previewEl?.textContent).toContain('Fix the hidden footer in the expanded card.');
    expect(previewEl?.textContent).not.toContain('Delegated Task Context');
    expect(sharedPrompt?.textContent).toContain('Fix the hidden footer in the expanded card.');
    expect(sharedPrompt?.textContent).not.toContain('Delegated Task Context');
  });

  it('syncs completed task summaries into the shared sub-agent modal', () => {
    mountOrchestration();

    const row = dom.chat?.querySelector('.orchestrate-task') as HTMLElement | null;
    const trigger = row?.querySelector('.orchestrate-task-summary') as HTMLElement | null;
    const sharedPanel = row?.querySelector('.subagent-panel') as HTMLElement | null;

    openOrchestrateTaskModal(trigger);
    const initialPlaceholder = dom.chat?.querySelector(
      '.subagent-modal-placeholder',
    ) as HTMLElement | null;
    const initialPlaceholderHeight = initialPlaceholder?.style.minHeight;

    markOrchestrateTask(
      {
        orchestrate_id: 'orch-1',
        id: 'task-a',
        cycles: 2,
        tool_calls: 1,
        duration_ms: 480,
        result_excerpt: 'The footer clipping came from a stale overflow rule.',
      },
      'completed',
    );

    const body = sharedPanel?.querySelector('.subagent-body') as HTMLElement | null;
    const summary = sharedPanel?.querySelector('.subagent-summary') as HTMLElement | null;
    const status = sharedPanel?.querySelector('.subagent-status') as HTMLElement | null;
    const placeholderPanel = dom.chat?.querySelector(
      '.subagent-modal-placeholder .subagent-panel',
    ) as HTMLElement | null;

    expect(sharedPanel?.classList.contains('subagent-modal-open')).toBe(true);
    expect(body?.classList.contains('show')).toBe(true);
    expect(summary?.classList.contains('hidden')).toBe(false);
    expect(summary?.textContent).toContain('The footer clipping came from a stale overflow rule.');
    expect(status?.textContent).toContain('Completed');
    expect(status?.textContent).toContain('2 cycles');
    expect(placeholderPanel?.classList.contains('subagent-active')).toBe(false);
    expect(placeholderPanel?.classList.contains('subagent-done')).toBe(true);
    expect(
      (dom.chat?.querySelector('.subagent-modal-placeholder') as HTMLElement | null)?.style
        .minHeight,
    ).toBe(initialPlaceholderHeight);
  });
});
