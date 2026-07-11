import { dom, state } from '../state.js';
import type { PlanReadyPayload } from '../types.js';
import { invalidateChatScrollCache } from '../scroll.js';
import { addMsg, setBusy } from './chat.js';

const APPROVED_PLAN_EXECUTION_PREFIX = 'Proceed with the approved plan.';

export function clearPendingPlanAction(): void {
  state.pendingPlanId = '';
  state.pendingPlanMessageIndex = null;
  state.pendingPlanExecutionId = '';
  document.querySelectorAll('.plan-execute-action').forEach((node) => node.remove());
}

export function restorePendingPlanAction(): void {
  if (!state.pendingPlanExecutionId) return;
  document.querySelectorAll<HTMLButtonElement>('.plan-execute-btn').forEach((button) => {
    button.disabled = false;
    button.textContent = '开始执行';
  });
  state.pendingPlanExecutionId = '';
}

export function confirmPendingPlanExecution(): void {
  if (!state.pendingPlanExecutionId) return;
  addMsg('user', APPROVED_PLAN_EXECUTION_PREFIX, undefined, { trackUnread: false });
  state.pendingPlanExecutionId = '';
}

function latestAssistantContent(): Element | null {
  if (!dom.chat) return null;
  const rows = Array.from(dom.chat.querySelectorAll('.msg-row.assistant'));
  for (let idx = rows.length - 1; idx >= 0; idx -= 1) {
    const content = rows[idx].querySelector('.msg-content');
    if (content) return content;
  }
  return null;
}

function assistantContentForPlan(messageIndex: number | null): Element | null {
  if (!dom.chat) return null;
  if (typeof messageIndex === 'number') {
    const row = dom.chat.querySelector(`.msg-row.assistant[data-message-index="${messageIndex}"]`);
    const content = row?.querySelector('.msg-content');
    if (content) return content;
  }

  const content = latestAssistantContent();
  const row = content?.closest('.msg-row.assistant') as HTMLElement | null;
  if (row && typeof messageIndex === 'number') {
    row.dataset.messageIndex = String(messageIndex);
  }
  return content;
}

export function renderPendingPlanAction(plan: PlanReadyPayload | null | undefined): void {
  if (!plan?.plan_id) return;
  state.pendingPlanId = plan.plan_id;
  state.pendingPlanMessageIndex =
    typeof plan.message_index === 'number' ? plan.message_index : null;
  document.querySelectorAll('.plan-execute-action').forEach((node) => node.remove());
  const content = assistantContentForPlan(state.pendingPlanMessageIndex);
  if (!content) return;

  const action = document.createElement('div');
  action.className = 'plan-execute-action';
  const button = document.createElement('button');
  button.type = 'button';
  button.className = 'plan-execute-btn';
  button.dataset.action = 'execute-plan';
  button.dataset.planId = plan.plan_id;
  button.textContent = '开始执行';
  action.appendChild(button);
  const time = content.querySelector('.msg-time');
  if (time) {
    content.insertBefore(action, time);
  } else {
    content.appendChild(action);
  }
  invalidateChatScrollCache();
}

export function executePendingPlan(button: HTMLButtonElement | null | undefined): void {
  const planId = button?.dataset?.planId || state.pendingPlanId;
  if (!planId || state.busy || !state.ws || state.ws.readyState !== WebSocket.OPEN) return;
  if (button) {
    button.disabled = true;
    button.textContent = '执行中';
  }
  state.pendingPlanExecutionId = planId;
  state.ws.send(JSON.stringify({ execute_plan_id: planId }));
  setBusy(true);
}
