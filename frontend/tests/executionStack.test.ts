import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { dom, state } from '../src/state.js';
import {
  completeExecutionStack,
  completeExecutionStackForPlan,
  doneExecutionOutcome,
  focusExecutionStackRecovery,
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
    state.terminalExecutionStack = null;
    state.activeExecutionRunId = 0;
    state.activeExecutionServerRunId = '';
    state.activeExecutionPlanId = '';
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
    vi.unstubAllGlobals();
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
    expect(document.querySelector('.execution-stack-meta')?.textContent).toBe('0/1 complete');
  });

  it('completes and automatically collapses after 600ms', () => {
    vi.useFakeTimers();
    const panel = document.createElement('div');
    panel.dataset.executionAction = 'Review reasoning';
    panel.dataset.executionResult = 'Completed';
    panel.dataset.executionState = 'completed';
    mountExecutionPanel(panel, 'reasoning');
    const stack = state.activeExecutionStack as HTMLElement;

    completeExecutionStack({ durationMs: 12_600 });
    expect(stack.classList.contains('is-expanded')).toBe(true);
    expect(stack.querySelector('.execution-stack-title')?.textContent).toBe(
      'Completed: Review reasoning',
    );
    expect(stack.querySelector('.execution-stack-summary')?.textContent).toBe('Completed');
    expect(stack.querySelector('.execution-stack-meta')?.textContent).toBe('1/1 complete · 13s');

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

    expect(stack.querySelector('.execution-stack-title')?.textContent).toContain('Failed at: Read');
    expect(stack.classList.contains('is-failed')).toBe(true);
    expect(stack.querySelector('.execution-stack-meta')?.textContent).toBe('0/1 complete');
  });

  it('keeps an attention outcome and recovery visible when every inspector is filtered out', () => {
    addToolCall('read_file', '{}', 'tool-hidden-failure');
    addToolResult('read_file', 'permission denied', 'tool-hidden-failure', 10, true);
    const stack = state.activeExecutionStack as HTMLElement;

    state.showReasoning = false;
    state.showTools = false;
    completeExecutionStack({
      status: 'failed',
      summary: 'README.md could not be read',
    });
    syncAllExecutionStackVisibility();

    expect(stack.hidden).toBe(false);
    expect(stack.querySelector<HTMLElement>('.execution-stack-recovery')?.hidden).toBe(false);
    expect(stack.querySelector('.execution-stack-summary')?.textContent).toBe(
      'README.md could not be read',
    );
    expect(stack.querySelector('.execution-stack-recovery-summary')?.textContent).toBe(
      'This run needs attention before continuing.',
    );
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
    expect(stack.querySelector('.execution-stack-meta')?.textContent).toBe('已完成 0/1');
  });

  it.each(['failed', 'blocked', 'waiting_user', 'partial', 'stopped', 'incomplete'] as const)(
    'never auto-collapses the %s outcome and keeps recovery visible',
    (status) => {
      vi.useFakeTimers();
      const panel = document.createElement('div');
      panel.dataset.executionAction = 'Verify build';
      panel.dataset.executionResult = 'Needs attention';
      panel.dataset.executionState = status === 'failed' ? 'failed' : status;
      mountExecutionPanel(panel, 'tool');
      const stack = state.activeExecutionStack as HTMLElement;

      completeExecutionStack({ status, summary: `Outcome ${status}` });
      vi.advanceTimersByTime(1200);

      expect(stack.dataset.executionStatus).toBe(status);
      expect(stack.classList.contains('is-expanded')).toBe(true);
      expect(stack.querySelector<HTMLElement>('.execution-stack-recovery')?.hidden).toBe(false);
      expect(stack.querySelector('.execution-stack-summary')?.textContent).toBe(
        `Outcome ${status}`,
      );
    },
  );

  it('lets a manual collapse win even when the outcome needs attention', () => {
    vi.useFakeTimers();
    const panel = document.createElement('div');
    panel.dataset.executionAction = 'Verify build';
    mountExecutionPanel(panel, 'tool');
    const stack = state.activeExecutionStack as HTMLElement;
    toggleExecutionStack(stack.querySelector('.execution-stack-header'));

    completeExecutionStack({ status: 'blocked' });
    vi.advanceTimersByTime(1200);

    expect(stack.dataset.executionUserToggled).toBe('true');
    expect(stack.classList.contains('is-expanded')).toBe(false);
    expect(stack.querySelector<HTMLElement>('.execution-stack-recovery')?.hidden).toBe(false);
  });

  it('keeps a keyboard recovery target available and respects reduced motion', () => {
    const panel = document.createElement('div');
    panel.className = 'tool-panel-failed';
    panel.dataset.executionAction = 'Verify build';
    panel.dataset.executionState = 'failed';
    const detail = document.createElement('button');
    panel.appendChild(detail);
    const scrollIntoView = vi.fn();
    detail.scrollIntoView = scrollIntoView;
    vi.stubGlobal(
      'matchMedia',
      vi.fn().mockReturnValue({
        matches: true,
        addEventListener: vi.fn(),
        removeEventListener: vi.fn(),
      }),
    );
    mountExecutionPanel(panel, 'tool');
    const stack = state.activeExecutionStack as HTMLElement;
    completeExecutionStack({ status: 'failed' });

    focusExecutionStackRecovery(stack.querySelector('.execution-stack-recovery-action'));

    expect(document.activeElement).toBe(detail);
    expect(scrollIntoView).toHaveBeenCalledWith({ block: 'center', behavior: 'auto' });
    vi.unstubAllGlobals();
  });

  it('reveals a filtered failure step before focusing its recovery target', () => {
    const panel = document.createElement('div');
    panel.className = 'tool-panel-failed';
    panel.dataset.executionAction = 'Read';
    panel.dataset.executionState = 'failed';
    const detail = document.createElement('button');
    panel.appendChild(detail);
    mountExecutionPanel(panel, 'tool');
    const stack = state.activeExecutionStack as HTMLElement;
    const step = panel.closest<HTMLElement>('.execution-step')!;
    completeExecutionStack({ status: 'failed', recoveryTarget: detail });
    state.showTools = false;
    syncAllExecutionStackVisibility();
    expect(step.hidden).toBe(true);

    focusExecutionStackRecovery(stack.querySelector('.execution-stack-recovery-action'));

    expect(step.hidden).toBe(false);
    expect(stack.classList.contains('is-expanded')).toBe(true);
    expect(document.activeElement).toBe(detail);
  });

  it('resolves a replacement Plan card by exact plan id and revision on every recovery click', () => {
    state.executionRunSequence += 1;
    state.activeExecutionRunId = state.executionRunSequence;
    state.activeExecutionServerRunId = 'plan-recovery-run';
    state.activeExecutionPlanId = 'plan-recovery';
    mountExecutionPanel(document.createElement('div'), 'tool');
    const stack = state.activeExecutionStack as HTMLElement;
    const oldCard = document.createElement('article');
    oldCard.className = 'plan-artifact-card';
    oldCard.dataset.planId = 'plan-recovery';
    oldCard.dataset.planRevision = '2';
    document.body.appendChild(oldCard);
    completeExecutionStackForPlan({
      plan_id: 'plan-recovery',
      revision: 2,
      status: 'stopped',
      message_index: 1,
      created_at: 1,
      updated_at: 2,
      approved_at: 2,
      execution_attempt: 1,
      artifact: { title: 'Plan', goal: 'Recover', steps: [] },
      progress: [],
    });
    oldCard.remove();
    const replacement = document.createElement('article');
    replacement.className = 'plan-artifact-card';
    replacement.dataset.planId = 'plan-recovery';
    replacement.dataset.planRevision = '2';
    const unrelatedRevision = replacement.cloneNode(true) as HTMLElement;
    unrelatedRevision.dataset.planRevision = '3';
    document.body.appendChild(unrelatedRevision);
    document.body.appendChild(replacement);

    focusExecutionStackRecovery(stack.querySelector('.execution-stack-recovery-action'));

    expect(stack.dataset.executionStatus).toBe('stopped');
    expect(stack.classList.contains('is-stopped')).toBe(true);
    expect(stack.classList.contains('is-failed')).toBe(false);
    expect(document.activeElement).toBe(replacement);
    expect(replacement.tabIndex).toBe(-1);
  });

  it.each([
    { planStatus: 'failed' as const, stepStatus: 'in_progress' as const, expected: 'failed' },
    { planStatus: 'failed' as const, stepStatus: 'blocked' as const, expected: 'blocked' },
    { planStatus: 'stopped' as const, stepStatus: 'in_progress' as const, expected: 'stopped' },
    {
      planStatus: 'needs_input' as const,
      stepStatus: 'pending' as const,
      expected: 'waiting_user',
    },
    { planStatus: 'completed' as const, stepStatus: 'pending' as const, expected: 'partial' },
  ])(
    'keeps the Plan $expected terminal fact above later generic done outcomes',
    ({ planStatus, stepStatus, expected }) => {
      const planId = `plan-terminal-${expected}`;
      state.executionRunSequence += 1;
      state.activeExecutionRunId = state.executionRunSequence;
      state.activeExecutionServerRunId = `plan-terminal-${expected}-run`;
      state.activeExecutionPlanId = planId;
      mountExecutionPanel(document.createElement('div'), 'tool');
      const stack = state.activeExecutionStack as HTMLElement;
      const card = document.createElement('article');
      card.className = 'plan-artifact-card';
      card.dataset.planId = planId;
      document.body.appendChild(card);

      completeExecutionStackForPlan({
        plan_id: planId,
        revision: 2,
        status: planStatus,
        message_index: 1,
        created_at: 1,
        updated_at: 2,
        approved_at: 2,
        execution_attempt: 1,
        artifact: {
          title: 'Plan',
          goal: 'Keep the exact terminal fact',
          steps: [{ id: 'implement', title: 'Implement' }],
        },
        progress: [{ id: 'implement', title: 'Implement', status: stepStatus }],
        run_finished_with_unreported_steps: planStatus === 'completed',
        unfinished_steps: planStatus === 'completed' ? 1 : 0,
      });
      const planSummary = stack.querySelector('.execution-stack-summary')?.textContent;

      completeExecutionStack({
        stack,
        status: 'failed',
        summary: 'Generic later failure',
        mergeWithExisting: true,
        terminalSource: 'done',
      });

      expect(stack.dataset.executionStatus).toBe(expected);
      expect(stack.dataset.executionTerminalSource).toBe('plan');
      expect(stack.querySelector('.execution-stack-summary')?.textContent).toBe(planSummary);
      expect(state.terminalExecutionStack).toBe(stack);
    },
  );

  it('falls back to the durable outcome summary when no recovery target exists', () => {
    mountExecutionPanel(document.createElement('div'), 'tool');
    const stack = state.activeExecutionStack as HTMLElement;
    completeExecutionStack({ status: 'failed', summary: 'Storage is protected' });
    toggleExecutionStack(stack.querySelector('.execution-stack-header'));
    expect(stack.classList.contains('is-expanded')).toBe(false);

    focusExecutionStackRecovery(stack.querySelector('.execution-stack-recovery-action'));

    const summary = stack.querySelector<HTMLElement>('.execution-stack-recovery-summary');
    expect(stack.classList.contains('is-expanded')).toBe(true);
    expect(document.activeElement).toBe(summary);
    expect(summary?.tabIndex).toBe(-1);
  });

  it('requests the exact deferred Plan identity and does not open a newer revision when it is missing', () => {
    mountExecutionPanel(document.createElement('div'), 'tool');
    const stack = state.activeExecutionStack as HTMLElement;
    completeExecutionStack({
      status: 'failed',
      recoveryPlanId: 'missing-old-revision',
      recoveryPlanRevision: 2,
    });
    const newer = document.createElement('article');
    newer.className = 'plan-artifact-card';
    newer.dataset.planId = 'missing-old-revision';
    newer.dataset.planRevision = '3';
    document.body.appendChild(newer);
    const reveal = vi.fn(() => null);

    focusExecutionStackRecovery(stack.querySelector('.execution-stack-recovery-action'), reveal);

    expect(reveal).toHaveBeenCalledExactlyOnceWith({
      sessionId: stack.dataset.executionSessionId,
      planId: 'missing-old-revision',
      revision: 2,
    });
    expect(document.activeElement).toBe(stack.querySelector('.execution-stack-recovery-summary'));
    expect(document.activeElement).not.toBe(newer);
  });

  it.each(['detached', 'other-session', 'invalid-revision'])(
    'does not load Plan history for a %s recovery identity',
    (invalidIdentity) => {
      mountExecutionPanel(document.createElement('div'), 'tool');
      const stack = state.activeExecutionStack as HTMLElement;
      completeExecutionStack({
        status: 'failed',
        recoveryPlanId: 'invalid-recovery',
        recoveryPlanRevision: invalidIdentity === 'invalid-revision' ? 0 : 2,
      });
      if (invalidIdentity === 'detached') stack.remove();
      if (invalidIdentity === 'other-session') stack.dataset.executionSessionId = 'another-session';
      const reveal = vi.fn(() => null);

      focusExecutionStackRecovery(stack.querySelector('.execution-stack-recovery-action'), reveal);

      expect(reveal).not.toHaveBeenCalled();
    },
  );

  it('marks prior failures for the same structured Tool target as recovered', () => {
    addToolCall('read_file', '{"path":"README.md","end_line":0}', 'read-first');
    addToolResult('read_file', 'invalid end_line', 'read-first', 3, true);
    addToolCall('read_file', '{"end_line":80,"path":"README.md"}', 'read-retry');
    addToolResult('read_file', 'README contents', 'read-retry', 4, false);
    const stack = state.activeExecutionStack as HTMLElement;

    completeExecutionStack({ status: 'completed', immediate: true });

    expect(stack.querySelectorAll('[data-execution-recovered="true"]')).toHaveLength(1);
    expect(stack.querySelector('.execution-stack-meta')?.textContent).toContain(
      'Recovered after 1 retry',
    );
    expect(stack.querySelector('.execution-stack-meta')?.textContent).not.toContain('unresolved');
    expect(stack.classList.contains('is-failed')).toBe(false);
  });

  it('does not let a successful Tool call recover a different structured target', () => {
    addToolCall('read_file', '{"path":"README.md"}', 'read-a');
    addToolResult('read_file', 'denied', 'read-a', 3, true);
    addToolCall('read_file', '{"path":"AGENTS.md"}', 'read-b');
    addToolResult('read_file', 'rules', 'read-b', 4, false);
    const stack = state.activeExecutionStack as HTMLElement;

    completeExecutionStack({ ...doneExecutionOutcome('finish', 'complete'), immediate: true });

    expect(stack.dataset.executionStatus).toBe('partial');
    expect(stack.querySelector('.execution-stack-header')?.getAttribute('aria-expanded')).toBe(
      'true',
    );
    expect(stack.querySelectorAll('[data-execution-recovered="true"]')).toHaveLength(0);
    expect(stack.querySelector('.execution-stack-meta')?.textContent).toContain('1 unresolved');
    expect(stack.classList.contains('is-failed')).toBe(true);
  });

  it('recovers every prior failure after multiple retries of the same Tool target', () => {
    addToolCall('read_file', '{"path":"README.md","start_line":500}', 'retry-many-a');
    addToolResult('read_file', 'past end of file', 'retry-many-a', 2, true);
    addToolCall('read_file', '{"path":"README.md","start_line":200}', 'retry-many-b');
    addToolResult('read_file', 'past end of file', 'retry-many-b', 2, true);
    addToolCall('read_file', '{"path":"README.md","start_line":1}', 'retry-many-c');
    addToolResult('read_file', 'README contents', 'retry-many-c', 3, false);
    const stack = state.activeExecutionStack as HTMLElement;

    completeExecutionStack({ status: 'completed', immediate: true });

    expect(stack.querySelectorAll('[data-execution-recovered="true"]')).toHaveLength(2);
    expect(stack.querySelector('.execution-stack-meta')?.textContent).toContain(
      'Recovered after 2 retries',
    );
    expect(stack.querySelector('.execution-stack-meta')?.textContent).not.toContain('unresolved');
  });

  it('keeps the final failed retry unresolved when no later success supersedes it', () => {
    addToolCall('read_file', '{"path":"README.md","start_line":500}', 'retry-final-a');
    addToolResult('read_file', 'past end of file', 'retry-final-a', 2, true);
    addToolCall('read_file', '{"path":"README.md","start_line":200}', 'retry-final-b');
    addToolResult('read_file', 'still past end of file', 'retry-final-b', 2, true);
    const stack = state.activeExecutionStack as HTMLElement;

    completeExecutionStack({ status: 'failed', immediate: true });

    expect(stack.querySelectorAll('[data-execution-recovered="true"]')).toHaveLength(0);
    expect(stack.querySelector('.execution-stack-meta')?.textContent).toContain('2 unresolved');
    expect(stack.classList.contains('is-failed')).toBe(true);
  });

  it('relocalizes stored outcome and recovery keys without changing stack identity', () => {
    const panel = document.createElement('div');
    panel.dataset.executionAction = 'Verify';
    mountExecutionPanel(panel, 'tool');
    const stack = state.activeExecutionStack as HTMLElement;
    completeExecutionStack({
      stack,
      status: 'stopped',
      summary: 'The run was stopped before completion.',
      summaryKey: 'execution.stoppedSummary',
      recoveryLabel: 'Review stopped work',
      recoveryLabelKey: 'execution.reviewInterrupted',
    });

    setLanguage('zh-CN');
    refreshExecutionStacks();

    expect(stack.querySelector('.execution-stack-summary')?.textContent).toBe(
      '本次运行在完成前被停止。',
    );
    expect(stack.querySelector('.execution-stack-recovery-action')?.textContent).toBe(
      '查看已停止工作',
    );
    expect(stack.dataset.executionRunId).toBeTruthy();
  });

  it.each([
    ['failed', 'execution.failedSummary', 'execution.reviewError'],
    ['blocked', 'execution.blockedSummary', 'execution.reviewDetails'],
    ['waiting_user', 'execution.waiting_userSummary', 'execution.reviewDetails'],
    ['partial', 'execution.unknownTerminalSummary', 'execution.reviewDetails'],
    ['incomplete', 'execution.incompleteSummary', 'execution.reviewIncomplete'],
  ] as const)(
    'relocalizes a persisted %s outcome without changing identity or disclosure',
    (status, summaryKey, recoveryKey) => {
      const panel = document.createElement('div');
      panel.dataset.executionAction = 'Inspect';
      mountExecutionPanel(panel, 'tool');
      const stack = state.activeExecutionStack as HTMLElement;
      completeExecutionStack({
        stack,
        status,
        summary: 'English terminal summary',
        summaryKey,
        recoveryLabel: 'English recovery',
        recoveryLabelKey: recoveryKey,
      });
      const identity = stack.dataset.executionRunId;
      const expanded = stack
        .querySelector('.execution-stack-header')
        ?.getAttribute('aria-expanded');

      setLanguage('zh-CN');
      refreshExecutionStacks();
      expect(stack.dataset.executionRunId).toBe(identity);
      expect(stack.querySelector('.execution-stack-header')?.getAttribute('aria-expanded')).toBe(
        expanded,
      );
      expect(stack.querySelector('.execution-stack-summary')?.textContent).not.toBe(
        'English terminal summary',
      );
      expect(stack.querySelector('.execution-stack-recovery-action')?.textContent).not.toBe(
        'English recovery',
      );

      setLanguage('en');
      refreshExecutionStacks();
      expect(stack.dataset.executionRunId).toBe(identity);
    },
  );

  it('turns an authoritative discarded Plan into a neutral terminal without recovery', () => {
    state.executionRunSequence += 1;
    state.activeExecutionRunId = state.executionRunSequence;
    state.activeExecutionServerRunId = 'discard-plan-run';
    state.activeExecutionPlanId = 'discard-plan';
    mountExecutionPanel(document.createElement('div'), 'reasoning');
    const stack = state.activeExecutionStack as HTMLElement;
    stack.dataset.executionPlanId = 'discard-plan';

    completeExecutionStackForPlan({
      plan_id: 'discard-plan',
      revision: 1,
      status: 'discarded',
      message_index: 1,
      created_at: 1,
      updated_at: 2,
      artifact: { title: 'Plan', goal: 'Discard it', steps: [] },
      progress: [],
    });

    expect(stack.dataset.executionStatus).toBe('discarded');
    expect(stack.querySelector('.execution-stack-summary')?.textContent).toBe(
      'The formal Plan was discarded. No response or recovery action is pending.',
    );
    expect(stack.querySelector<HTMLElement>('.execution-stack-recovery')?.hidden).toBe(true);
  });

  it.each([
    ['finish', 'complete', 'completed'],
    ['stopped', 'user_stop', 'stopped'],
    ['hard_cap', 'hard_cap', 'incomplete'],
    ['failed', 'incomplete_plan', 'incomplete'],
    ['failed', 'completion_contract_failed', 'failed'],
    ['finish', 'empty', 'incomplete'],
    [undefined, undefined, 'partial'],
  ] as const)('maps done phase=%s reason=%s to %s', (phase, reason, expected) => {
    expect(doneExecutionOutcome(phase, reason).status).toBe(expected);
  });

  it('summarizes tools as action, object, result, validation and artifacts', () => {
    addToolCall('write_file', '{"path":"result.txt"}', 'write');
    addToolResult('write_file', 'ok', 'write', 20, false);
    addToolCall('cargo_test', '{"command":"cargo test"}', 'verify');
    addToolResult('cargo_test', 'passed', 'verify', 40, false);
    const stack = state.activeExecutionStack as HTMLElement;

    expect(stack.querySelector('.tool-name')?.textContent).toBe('Change');
    expect(stack.querySelector('.tool-args-preview')?.textContent).toBe('result.txt');
    expect(stack.querySelector('.execution-stack-title')?.textContent).toContain(
      'Verify · cargo test',
    );
    expect(stack.querySelector('.execution-stack-meta')?.textContent).toContain('1 verification');
    expect(stack.querySelector('.execution-stack-meta')?.textContent).toContain(
      '1 artifact action',
    );
  });
});
