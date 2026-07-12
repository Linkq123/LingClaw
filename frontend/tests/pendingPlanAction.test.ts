import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  clearPendingPlanAction,
  confirmPendingPlanExecution,
  executePendingPlan,
  renderPendingPlanAction,
  restorePendingPlanAction,
} from '../src/renderers/pending-plan.js';
import { dom, state } from '../src/state.js';

function mountDom() {
  document.body.innerHTML = `
    <div id="chat">
      <div class="msg-row assistant">
        <div class="msg-content">
          <div class="msg assistant">Plan body</div>
          <div class="msg-time">12:00</div>
        </div>
      </div>
    </div>
    <textarea id="input"></textarea>
    <button id="stop"></button>
    <button id="send"></button>
    <span id="send-icon"></span>
  `;
  dom.chat = document.getElementById('chat') as HTMLElement;
  dom.input = document.getElementById('input') as HTMLTextAreaElement;
  dom.stopBtn = document.getElementById('stop') as HTMLButtonElement;
  dom.sendBtn = document.getElementById('send') as HTMLButtonElement;
  dom.sendIcon = document.getElementById('send-icon');
}

describe('pending plan action', () => {
  beforeEach(() => {
    mountDom();
    state.busy = false;
    state.pendingPlanId = '';
    state.pendingPlanMessageIndex = null;
    state.pendingPlanExecutionId = '';
    state.composerModelAvailability = 'ready';
    state.imageUploadInFlight = false;
    state.sessionSwitchInFlight = false;
    state.sessionIdentityMutationInFlight = false;
    state.composerSessionTransitionPending = false;
    state.composerSessionIdentityPending = false;
    state.ws = {
      readyState: WebSocket.OPEN,
      send: vi.fn(),
    } as unknown as WebSocket;
  });

  afterEach(() => {
    clearPendingPlanAction();
    state.ws = null;
    document.body.innerHTML = '';
    dom.chat = null;
    dom.input = null;
    dom.stopBtn = null;
    dom.sendBtn = null;
    dom.sendIcon = null;
  });

  it('renders one execute button for the latest assistant plan', () => {
    renderPendingPlanAction({
      plan_id: 'plan_123',
      message_index: 2,
      created_at: 1710000000,
    });
    renderPendingPlanAction({
      plan_id: 'plan_456',
      message_index: 4,
      created_at: 1710000100,
    });

    const buttons = document.querySelectorAll<HTMLButtonElement>('.plan-execute-btn');
    expect(buttons).toHaveLength(1);
    expect(buttons[0].textContent).toBe('开始执行');
    expect(buttons[0].dataset.planId).toBe('plan_456');
    expect(state.pendingPlanId).toBe('plan_456');
    expect(state.pendingPlanMessageIndex).toBe(4);
  });

  it('attaches the execute button to the matching assistant message index', () => {
    document.body.innerHTML = `
      <div id="chat">
        <div class="msg-row assistant" data-message-index="2">
          <div class="msg-content">
            <div class="msg assistant">Approved plan</div>
            <div class="msg-time">12:00</div>
          </div>
        </div>
        <div class="msg-row assistant" data-message-index="4">
          <div class="msg-content">
            <div class="msg assistant">Later response</div>
            <div class="msg-time">12:01</div>
          </div>
        </div>
      </div>
      <textarea id="input"></textarea>
      <button id="stop"></button>
      <button id="send"></button>
      <span id="send-icon"></span>
    `;
    dom.chat = document.getElementById('chat') as HTMLElement;
    dom.input = document.getElementById('input') as HTMLTextAreaElement;
    dom.stopBtn = document.getElementById('stop') as HTMLButtonElement;
    dom.sendBtn = document.getElementById('send') as HTMLButtonElement;
    dom.sendIcon = document.getElementById('send-icon');

    renderPendingPlanAction({
      plan_id: 'plan_123',
      message_index: 2,
      created_at: 1710000000,
    });

    const firstRow = document.querySelector<HTMLElement>('[data-message-index="2"]');
    const laterRow = document.querySelector<HTMLElement>('[data-message-index="4"]');
    expect(firstRow?.querySelector('.plan-execute-btn')).not.toBeNull();
    expect(laterRow?.querySelector('.plan-execute-btn')).toBeNull();
  });

  it('sends only execute_plan_id when clicked', () => {
    renderPendingPlanAction({
      plan_id: 'plan_123',
      message_index: 2,
      created_at: 1710000000,
    });

    const button = document.querySelector<HTMLButtonElement>('.plan-execute-btn');
    executePendingPlan(button);

    expect(state.ws?.send).toHaveBeenCalledWith(JSON.stringify({ execute_plan_id: 'plan_123' }));
    expect(state.ws?.send).toHaveBeenCalledTimes(1);
    expect(button?.disabled).toBe(true);
    expect(button?.textContent).toBe('执行中');
    expect(state.busy).toBe(true);
    expect(state.pendingPlanExecutionId).toBe('plan_123');
    expect(document.querySelector('.msg-row.user .msg')).toBeNull();
  });

  it('disables and blocks plan execution when the Agent model is unconfigured', () => {
    state.composerModelAvailability = 'agent-model-unconfigured';
    renderPendingPlanAction({
      plan_id: 'plan_123',
      message_index: 2,
      created_at: 1710000000,
    });

    const button = document.querySelector<HTMLButtonElement>('.plan-execute-btn');
    expect(button?.disabled).toBe(true);

    executePendingPlan(button);

    expect(state.ws?.send).not.toHaveBeenCalled();
    expect(state.busy).toBe(false);
    expect(state.pendingPlanExecutionId).toBe('');
  });

  it('disables and blocks plan execution while an attachment upload is pending', () => {
    state.imageUploadInFlight = true;
    renderPendingPlanAction({
      plan_id: 'plan_123',
      message_index: 2,
      created_at: 1710000000,
    });

    const button = document.querySelector<HTMLButtonElement>('.plan-execute-btn');
    expect(button?.disabled).toBe(true);

    executePendingPlan(button);

    expect(state.ws?.send).not.toHaveBeenCalled();
    expect(state.busy).toBe(false);
    expect(state.pendingPlanExecutionId).toBe('');
  });

  it('adds the execution transcript only after backend start confirms execution', () => {
    renderPendingPlanAction({
      plan_id: 'plan_123',
      message_index: 2,
      created_at: 1710000000,
    });

    const button = document.querySelector<HTMLButtonElement>('.plan-execute-btn');
    executePendingPlan(button);
    confirmPendingPlanExecution();

    expect(document.querySelector('.msg-row.user .msg')?.textContent).toBe(
      'Proceed with the approved plan.',
    );
    expect(state.pendingPlanExecutionId).toBe('');
  });

  it('restores the execute button if backend rejects execution', () => {
    renderPendingPlanAction({
      plan_id: 'plan_123',
      message_index: 2,
      created_at: 1710000000,
    });

    const button = document.querySelector<HTMLButtonElement>('.plan-execute-btn');
    executePendingPlan(button);
    restorePendingPlanAction();

    expect(button?.disabled).toBe(false);
    expect(button?.textContent).toBe('开始执行');
    expect(state.pendingPlanExecutionId).toBe('');
    expect(document.querySelector('.msg-row.user .msg')).toBeNull();
  });

  it('clears pending action state and DOM', () => {
    renderPendingPlanAction({
      plan_id: 'plan_123',
      message_index: 2,
      created_at: 1710000000,
    });

    clearPendingPlanAction();

    expect(document.querySelector('.plan-execute-action')).toBeNull();
    expect(state.pendingPlanId).toBe('');
    expect(state.pendingPlanMessageIndex).toBeNull();
    expect(state.pendingPlanExecutionId).toBe('');
  });
});
