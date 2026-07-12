import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { dom, state } from '../src/state.js';
import {
  completeExecutionStack,
  mountExecutionPanel,
  removeExecutionPanel,
  refreshExecutionStacks,
  resetExecutionStackState,
  resumeExecutionStackAutoCollapse,
  restoreExecutionStackState,
  syncAllExecutionStackVisibility,
  toggleExecutionStack,
} from '../src/renderers/execution-stack.js';
import { addToolCall, addToolResult } from '../src/renderers/tools.js';
import { setLanguage } from '../src/i18n.js';
import { animateCollapsibleSection, linkCollapsibleControl } from '../src/renderers/timeline.js';

describe('execution stack', () => {
  beforeEach(() => {
    document.body.innerHTML = '<div id="chat"></div>';
    dom.chat = document.getElementById('chat') as HTMLElement;
    dom.toolDrawer = null;
    dom.toolDrawerBackdrop = null;
    state.activeExecutionStack = null;
    state.activeToolPanel = null;
    state.currentMsg = null;
    state.currentRoundStartedAt = 0;
    state.showReasoning = true;
    state.showTools = true;
    state.autoFollowChat = false;
    setLanguage('en');
  });

  afterEach(() => {
    vi.useRealTimers();
    setLanguage('en');
    state.activeExecutionStack = null;
    state.activeToolPanel = null;
    dom.chat = null;
    document.body.innerHTML = '';
  });

  it('groups multiple dynamic panels into one active stack', () => {
    mountExecutionPanel(document.createElement('div'), 'reasoning');
    mountExecutionPanel(document.createElement('div'), 'tool');

    expect(document.querySelectorAll('.execution-stack')).toHaveLength(1);
    expect(document.querySelectorAll('.execution-step')).toHaveLength(2);
    expect(document.querySelector('.execution-stack-meta')?.textContent).toBe('2 steps');
  });

  it('completes and automatically collapses after 600ms', () => {
    vi.useFakeTimers();
    mountExecutionPanel(document.createElement('div'), 'reasoning');
    const stack = state.activeExecutionStack as HTMLElement;

    completeExecutionStack({ durationMs: 12_600 });
    expect(stack.classList.contains('is-expanded')).toBe(true);
    expect(stack.querySelector('.execution-stack-title')?.textContent).toBe('Worked');
    expect(stack.querySelector('.execution-stack-meta')?.textContent).toBe('1 step · 13s');

    vi.advanceTimersByTime(600);
    expect(stack.classList.contains('is-expanded')).toBe(false);
    expect(stack.querySelector('.execution-stack-header')?.getAttribute('aria-expanded')).toBe(
      'false',
    );
    expect(stack.hidden).toBe(false);
    expect((stack.querySelector('.execution-stack-body') as HTMLElement).hidden).toBe(true);
  });

  it('preserves a manual collapse when execution completes', () => {
    vi.useFakeTimers();
    mountExecutionPanel(document.createElement('div'), 'tool');
    const stack = state.activeExecutionStack as HTMLElement;
    toggleExecutionStack(stack.querySelector('.execution-stack-header'));

    completeExecutionStack({ durationMs: 1000 });
    vi.advanceTimersByTime(1000);

    expect(stack.dataset.executionUserToggled).toBe('true');
    expect(stack.classList.contains('is-expanded')).toBe(false);
  });

  it('returns focus to the summary before an automatic collapse hides a step control', () => {
    vi.useFakeTimers();
    const panel = document.createElement('div');
    const detailButton = document.createElement('button');
    panel.appendChild(detailButton);
    mountExecutionPanel(panel, 'tool');
    const stack = state.activeExecutionStack as HTMLElement;
    detailButton.focus();

    completeExecutionStack({ durationMs: 100 });
    vi.advanceTimersByTime(600);

    expect(stack.classList.contains('is-expanded')).toBe(false);
    expect(document.activeElement).toBe(stack.querySelector('.execution-stack-header'));
  });

  it('removes an empty stack with its final step', () => {
    const panel = document.createElement('div');
    mountExecutionPanel(panel, 'reasoning');
    removeExecutionPanel(panel);

    expect(document.querySelector('.execution-stack')).toBeNull();
    expect(state.activeExecutionStack).toBeNull();
  });

  it('filters step types and hides a stack with no visible steps', () => {
    mountExecutionPanel(document.createElement('div'), 'reasoning');
    const stack = state.activeExecutionStack as HTMLElement;

    state.showReasoning = false;
    syncAllExecutionStackVisibility();
    expect(stack.hidden).toBe(true);

    state.showReasoning = true;
    syncAllExecutionStackVisibility();
    expect(stack.hidden).toBe(false);
  });

  it('updates a tool result in place and exposes failure semantics', () => {
    const panel = addToolCall('read_file', '{"path":"README.md"}', 'tool-1') as HTMLElement;
    addToolResult('read_file', 'permission denied', 'tool-1', 250, true);

    expect(document.querySelectorAll('.execution-step--tool')).toHaveLength(1);
    expect(panel.classList.contains('tool-panel-failed')).toBe(true);
    expect(panel.querySelector('.tool-status')?.textContent).toBe('Failed (250ms)');
    expect(panel.closest('.execution-stack')?.classList.contains('is-failed')).toBe(true);
  });

  it('keeps failure semantics when failed tool steps are filtered out', () => {
    mountExecutionPanel(document.createElement('div'), 'reasoning');
    addToolCall('read_file', '{}', 'tool-filtered');
    addToolResult('read_file', 'permission denied', 'tool-filtered', 10, true);
    const stack = state.activeExecutionStack as HTMLElement;

    state.showTools = false;
    syncAllExecutionStackVisibility();

    expect(stack.querySelector('.execution-stack-title')?.textContent).toBe('Execution failed');
    expect(stack.classList.contains('is-failed')).toBe(true);
    expect(stack.querySelector('.execution-stack-meta')?.textContent).toBe('1 step');
  });

  it('does not auto-collapse while an inspector owns focus inside the stack', () => {
    vi.useFakeTimers();
    const panel = addToolCall('read_file', '{}', 'tool-open') as HTMLElement;
    const stack = state.activeExecutionStack as HTMLElement;
    state.activeToolPanel = panel;

    completeExecutionStack({ durationMs: 100 });
    vi.advanceTimersByTime(600);

    expect(stack.classList.contains('is-expanded')).toBe(true);
    expect(stack.querySelector('.execution-stack-header')?.getAttribute('aria-expanded')).toBe(
      'true',
    );

    state.activeToolPanel = null;
    const collapsedHeader = resumeExecutionStackAutoCollapse(panel);

    expect(stack.classList.contains('is-expanded')).toBe(false);
    expect(collapsedHeader).toBe(stack.querySelector('.execution-stack-header'));
    expect(stack.querySelector('.execution-stack-header')?.getAttribute('aria-expanded')).toBe(
      'false',
    );
  });

  it('synchronizes aria-expanded when a linked section is collapsed programmatically', () => {
    const panel = document.createElement('div');
    const header = document.createElement('button');
    const body = document.createElement('div');
    body.className = 'show';
    panel.append(header, body);
    document.body.appendChild(panel);
    linkCollapsibleControl(header, body, 'test-body');

    animateCollapsibleSection(body, false);

    expect(header.getAttribute('aria-expanded')).toBe('false');
  });

  it('starts a new stack after the previous execution completes', () => {
    mountExecutionPanel(document.createElement('div'), 'reasoning');
    const firstStack = state.activeExecutionStack as HTMLElement;
    completeExecutionStack({ immediate: true, durationMs: null });
    mountExecutionPanel(document.createElement('div'), 'tool');

    expect(document.querySelectorAll('.execution-stack')).toHaveLength(2);
    expect(document.querySelectorAll('.execution-stack.is-complete')).toHaveLength(1);
    expect(document.querySelectorAll('.execution-stack.is-running')).toHaveLength(1);
    expect(firstStack.isConnected).toBe(true);
    expect(firstStack.hidden).toBe(false);
    expect(state.activeExecutionStack).not.toBe(firstStack);
  });

  it('restores a running stack after history pagination temporarily detaches it', () => {
    mountExecutionPanel(document.createElement('div'), 'reasoning');
    const stack = state.activeExecutionStack as HTMLElement;
    const captured = resetExecutionStackState();
    stack.remove();

    dom.chat?.appendChild(stack);
    restoreExecutionStackState(captured);
    mountExecutionPanel(document.createElement('div'), 'tool');

    expect(state.activeExecutionStack).toBe(stack);
    expect(document.querySelectorAll('.execution-stack')).toHaveLength(1);
    expect(stack.querySelectorAll('.execution-step')).toHaveLength(2);
  });

  it('refreshes an open stack when the interface language changes', () => {
    mountExecutionPanel(document.createElement('div'), 'reasoning');
    const stack = state.activeExecutionStack as HTMLElement;

    setLanguage('zh-CN');
    refreshExecutionStacks();

    expect(stack.querySelector('.execution-stack-title')?.textContent).toBe('处理中');
    expect(stack.querySelector('.execution-stack-meta')?.textContent).toBe('1 个步骤');
  });
});
