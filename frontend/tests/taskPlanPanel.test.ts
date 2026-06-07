import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import {
  applyTaskPlan,
  finishTaskPlanPanel,
  resetTaskPlanPanel,
  supersedeTaskPlanPanel,
} from '../src/renderers/task-plan.js';
import { dom, state } from '../src/state.js';
import type { TaskPlanEvent } from '../src/types.js';

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
    resetTaskPlanPanel();
  });

  afterEach(() => {
    resetTaskPlanPanel();
    state.activeToolPanel = null;
    document.body.innerHTML = '';
    dom.chat = null;
    dom.toolDrawer = null;
    dom.toolDrawerBackdrop = null;
  });

  it('creates a timeline panel for task_plan events', () => {
    applyTaskPlan(sampleTaskPlan());

    const panel = document.querySelector('[data-task-plan-panel="true"]');
    expect(panel).not.toBeNull();
    expect(panel?.closest('.timeline-node--task-plan')).not.toBeNull();
    expect(panel?.classList.contains('tool-panel')).toBe(true);
    expect(panel?.textContent).toContain('Fix MCP timeout handling');
    expect(panel?.textContent).not.toContain('cargo test mcp');
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

  it('removes active task plans on done', () => {
    applyTaskPlan(sampleTaskPlan());
    finishTaskPlanPanel();

    expect(document.querySelector('[data-task-plan-panel="true"]')).toBeNull();
    expect(document.querySelector('.timeline-node--task-plan')).toBeNull();
  });

  it('removes ready task plans on done', () => {
    applyTaskPlan(
      sampleTaskPlan({
        plan: {
          ...sampleTaskPlan().plan,
          status: 'ready',
        },
      }),
    );
    finishTaskPlanPanel();

    expect(document.querySelector('[data-task-plan-panel="true"]')).toBeNull();
    expect(document.querySelector('.timeline-node--task-plan')).toBeNull();
  });

  it('closes the tool drawer when the removed task plan is active', () => {
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
    expect(document.querySelector('[data-task-plan-panel="true"]')).toBeNull();
  });

  it('marks the previous cycle stale before accepting the next plan', () => {
    applyTaskPlan(sampleTaskPlan({ round: 1, cycle: 0 }));
    supersedeTaskPlanPanel(1, 1);
    applyTaskPlan(sampleTaskPlan({ round: 1, cycle: 1 }));

    const panels = Array.from(
      document.querySelectorAll('[data-task-plan-panel="true"]'),
    ) as HTMLElement[];
    expect(panels).toHaveLength(2);
    expect(panels[0].dataset.taskPlanStatus).toBe('stale');
    expect(panels[0].dataset.toolStatus).toBe('stale');
    expect(panels[0].classList.contains('task-plan-stale')).toBe(true);
    expect(panels[0].textContent).toContain('stale');
    expect(panels[1].dataset.taskPlanCycle).toBe('1');
    expect(panels[1].dataset.taskPlanStatus).toBe('active');
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
    expect(panel.querySelector('button')).toBeNull();
    expect(panel.querySelector('[data-command]')).toBeNull();
  });
});
