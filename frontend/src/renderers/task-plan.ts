import { invalidateChatScrollCache, scrollDown } from '../scroll.js';
import { dom } from '../state.js';
import type { TaskPlanEvent, TaskPlanPayload } from '../types.js';
import { escHtml, hideWelcome } from '../utils.js';
import { animatePanelIn, wrapInTimeline } from './timeline.js';

let activeTaskPlanPanel: HTMLElement | null = null;
let activeTaskPlanKey = '';

function taskPlanKey(round: number, cycle: number): string {
  return `${round}:${cycle}`;
}

function renderList<T>(
  items: T[] | undefined,
  className: string,
  render: (item: T) => string,
): string {
  if (!items || items.length === 0) return '';
  return `<ul class="${className}">${items.map((item) => `<li>${render(item)}</li>`).join('')}</ul>`;
}

function renderPlanBody(plan: TaskPlanPayload): string {
  const steps = renderList(
    plan.steps,
    'task-plan-list task-plan-steps',
    (step) =>
      `<span class="task-plan-status task-plan-status-${escHtml(step.status)}">${escHtml(step.status)}</span><span>${escHtml(step.title)}</span>`,
  );
  const tools = renderList(
    plan.suggestedTools,
    'task-plan-list',
    (tool) =>
      `<code>${escHtml(tool.name)}</code><span>${escHtml(tool.reason)}</span>${tool.source ? `<span class="task-plan-muted">${escHtml(tool.source)}</span>` : ''}`,
  );
  const agents = renderList(
    plan.suggestedAgents,
    'task-plan-list',
    (agent) => `<code>${escHtml(agent.name)}</code><span>${escHtml(agent.reason)}</span>`,
  );
  const verification = renderList(
    plan.verificationSuggestions,
    'task-plan-list',
    (item) =>
      `<code>${escHtml(item.command)}</code><span>${escHtml(item.reason)}</span><span class="task-plan-muted">${escHtml(item.confidence)} · ${escHtml(item.when)}</span>`,
  );
  const criteria = renderList(
    plan.acceptanceCriteria,
    'task-plan-list',
    (item) => `<span>${escHtml(item)}</span>`,
  );
  const questions = renderList(
    plan.openQuestions,
    'task-plan-list',
    (item) => `<span>${escHtml(item)}</span>`,
  );

  return `
    ${steps ? `<section><h4>Steps</h4>${steps}</section>` : ''}
    ${questions ? `<section><h4>Open Questions</h4>${questions}</section>` : ''}
    ${tools ? `<section><h4>Suggested Tools</h4>${tools}</section>` : ''}
    ${agents ? `<section><h4>Suggested Agents</h4>${agents}</section>` : ''}
    ${verification ? `<section><h4>Verification Suggestions</h4>${verification}</section>` : ''}
    ${criteria ? `<section><h4>Acceptance Criteria</h4>${criteria}</section>` : ''}
  `;
}

function updatePanel(panel: HTMLElement, event: TaskPlanEvent): void {
  panel.dataset.taskPlanPanel = 'true';
  panel.dataset.taskPlanRound = String(event.round);
  panel.dataset.taskPlanCycle = String(event.cycle);
  panel.dataset.taskPlanStatus = event.plan.status;
  panel.className = `task-plan-panel task-plan-${event.plan.status || 'active'}`;
  panel.innerHTML = `
    <div class="task-plan-header">
      <span class="task-plan-tag">Task Plan</span>
      <span class="task-plan-meta">round ${event.round} · cycle ${event.cycle}</span>
      <span class="task-plan-meta">${escHtml(event.plan.intent)} · ${escHtml(event.plan.status)}</span>
    </div>
    <div class="task-plan-goal">${escHtml(event.plan.goal)}</div>
    <div class="task-plan-body">${renderPlanBody(event.plan)}</div>
  `;
}

function markTaskPlanPanel(panel: HTMLElement, status: 'complete' | 'stale'): void {
  const currentStatus = panel.dataset.taskPlanStatus;
  if (!currentStatus || currentStatus === status) return;
  if (currentStatus === 'complete' || currentStatus === 'stale') return;

  panel.dataset.taskPlanStatus = status;
  panel.classList.remove(`task-plan-${currentStatus}`);
  panel.classList.add(`task-plan-${status}`);
  const meta = panel.querySelector('.task-plan-header');
  if (meta && !meta.textContent?.includes(status)) {
    const marker = document.createElement('span');
    marker.className = 'task-plan-meta';
    marker.textContent = status;
    meta.appendChild(marker);
  }
}

export function applyTaskPlan(event: TaskPlanEvent): HTMLElement | null {
  if (!dom.chat || !event.plan) return null;
  const key = taskPlanKey(event.round, event.cycle);
  const shouldReuse = activeTaskPlanPanel?.isConnected && activeTaskPlanKey === key;
  const panel = shouldReuse ? activeTaskPlanPanel! : document.createElement('div');
  updatePanel(panel, event);

  if (!shouldReuse) {
    activeTaskPlanPanel = panel;
    activeTaskPlanKey = key;
    const currentRow = document.querySelector('.msg-row.assistant.typing')?.closest('.msg-row');
    const wrapper = wrapInTimeline(panel, 'task-plan');
    if (currentRow && currentRow.parentElement === dom.chat) {
      dom.chat.insertBefore(wrapper, currentRow);
    } else {
      dom.chat.appendChild(wrapper);
    }
    animatePanelIn(panel);
    hideWelcome();
  }

  invalidateChatScrollCache();
  scrollDown();
  return panel;
}

export function finishTaskPlanPanel(): void {
  if (!activeTaskPlanPanel?.isConnected) return;
  markTaskPlanPanel(activeTaskPlanPanel, 'complete');
}

export function supersedeTaskPlanPanel(nextRound?: number, nextCycle?: number): void {
  const nextKey =
    typeof nextRound === 'number' && typeof nextCycle === 'number'
      ? taskPlanKey(nextRound, nextCycle)
      : '';
  if (activeTaskPlanPanel?.isConnected && nextKey && activeTaskPlanKey === nextKey) {
    return;
  }
  if (activeTaskPlanPanel?.isConnected) {
    markTaskPlanPanel(activeTaskPlanPanel, 'stale');
  }
  resetTaskPlanPanel();
}

export function resetTaskPlanPanel(): void {
  activeTaskPlanPanel = null;
  activeTaskPlanKey = '';
}
