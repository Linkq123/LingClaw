import { invalidateChatScrollCache, scrollDown } from '../scroll.js';
import { dom, state } from '../state.js';
import type { TaskPlanEvent, TaskPlanPayload } from '../types.js';
import { escHtml, hideWelcome } from '../utils.js';
import { animatePanelIn } from './timeline.js';
import { closeToolDrawer, syncToolDrawer } from './tools.js';
import { iconMarkup } from '../icons.js';
import { mountExecutionPanel, refreshExecutionStackForPanel } from './execution-stack.js';
import { tr } from '../i18n.js';

let activeTaskPlanPanel: HTMLElement | null = null;
let activeTaskPlanKey = '';

function taskPlanStatusText(status: string): string {
  switch (status) {
    case 'active':
      return tr('execution.planActive');
    case 'ready':
      return tr('execution.planReady');
    case 'complete':
      return tr('execution.completed');
    case 'stale':
      return tr('execution.planSuperseded');
    default:
      return status;
  }
}

function syncTaskPlanPresentation(panel: HTMLElement): void {
  const status = panel.dataset.taskPlanStatus || 'active';
  const localizedStatus = taskPlanStatusText(status);
  panel.dataset.toolName = tr('execution.taskPlan');
  panel.dataset.toolStatus = localizedStatus;
  try {
    const plan = JSON.parse(panel.dataset.taskPlanDetail || '') as TaskPlanPayload;
    panel.dataset.toolArgs = tr('execution.planContext', {
      round: panel.dataset.taskPlanRound || '',
      cycle: panel.dataset.taskPlanCycle || '',
      intent: plan.intent,
    });
    panel.dataset.toolResult = renderPlanDetail(plan, status);
  } catch {
    // Preserve the last valid inspector content if a legacy panel has no raw plan data.
  }
  const nameEl = panel.querySelector<HTMLElement>('.tool-name');
  const statusEl = panel.querySelector<HTMLElement>('.tool-status');
  if (nameEl) nameEl.textContent = tr('execution.taskPlan');
  if (statusEl) statusEl.textContent = localizedStatus;
  if (state.activeToolPanel === panel) syncToolDrawer(panel);
}

function taskPlanKey(round: number, cycle: number): string {
  return `${round}:${cycle}`;
}

function renderPlainSection(title: string, lines: string[]): string {
  const cleanLines = lines.map((line) => line.trim()).filter(Boolean);
  if (cleanLines.length === 0) return '';
  return [`${title}:`, ...cleanLines.map((line) => `- ${line}`)].join('\n');
}

function renderPlanDetail(plan: TaskPlanPayload, displayStatus = plan.status): string {
  const sections = [
    `${tr('execution.planGoal')}: ${plan.goal}`,
    `${tr('execution.planIntent')}: ${plan.intent}`,
    `${tr('execution.planStatus')}: ${taskPlanStatusText(displayStatus)}`,
    renderPlainSection(
      tr('execution.planSteps'),
      (plan.steps || []).map((step) => `[${step.status}] ${step.title}`),
    ),
    renderPlainSection(tr('execution.planOpenQuestions'), plan.openQuestions || []),
    renderPlainSection(
      tr('execution.planSuggestedTools'),
      (plan.suggestedTools || []).map((tool) =>
        [tool.name, tool.reason, tool.source ? `source=${tool.source}` : '']
          .filter(Boolean)
          .join(' - '),
      ),
    ),
    renderPlainSection(
      tr('execution.planSuggestedAgents'),
      (plan.suggestedAgents || []).map((agent) => `${agent.name} - ${agent.reason}`),
    ),
    renderPlainSection(
      tr('execution.planVerification'),
      (plan.verificationSuggestions || []).map(
        (item) => `${item.command} - ${item.reason} (${item.confidence}, ${item.when})`,
      ),
    ),
    renderPlainSection(tr('execution.planAcceptance'), plan.acceptanceCriteria || []),
  ].filter(Boolean);

  return sections.join('\n\n');
}

function updatePanel(panel: HTMLElement, event: TaskPlanEvent): void {
  panel.dataset.taskPlanPanel = 'true';
  panel.dataset.taskPlanRound = String(event.round);
  panel.dataset.taskPlanCycle = String(event.cycle);
  panel.dataset.taskPlanStatus = event.plan.status;
  panel.dataset.taskPlanDetail = JSON.stringify(event.plan);
  panel.dataset.toolId = taskPlanKey(event.round, event.cycle);
  panel.dataset.toolName = tr('execution.taskPlan');
  panel.dataset.toolArgs = tr('execution.planContext', {
    round: event.round,
    cycle: event.cycle,
    intent: event.plan.intent,
  });
  panel.dataset.toolResult = renderPlanDetail(event.plan);
  panel.dataset.toolHasResult = 'true';
  panel.dataset.toolStatus = taskPlanStatusText(event.plan.status || 'active');
  panel.className = `tool-panel tool-panel-ready task-plan-panel task-plan-${event.plan.status || 'active'}`;
  panel.innerHTML = `
    <button type="button" class="tool-header task-plan-header" data-action="open-tool-drawer" aria-haspopup="dialog">
      <span class="tool-icon task-plan-icon">${iconMarkup('task-plan')}</span>
      <span class="tool-name" data-i18n="execution.taskPlan">${tr('execution.taskPlan')}</span>
      <span class="tool-args-preview">${escHtml(event.plan.goal)}</span>
      <span class="tool-status">${escHtml(taskPlanStatusText(event.plan.status || 'active'))}</span>
    </button>
  `;
}

function markTaskPlanPanel(panel: HTMLElement, status: 'complete' | 'stale'): void {
  const currentStatus = panel.dataset.taskPlanStatus;
  if (!currentStatus || currentStatus === status) return;
  if (currentStatus === 'complete' || currentStatus === 'stale') return;

  panel.dataset.taskPlanStatus = status;
  panel.dataset.toolStatus = taskPlanStatusText(status);
  panel.classList.remove(`task-plan-${currentStatus}`);
  panel.classList.add(`task-plan-${status}`);
  if (status === 'complete') {
    panel.classList.add('tool-panel-ready');
  }
  const statusEl = panel.querySelector('.tool-status');
  if (statusEl) {
    statusEl.textContent = taskPlanStatusText(status);
  }
  syncTaskPlanPresentation(panel);
  refreshExecutionStackForPanel(panel);
}

export function applyTaskPlan(event: TaskPlanEvent): HTMLElement | null {
  if (!dom.chat || !event.plan) return null;
  const key = taskPlanKey(event.round, event.cycle);
  const shouldReuse = Boolean(
    activeTaskPlanPanel?.isConnected &&
    activeTaskPlanPanel.closest('.execution-stack') === state.activeExecutionStack,
  );
  const panel = shouldReuse ? activeTaskPlanPanel! : document.createElement('div');
  updatePanel(panel, event);
  activeTaskPlanKey = key;

  if (!shouldReuse) {
    activeTaskPlanPanel = panel;
    const currentRow = document.querySelector('.msg-row.assistant.typing')?.closest('.msg-row');
    mountExecutionPanel(panel, 'task-plan', currentRow);
    animatePanelIn(panel);
    hideWelcome();
  }

  invalidateChatScrollCache();
  scrollDown();
  return panel;
}

export function finishTaskPlanPanel(): void {
  if (!activeTaskPlanPanel?.isConnected) return;
  if (state.activeToolPanel === activeTaskPlanPanel) {
    closeToolDrawer();
  }
  markTaskPlanPanel(activeTaskPlanPanel, 'complete');
  resetTaskPlanPanel();
  invalidateChatScrollCache();
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
}

export function resetTaskPlanPanel(): void {
  activeTaskPlanPanel = null;
  activeTaskPlanKey = '';
}

export function refreshTaskPlanPanelsLanguage(): void {
  document
    .querySelectorAll<HTMLElement>('[data-task-plan-panel="true"]')
    .forEach(syncTaskPlanPresentation);
}
