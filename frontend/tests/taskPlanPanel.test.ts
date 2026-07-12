import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import {
  applyTaskPlan,
  finishTaskPlanPanel,
  refreshTaskPlanPanelsLanguage,
  resetTaskPlanPanel,
  supersedeTaskPlanPanel,
} from '../src/renderers/task-plan.js';
import { dom, state } from '../src/state.js';
import type { TaskPlanEvent } from '../src/types.js';
import { setLanguage } from '../src/i18n.js';

function sampleTaskPlan(overrides: Partial<TaskPlanEvent> = {}): TaskPlanEvent {
  return {
    type: 'task_plan',
    round: 1,
    cycle: 0,
    plan: {
      goal: 'Fix MCP timeout handling',
      intent: 'change',
      steps: [{ id: 'inspect', title: 'Inspect relevant code', status: 'pending' }],
      openQuestions: [],
      suggestedTools: [
        {
          name: 'read_file',
          reason: 'Inspect the current implementation',
          score: 5,
          source: 'intent',
        },
      ],
      suggestedAgents: [],
      verificationSuggestions: [
        {
          command: 'cargo test mcp',
          reason: 'MCP behavior appears relevant',
          confidence: 'high',
          when: 'before_finish',
        },
      ],
      acceptanceCriteria: ['Relevant tests pass'],
      status: 'active',
    },
    ...overrides,
  };
}

describe('task plan timeline panel', () => {
  beforeEach(() => {
    document.body.innerHTML = `
      <div id="chat"></div>
      <div id="tool-drawer" class="tool-drawer"></div>
      <div id="tool-drawer-backdrop" class="tool-drawer-backdrop"></div>
    `;
    dom.chat = document.getElementById('chat') as HTMLElement;
    dom.toolDrawer = document.getElementById('tool-drawer');
    dom.toolDrawerBackdrop = document.getElementById('tool-drawer-backdrop');
    state.autoFollowChat = false;
    state.bulkRenderingChat = false;
    state.activeToolPanel = null;
    setLanguage('en');
    resetTaskPlanPanel();
  });

  afterEach(() => {
    resetTaskPlanPanel();
    state.activeToolPanel = null;
    setLanguage('en');
    document.body.innerHTML = '';
    dom.chat = null;
    dom.toolDrawer = null;
    dom.toolDrawerBackdrop = null;
  });

  it('creates an execution-stack step for task_plan events', () => {
    applyTaskPlan(sampleTaskPlan());

    const panel = document.querySelector('[data-task-plan-panel="true"]');
    expect(panel).not.toBeNull();
    expect(panel?.closest('.execution-step--task-plan')).not.toBeNull();
    expect(panel?.classList.contains('tool-panel')).toBe(true);
    expect(panel?.textContent).toContain('Fix MCP timeout handling');
    expect(panel?.textContent).not.toContain('cargo test mcp');
    expect(panel?.querySelector('.task-plan-icon use')?.getAttribute('href')).toBe(
      '#icon-task-plan',
    );
    expect((panel as HTMLElement | null)?.dataset.toolResult).toContain('cargo test mcp');
    expect(panel?.querySelector('.task-plan-body')).toBeNull();
  });

  it('updates the same round and cycle without appending a duplicate panel', () => {
    applyTaskPlan(sampleTaskPlan());
    applyTaskPlan(
      sampleTaskPlan({
        plan: {
          ...sampleTaskPlan().plan,
          goal: 'Updated plan goal',
          steps: [{ id: 'inspect', title: 'Inspect changed files', status: 'done' }],
        },
      }),
    );

    const panels = document.querySelectorAll('[data-task-plan-panel="true"]');
    expect(panels).toHaveLength(1);
    expect(panels[0].textContent).toContain('Updated plan goal');
    expect((panels[0] as HTMLElement).dataset.toolResult).toContain('Inspect changed files');
  });

  it('renders replayed task plans as the current panel', () => {
    applyTaskPlan(sampleTaskPlan({ round: 2, cycle: 1 }));

    const panel = document.querySelector('[data-task-plan-panel="true"]') as HTMLElement;
    expect(panel?.dataset.taskPlanRound).toBe('2');
    expect(panel?.dataset.taskPlanCycle).toBe('1');
    expect(panel?.dataset.toolArgs).toContain('round 2');
    expect(panel?.dataset.toolArgs).toContain('cycle 1');
  });

  it('keeps and completes active task plans on done', () => {
    applyTaskPlan(sampleTaskPlan());
    finishTaskPlanPanel();

    const panel = document.querySelector('[data-task-plan-panel="true"]') as HTMLElement;
    expect(panel.dataset.taskPlanStatus).toBe('complete');
    expect(panel.dataset.toolStatus).toBe('Completed');
  });

  it('refreshes the task-plan label and status when language changes', () => {
    applyTaskPlan(sampleTaskPlan());
    const panel = document.querySelector('[data-task-plan-panel="true"]') as HTMLElement;

    setLanguage('zh-CN');
    refreshTaskPlanPanelsLanguage();

    expect(panel.querySelector('.tool-name')?.textContent).toBe('任务计划');
    expect(panel.querySelector('.tool-status')?.textContent).toBe('进行中');
    expect(panel.dataset.toolStatus).toBe('进行中');
    expect(panel.dataset.toolArgs).toBe('第 1 轮，第 0 周期，意图 change');
    expect(panel.dataset.toolResult).toContain('目标: Fix MCP timeout handling');
    expect(panel.dataset.toolResult).toContain('验证建议:');
    expect(panel.dataset.toolResult).toContain('cargo test mcp');

    finishTaskPlanPanel();
    expect(panel.querySelector('.tool-status')?.textContent).toBe('已完成');
    expect(panel.dataset.toolResult).toContain('状态: 已完成');
  });

  it('keeps and completes ready task plans on done', () => {
    applyTaskPlan(
      sampleTaskPlan({
        plan: {
          ...sampleTaskPlan().plan,
          status: 'ready',
        },
      }),
    );
    finishTaskPlanPanel();

    const panel = document.querySelector('[data-task-plan-panel="true"]') as HTMLElement;
    expect(panel.dataset.taskPlanStatus).toBe('complete');
  });

  it('closes the tool drawer when the task plan is completed', () => {
    applyTaskPlan(sampleTaskPlan());
    const panel = document.querySelector('[data-task-plan-panel="true"]') as HTMLElement;
    panel.classList.add('tool-panel-active');
    state.activeToolPanel = panel;
    dom.toolDrawer?.classList.add('open');
    dom.toolDrawerBackdrop?.classList.add('open');
    dom.toolDrawer?.setAttribute('aria-hidden', 'false');

    finishTaskPlanPanel();

    expect(state.activeToolPanel).toBeNull();
    expect(dom.toolDrawer?.classList.contains('open')).toBe(false);
    expect(dom.toolDrawerBackdrop?.classList.contains('open')).toBe(false);
    expect(dom.toolDrawer?.getAttribute('aria-hidden')).toBe('true');
    expect(
      (document.querySelector('[data-task-plan-panel="true"]') as HTMLElement).dataset
        .taskPlanStatus,
    ).toBe('complete');
  });

  it('reuses the task-plan step for the next cycle', () => {
    applyTaskPlan(sampleTaskPlan({ round: 1, cycle: 0 }));
    supersedeTaskPlanPanel(1, 1);
    applyTaskPlan(sampleTaskPlan({ round: 1, cycle: 1 }));

    const panels = Array.from(
      document.querySelectorAll('[data-task-plan-panel="true"]'),
    ) as HTMLElement[];
    expect(panels).toHaveLength(1);
    expect(panels[0].dataset.taskPlanCycle).toBe('1');
    expect(panels[0].dataset.taskPlanStatus).toBe('active');
    expect(panels[0].classList.contains('task-plan-stale')).toBe(false);
  });

  it('keeps the active panel when replaying the same round and cycle', () => {
    applyTaskPlan(sampleTaskPlan({ round: 1, cycle: 0 }));
    supersedeTaskPlanPanel(1, 0);
    applyTaskPlan(
      sampleTaskPlan({
        round: 1,
        cycle: 0,
        plan: {
          ...sampleTaskPlan().plan,
          goal: 'Replayed plan goal',
        },
      }),
    );

    const panels = Array.from(
      document.querySelectorAll('[data-task-plan-panel="true"]'),
    ) as HTMLElement[];
    expect(panels).toHaveLength(1);
    expect(panels[0].dataset.taskPlanStatus).toBe('active');
    expect(panels[0].classList.contains('task-plan-stale')).toBe(false);
    expect(panels[0].textContent).toContain('Replayed plan goal');
  });

  it('shows verification suggestions as passive text only', () => {
    applyTaskPlan(sampleTaskPlan());

    const panel = document.querySelector('[data-task-plan-panel="true"]') as HTMLElement;
    expect(panel.textContent).not.toContain('cargo test mcp');
    expect(panel.dataset.toolResult).toContain('cargo test mcp');
    expect(panel.querySelector('button[data-action="open-tool-drawer"]')).not.toBeNull();
    expect(panel.querySelector('[data-command]')).toBeNull();
  });
});
