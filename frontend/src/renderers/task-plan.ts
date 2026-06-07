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

function renderPlainSection(title: string, lines: string[]): string {
  const cleanLines = lines.map((line) => line.trim()).filter(Boolean);
  if (cleanLines.length === 0) return '';
  return [`${title}:`, ...cleanLines.map((line) => `- ${line}`)].join('\n');
}

function renderPlanDetail(plan: TaskPlanPayload): string {
  const sections = [
    `Goal: ${plan.goal}`,
    `Intent: ${plan.intent}`,
    `Status: ${plan.status}`,
    renderPlainSection(
      'Steps',
      (plan.steps || []).map((step) => `[${step.status}] ${step.title}`),
    ),
    renderPlainSection('Open Questions', plan.openQuestions || []),
    renderPlainSection(
      'Suggested Tools',
      (plan.suggestedTools || []).map((tool) =>
        [tool.name, tool.reason, tool.source ? `source=${tool.source}` : '']
          .filter(Boolean)
          .join(' - '),
      ),
    ),
    renderPlainSection(
      'Suggested Agents',
      (plan.suggestedAgents || []).map((agent) => `${agent.name} - ${agent.reason}`),
    ),
    renderPlainSection(
      'Verification Suggestions',
      (plan.verificationSuggestions || []).map(
        (item) => `${item.command} - ${item.reason} (${item.confidence}, ${item.when})`,
      ),
    ),
    renderPlainSection('Acceptance Criteria', plan.acceptanceCriteria || []),
  ].filter(Boolean);

  return sections.join('\n\n');
}

function updatePanel(panel: HTMLElement, event: TaskPlanEvent): void {
  panel.dataset.taskPlanPanel = 'true';
  panel.dataset.taskPlanRound = String(event.round);
  panel.dataset.taskPlanCycle = String(event.cycle);
  panel.dataset.taskPlanStatus = event.plan.status;
  panel.dataset.toolId = taskPlanKey(event.round, event.cycle);
  panel.dataset.toolName = 'Task Plan';
  panel.dataset.toolArgs = `round ${event.round}, cycle ${event.cycle}, intent ${event.plan.intent}`;
  panel.dataset.toolResult = renderPlanDetail(event.plan);
  panel.dataset.toolHasResult = 'true';
  panel.dataset.toolStatus = event.plan.status || 'active';
  panel.className = `tool-panel tool-panel-ready task-plan-panel task-plan-${event.plan.status || 'active'}`;
  panel.innerHTML = `
    <div class="tool-header task-plan-header" data-action="open-tool-drawer">
      <span class="tool-icon task-plan-icon">▣</span>
      <span class="tool-name">Task Plan</span>
      <span class="tool-args-preview">${escHtml(event.plan.goal)}</span>
      <span class="tool-status">${escHtml(event.plan.status || 'active')}</span>
    </div>
  `;
}

function markTaskPlanPanel(panel: HTMLElement, status: 'complete' | 'stale'): void {
  const currentStatus = panel.dataset.taskPlanStatus;
  if (!currentStatus || currentStatus === status) return;
  if (currentStatus === 'complete' || currentStatus === 'stale') return;

  panel.dataset.taskPlanStatus = status;
  panel.dataset.toolStatus = status;
  panel.classList.remove(`task-plan-${currentStatus}`);
  panel.classList.add(`task-plan-${status}`);
  if (status === 'complete') {
    panel.classList.add('tool-panel-ready');
  }
  const statusEl = panel.querySelector('.tool-status');
  if (statusEl) {
    statusEl.textContent = status;
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
