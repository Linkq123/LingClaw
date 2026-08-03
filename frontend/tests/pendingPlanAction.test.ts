import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  clearPlanStateForSessionTransition,
  clearPendingPlanAction,
  confirmPendingPlanExecution,
  copyPlan,
  discardPlan,
  executePendingPlan,
  executeStalePlan,
  handlePlanRevisionConflict,
  handlePlanStale,
  jumpToPlan,
  renderPendingPlanAction,
  renderPlanHistory,
  renderPlanState,
  refreshPlanLanguage,
  refreshPlanMounts,
  resumePlan,
  restorePendingPlanAction,
  submitPlanFeedback,
  togglePlanFeedback,
} from '../src/renderers/pending-plan.js';
import { syncComposerAvailability } from '../src/composerAvailability.js';
import { setLanguage } from '../src/i18n.js';
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
    <button id="execute-mode-toggle"></button>
    <button id="plan-mode-toggle"></button>
    <div id="plan-progress" hidden></div>
  `;
  dom.chat = document.getElementById('chat') as HTMLElement;
  dom.input = document.getElementById('input') as HTMLTextAreaElement;
  dom.stopBtn = document.getElementById('stop') as HTMLButtonElement;
  dom.sendBtn = document.getElementById('send') as HTMLButtonElement;
  dom.sendIcon = document.getElementById('send-icon');
  dom.executeModeToggle = document.getElementById('execute-mode-toggle') as HTMLButtonElement;
  dom.planModeToggle = document.getElementById('plan-mode-toggle') as HTMLButtonElement;
  dom.planProgress = document.getElementById('plan-progress') as HTMLElement;
}

describe('pending plan action', () => {
  beforeEach(() => {
    setLanguage('en');
    mountDom();
    state.busy = false;
    state.pendingPlanId = '';
    state.pendingPlanMessageIndex = null;
    state.pendingPlanExecutionId = '';
    state.activeSessionId = 'main';
    state.activeGroupId = '';
    state.storageMode = 'healthy';
    state.planHistory = [];
    state.deferredHistory = [];
    state.planModesBySession.clear();
    state.composerModelAvailability = 'ready';
    state.imageUploadInFlight = false;
    state.sessionSwitchInFlight = false;
    state.sessionIdentityMutationInFlight = false;
    state.composerSessionTransitionPending = false;
    state.composerSessionIdentityPending = false;
    state.currentMsg = null;
    state.pendingAssistantText = '';
    state.ws = {
      readyState: WebSocket.OPEN,
      send: vi.fn(),
    } as unknown as WebSocket;
  });

  afterEach(() => {
    setLanguage('en');
    clearPendingPlanAction();
    state.storageMode = 'healthy';
    state.deferredHistory = [];
    state.currentMsg = null;
    state.pendingAssistantText = '';
    state.ws = null;
    document.body.innerHTML = '';
    dom.chat = null;
    dom.input = null;
    dom.stopBtn = null;
    dom.sendBtn = null;
    dom.sendIcon = null;
    dom.executeModeToggle = null;
    dom.planModeToggle = null;
    dom.planProgress = null;
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

    const buttons = document.querySelectorAll<HTMLButtonElement>('[data-action="execute-plan"]');
    expect(buttons).toHaveLength(1);
    expect(buttons[0].textContent).toBe('Execute plan');
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
    expect(firstRow?.querySelector('[data-action="execute-plan"]')).not.toBeNull();
    expect(laterRow?.querySelector('[data-action="execute-plan"]')).toBeNull();
  });

  it('sends the revision-aware execute action when clicked', () => {
    renderPendingPlanAction({
      plan_id: 'plan_123',
      message_index: 2,
      created_at: 1710000000,
    });

    const button = document.querySelector<HTMLButtonElement>('[data-action="execute-plan"]');
    executePendingPlan(button);

    expect(state.ws?.send).toHaveBeenCalledWith(
      JSON.stringify({
        plan_action: { action: 'execute', plan_id: 'plan_123', revision: 1 },
      }),
    );
    expect(state.ws?.send).toHaveBeenCalledTimes(1);
    expect(button?.disabled).toBe(true);
    expect(state.busy).toBe(true);
    expect(state.pendingPlanExecutionId).toBe('plan_123');
    expect(document.querySelector('.msg-row.user .msg')).toBeNull();
  });

  it('resynchronizes a stale plan before the next action', () => {
    renderPlanState({
      plan_id: 'plan_revision',
      revision: 2,
      status: 'ready',
      message_index: 2,
      created_at: 10,
      updated_at: 20,
      artifact: {
        title: 'Old revision',
        goal: 'Old goal',
        steps: [{ id: 'old', title: 'Old step' }],
      },
      progress: [{ id: 'old', title: 'Old step', status: 'pending' }],
    });

    handlePlanRevisionConflict({
      plan_id: 'plan_revision',
      revision: 3,
      status: 'ready',
      message_index: 2,
      created_at: 10,
      updated_at: 30,
      artifact: {
        title: 'Current revision',
        goal: 'Current goal',
        steps: [{ id: 'current', title: 'Current step' }],
      },
      progress: [{ id: 'current', title: 'Current step', status: 'pending' }],
    });

    expect(state.activePlan?.revision).toBe(3);
    const button = document.querySelector<HTMLButtonElement>('[data-action="execute-plan"]');
    executePendingPlan(button);
    expect(state.ws?.send).toHaveBeenCalledWith(
      JSON.stringify({
        plan_action: { action: 'execute', plan_id: 'plan_revision', revision: 3 },
      }),
    );
  });

  it('disables and blocks plan execution when the Agent model is unconfigured', () => {
    state.composerModelAvailability = 'agent-model-unconfigured';
    renderPendingPlanAction({
      plan_id: 'plan_123',
      message_index: 2,
      created_at: 1710000000,
    });

    const button = document.querySelector<HTMLButtonElement>('[data-action="execute-plan"]');
    expect(button?.disabled).toBe(true);

    executePendingPlan(button);

    expect(state.ws?.send).not.toHaveBeenCalled();
    expect(state.busy).toBe(false);
    expect(state.pendingPlanExecutionId).toBe('');
  });

  it('keeps plan execution controls synchronized with Composer availability', () => {
    renderPendingPlanAction({
      plan_id: 'plan_dynamic_availability',
      message_index: 2,
      created_at: 1710000000,
    });

    const button = document.querySelector<HTMLButtonElement>('[data-action="execute-plan"]');
    expect(button?.dataset.planRequiresModel).toBe('true');
    expect(button?.disabled).toBe(false);

    state.composerModelAvailability = 'agent-model-unconfigured';
    syncComposerAvailability();
    expect(button?.disabled).toBe(true);
    expect(button?.getAttribute('aria-describedby')).toBe('composer-availability-detail');

    state.composerModelAvailability = 'ready';
    syncComposerAvailability();
    expect(button?.disabled).toBe(false);
    expect(button?.hasAttribute('aria-describedby')).toBe(false);
  });

  it('disables resume and stale execution controls when the model becomes unavailable', () => {
    renderPlanState({
      plan_id: 'plan_resume_availability',
      revision: 3,
      status: 'stopped',
      message_index: 2,
      created_at: 1710000000,
      updated_at: 1710000001,
      approved_at: 1710000000,
      execution_attempt: 1,
      artifact: {
        title: 'Resume safely',
        goal: 'Finish the approved work',
        steps: [{ id: 'finish', title: 'Finish' }],
      },
      progress: [{ id: 'finish', title: 'Finish', status: 'pending' }],
    });
    handlePlanStale({
      plan_id: 'plan_resume_availability',
      revision: 3,
      paths: ['src/main.rs'],
    });

    state.composerModelAvailability = 'agent-model-unconfigured';
    syncComposerAvailability();

    expect(document.querySelector<HTMLButtonElement>('[data-action="plan-resume"]')?.disabled).toBe(
      true,
    );
    expect(
      document.querySelector<HTMLButtonElement>('[data-action="plan-execute-stale"]')?.disabled,
    ).toBe(true);
  });

  it('does not send a stale plan action after switching Sessions', () => {
    state.activeSessionId = 'session-alpha';
    renderPendingPlanAction({
      plan_id: 'plan_alpha',
      message_index: 2,
      created_at: 1710000000,
    });

    state.activeSessionId = 'session-beta';
    syncComposerAvailability();
    expect(
      document.querySelector<HTMLButtonElement>('[data-action="plan-discard"]')?.disabled,
    ).toBe(true);

    discardPlan();

    expect(state.ws?.send).not.toHaveBeenCalled();
  });

  it('clears the previous Session plan lock before the next Session history arrives', () => {
    state.activeSessionId = 'session-alpha';
    renderPlanState({
      plan_id: 'plan_alpha_lock',
      revision: 1,
      status: 'ready',
      message_index: 2,
      created_at: 1710000000,
      updated_at: 1710000001,
      artifact: {
        title: 'Alpha plan',
        goal: 'Keep the lock scoped to Session alpha',
        steps: [{ id: 'finish', title: 'Finish alpha' }],
      },
      progress: [{ id: 'finish', title: 'Finish alpha', status: 'pending' }],
    });

    expect(dom.executeModeToggle?.disabled).toBe(true);

    clearPlanStateForSessionTransition('session-beta');

    expect(state.activePlan).toBeNull();
    expect(state.pendingPlanId).toBe('');
    expect(dom.executeModeToggle?.disabled).toBe(false);
    expect(dom.executeModeToggle?.title).toBe('');
    expect(document.querySelector('.plan-artifact-card')).toBeNull();
  });

  it('disables and blocks durable plan actions while storage is protected', () => {
    state.storageMode = 'protected';
    renderPendingPlanAction({
      plan_id: 'plan_protected',
      message_index: 2,
      created_at: 1710000000,
    });

    const execute = document.querySelector<HTMLButtonElement>('[data-action="execute-plan"]');
    const discard = document.querySelector<HTMLButtonElement>('[data-action="plan-discard"]');
    const revise = document.querySelector<HTMLButtonElement>(
      '[data-action="plan-toggle-feedback"]',
    );
    const copy = document.querySelector<HTMLButtonElement>('[data-action="plan-copy"]');
    expect(execute?.disabled).toBe(true);
    expect(discard?.disabled).toBe(true);
    expect(revise?.disabled).toBe(false);
    expect(copy?.disabled).toBe(false);

    executePendingPlan(execute);
    discardPlan();
    expect(state.ws?.send).not.toHaveBeenCalled();
  });

  it('serializes discard requests until the backend acknowledges the action', () => {
    renderPendingPlanAction({
      plan_id: 'plan_discard_once',
      message_index: 2,
      created_at: 1710000000,
    });

    discardPlan();
    refreshPlanLanguage();
    discardPlan();

    expect(state.ws?.send).toHaveBeenCalledTimes(1);
    expect(state.busy).toBe(false);
    expect(
      document
        .querySelector('.plan-artifact-card:not([data-historical="true"])')
        ?.getAttribute('aria-busy'),
    ).toBe('true');
    expect(
      document.querySelector<HTMLButtonElement>('[data-action="plan-discard"]')?.disabled,
    ).toBe(true);

    renderPlanState({ ...state.activePlan!, updated_at: 1710000001 });
    discardPlan();

    expect(state.ws?.send).toHaveBeenCalledTimes(2);
  });

  it('disables and blocks plan execution while an attachment upload is pending', () => {
    state.imageUploadInFlight = true;
    renderPendingPlanAction({
      plan_id: 'plan_123',
      message_index: 2,
      created_at: 1710000000,
    });

    const button = document.querySelector<HTMLButtonElement>('[data-action="execute-plan"]');
    expect(button?.disabled).toBe(true);

    executePendingPlan(button);

    expect(state.ws?.send).not.toHaveBeenCalled();
    expect(state.busy).toBe(false);
    expect(state.pendingPlanExecutionId).toBe('');
  });

  it('does not add a synthetic execution transcript after backend start', () => {
    renderPendingPlanAction({
      plan_id: 'plan_123',
      message_index: 2,
      created_at: 1710000000,
    });

    const button = document.querySelector<HTMLButtonElement>('[data-action="execute-plan"]');
    executePendingPlan(button);
    confirmPendingPlanExecution();

    expect(document.querySelector('.msg-row.user .msg')).toBeNull();
    expect(state.pendingPlanExecutionId).toBe('');
  });

  it('restores the execute button if backend rejects execution', () => {
    renderPendingPlanAction({
      plan_id: 'plan_123',
      message_index: 2,
      created_at: 1710000000,
    });

    const button = document.querySelector<HTMLButtonElement>('[data-action="execute-plan"]');
    executePendingPlan(button);
    restorePendingPlanAction();

    const restored = document.querySelector<HTMLButtonElement>('[data-action="execute-plan"]');
    expect(restored?.disabled).toBe(false);
    expect(restored?.textContent).toBe('Execute plan');
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

    expect(document.querySelector('.plan-artifact-card')).toBeNull();
    expect(state.pendingPlanId).toBe('');
    expect(state.pendingPlanMessageIndex).toBeNull();
    expect(state.pendingPlanExecutionId).toBe('');
  });

  it('renders blocking questions and sends answers as a new revision request', () => {
    renderPlanState({
      plan_id: 'plan_questions',
      revision: 4,
      status: 'needs_input',
      message_index: 2,
      created_at: 1710000000,
      updated_at: 1710000001,
      artifact: {
        title: 'Choose storage',
        goal: 'Select the storage strategy',
        steps: [{ id: 'inspect', title: 'Inspect the current storage' }],
        questions: [
          {
            id: 'storage',
            prompt: 'Which storage should be used?',
            options: [
              { id: 'sqlite', label: 'SQLite' },
              { id: 'files', label: 'Files' },
            ],
          },
        ],
      },
      progress: [
        { id: 'inspect', title: 'Inspect the current storage', status: 'completed' },
        {
          id: 'compat',
          title: 'Preserve compatibility',
          status: 'pending',
          deviation_reason: 'A legacy format was discovered',
        },
      ],
    });

    expect(document.querySelectorAll('.plan-step')).toHaveLength(2);
    const sqlite = document.querySelector<HTMLInputElement>('input[value="sqlite"]');
    if (sqlite) sqlite.checked = true;
    const submit = document.querySelector<HTMLElement>('[data-action="plan-submit-feedback"]');
    submitPlanFeedback(submit);

    expect(state.ws?.send).toHaveBeenCalledWith(
      JSON.stringify({
        plan_action: {
          action: 'feedback',
          plan_id: 'plan_questions',
          revision: 4,
          answers: { storage: 'SQLite' },
        },
      }),
    );
    expect(state.busy).toBe(true);
  });

  it('prefers a custom question answer over the selected option', () => {
    renderPlanState({
      plan_id: 'plan_custom_answer',
      revision: 2,
      status: 'needs_input',
      message_index: 2,
      created_at: 1710000000,
      updated_at: 1710000001,
      artifact: {
        title: 'Choose storage',
        goal: 'Select the storage strategy',
        steps: [{ id: 'inspect', title: 'Inspect' }],
        questions: [
          {
            id: 'storage',
            prompt: 'Which storage should be used?',
            options: [{ id: 'sqlite', label: 'SQLite' }],
          },
        ],
      },
      progress: [{ id: 'inspect', title: 'Inspect', status: 'pending' }],
    });

    document.querySelector<HTMLInputElement>('input[value="sqlite"]')!.checked = true;
    document.querySelector<HTMLTextAreaElement>('[data-plan-question="storage"]')!.value =
      'PostgreSQL with row-level security';
    submitPlanFeedback(document.querySelector('[data-action="plan-submit-feedback"]'));

    expect(state.ws?.send).toHaveBeenCalledWith(
      JSON.stringify({
        plan_action: {
          action: 'feedback',
          plan_id: 'plan_custom_answer',
          revision: 2,
          answers: { storage: 'PostgreSQL with row-level security' },
        },
      }),
    );
  });

  it('restores durable feedback after an interrupted planning run', () => {
    renderPlanState({
      plan_id: 'plan_recovered_feedback',
      revision: 3,
      status: 'stopped',
      message_index: 2,
      created_at: 1710000000,
      updated_at: 1710000001,
      pending_feedback: 'Keep the rollback step and use SQLite.',
      artifact: {
        title: 'Recovered plan',
        goal: 'Resume without losing feedback',
        steps: [{ id: 'recover', title: 'Recover' }],
      },
      progress: [{ id: 'recover', title: 'Recover', status: 'pending' }],
    });

    expect(document.querySelector<HTMLElement>('.plan-feedback-editor')?.hidden).toBe(false);
    expect(document.querySelector<HTMLTextAreaElement>('[data-plan-feedback]')?.value).toBe(
      'Keep the rollback step and use SQLite.',
    );
  });

  it('copies the complete structured plan', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: { writeText },
    });
    renderPlanState({
      plan_id: 'plan_copy',
      revision: 1,
      status: 'ready',
      message_index: 2,
      created_at: 1710000000,
      updated_at: 1710000001,
      artifact: {
        title: 'Complete plan',
        goal: 'Copy every planning field',
        summary: 'A concise summary',
        steps: [
          {
            id: 'implement',
            title: 'Implement',
            description: 'Apply the change',
            affected_areas: ['src/plan.rs'],
          },
        ],
        assumptions: ['SQLite is available'],
        risks: ['Migration can fail'],
        acceptance_criteria: ['Feedback survives restart'],
        verification: ['Run storage tests'],
        questions: [
          {
            id: 'strategy',
            prompt: 'Which strategy?',
            options: [{ id: 'safe', label: 'Safe', description: 'Prefer compatibility' }],
          },
        ],
      },
      progress: [{ id: 'implement', title: 'Implement', status: 'pending' }],
    });

    await copyPlan();

    const copied = String(writeText.mock.calls[0]?.[0]);
    expect(copied).toContain('Complete plan');
    expect(copied).toContain('src/plan.rs');
    expect(copied).toContain('SQLite is available');
    expect(copied).toContain('Migration can fail');
    expect(copied).toContain('Feedback survives restart');
    expect(copied).toContain('Run storage tests');
    expect(copied).toContain('Which strategy?');
    expect(copied).toContain('Prefer compatibility');
  });

  it('copies a question-only plan when the backend omits empty steps', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: { writeText },
    });
    renderPlanState({
      plan_id: 'plan_copy_questions',
      revision: 1,
      status: 'needs_input',
      message_index: 2,
      created_at: 1710000000,
      updated_at: 1710000001,
      artifact: {
        title: 'Choose a strategy',
        goal: 'Resolve the blocking decision',
        questions: [{ id: 'strategy', prompt: 'Which strategy should be used?' }],
      },
      progress: [],
    });

    await expect(copyPlan()).resolves.toBeUndefined();

    const copied = String(writeText.mock.calls[0]?.[0]);
    expect(copied).toContain('Choose a strategy');
    expect(copied).toContain('Which strategy should be used?');
  });

  it('localizes the initial image-only placeholder instead of exposing backend English', () => {
    setLanguage('zh-CN');
    renderPlanState({
      plan_id: 'plan_image_placeholder',
      revision: 1,
      status: 'stopped',
      message_index: 2,
      created_at: 1710000000,
      updated_at: 1710000001,
      initial_submission_pending: true,
      initial_request_image_only: true,
      artifact: {
        title: 'Planning',
        goal: 'Prepare a plan using the attached image input.',
      },
      progress: [],
    });

    expect(document.querySelector('.plan-artifact-card h3')?.textContent).toBe('规划中');
    expect(document.querySelector('.plan-card-goal')?.textContent).toBe('根据所附图片制定计划。');
    expect(document.querySelector('.plan-artifact-card')?.getAttribute('aria-label')).not.toContain(
      'Planning',
    );
  });

  it('shows changed evidence and restores controls when approval is stale', () => {
    renderPendingPlanAction({
      plan_id: 'plan_stale',
      revision: 2,
      message_index: 2,
      created_at: 1710000000,
    });
    executePendingPlan(document.querySelector<HTMLButtonElement>('[data-action="execute-plan"]'));

    handlePlanStale({
      plan_id: 'plan_stale',
      revision: 2,
      paths: ['src/main.rs'],
      confirmation_token: 'stale-token',
    });

    expect(state.busy).toBe(false);
    expect(state.planStaleConfirmationToken).toBe('stale-token');
    expect(document.querySelector('.plan-stale-notice code')?.textContent).toBe('src/main.rs');
    expect(document.querySelector('[data-action="plan-refresh"]')).not.toBeNull();
    expect(document.querySelector('[data-action="plan-execute-stale"]')).not.toBeNull();
  });

  it('requires an explicit choice when plan evidence is incomplete', () => {
    renderPlanState({
      plan_id: 'plan_incomplete_evidence',
      revision: 1,
      status: 'ready',
      message_index: 2,
      created_at: 1710000000,
      updated_at: 1710000000,
      evidence_truncated: true,
      artifact: {
        title: 'Incomplete evidence',
        goal: 'Do not silently bypass verification',
        steps: [{ id: 'verify', title: 'Verify' }],
      },
      progress: [{ id: 'verify', title: 'Verify', status: 'pending' }],
    });
    executePendingPlan(document.querySelector<HTMLButtonElement>('[data-action="execute-plan"]'));

    handlePlanStale({
      plan_id: 'plan_incomplete_evidence',
      revision: 1,
      paths: [],
      evidence_incomplete: true,
      confirmation_token: 'incomplete-token',
    });

    expect(document.querySelector('.plan-stale-notice')?.textContent).toContain(
      'Some planning evidence could not be fully recorded or verified.',
    );
    expect(document.querySelector('.plan-stale-notice code')).toBeNull();
    expect(document.querySelector('[data-action="plan-refresh"]')).not.toBeNull();
    expect(document.querySelector('[data-action="plan-execute-stale"]')).not.toBeNull();
  });

  it('retries a stale stopped execution with resume instead of execute', () => {
    renderPlanState({
      plan_id: 'plan_stale_resume',
      revision: 3,
      status: 'stopped',
      message_index: 2,
      created_at: 1710000000,
      updated_at: 1710000001,
      approved_at: 1710000000,
      execution_attempt: 1,
      artifact: {
        title: 'Resume safely',
        goal: 'Finish the approved work',
        steps: [{ id: 'finish', title: 'Finish' }],
      },
      progress: [{ id: 'finish', title: 'Finish', status: 'pending' }],
    });

    resumePlan();
    handlePlanStale({
      plan_id: 'plan_stale_resume',
      revision: 3,
      paths: ['src/main.rs'],
      confirmation_token: 'resume-stale-token',
    });
    expect(document.querySelector('.plan-stale-notice code')?.textContent).toBe('src/main.rs');
    expect(document.querySelector('[data-action="plan-execute-stale"]')).not.toBeNull();
    executeStalePlan();

    expect(state.ws?.send).toHaveBeenLastCalledWith(
      JSON.stringify({
        plan_action: {
          action: 'resume',
          plan_id: 'plan_stale_resume',
          revision: 3,
          allow_stale: true,
          stale_confirmation_token: 'resume-stale-token',
        },
      }),
    );
  });

  it('preserves question answers and revision feedback when the plan card is localized again', () => {
    renderPlanState({
      plan_id: 'plan_draft',
      revision: 2,
      status: 'stopped',
      message_index: 2,
      created_at: 1710000000,
      updated_at: 1710000001,
      artifact: {
        title: 'Keep the draft',
        goal: 'Preserve unsent input',
        steps: [{ id: 'draft', title: 'Draft' }],
        questions: [
          {
            id: 'storage',
            prompt: 'Which storage?',
            options: [
              { id: 'sqlite', label: 'SQLite' },
              { id: 'files', label: 'Files' },
            ],
          },
        ],
      },
      progress: [{ id: 'draft', title: 'Draft', status: 'pending' }],
    });

    const selected = document.querySelector<HTMLInputElement>('input[value="sqlite"]');
    if (selected) selected.checked = true;
    const answer = document.querySelector<HTMLTextAreaElement>('[data-plan-question="storage"]');
    if (answer) answer.value = 'Keep WAL enabled';
    togglePlanFeedback(document.querySelector('[data-action="plan-toggle-feedback"]'));
    const feedback = document.querySelector<HTMLTextAreaElement>('[data-plan-feedback]');
    if (feedback) feedback.value = 'Add a rollback step';

    refreshPlanLanguage();

    expect(document.querySelector<HTMLInputElement>('input[value="sqlite"]')?.checked).toBe(true);
    expect(
      document.querySelector<HTMLTextAreaElement>('[data-plan-question="storage"]')?.value,
    ).toBe('Keep WAL enabled');
    expect(document.querySelector<HTMLTextAreaElement>('[data-plan-feedback]')?.value).toBe(
      'Add a rollback step',
    );
    expect(document.querySelector<HTMLElement>('.plan-feedback-editor')?.hidden).toBe(false);
  });

  it('keeps unsent plan drafts isolated while switching between Sessions', () => {
    state.activeSessionId = 'session-alpha';
    const alpha: Parameters<typeof renderPlanState>[0] = {
      plan_id: 'shared-plan-id',
      revision: 1,
      status: 'needs_input',
      message_index: 2,
      created_at: 1710000000,
      updated_at: 1710000001,
      artifact: {
        title: 'Session-specific draft',
        goal: 'Keep each Session draft isolated',
        steps: [{ id: 'database', title: 'Choose a database' }],
        questions: [
          {
            id: 'database',
            prompt: 'Which database?',
            options: [{ id: 'sqlite', label: 'SQLite' }],
          },
        ],
      },
      progress: [{ id: 'database', title: 'Choose a database', status: 'pending' }],
    };
    renderPlanState(alpha);
    document.querySelector<HTMLInputElement>('input[value="sqlite"]')!.checked = true;
    document.querySelector<HTMLTextAreaElement>('[data-plan-question="database"]')!.value =
      'Keep WAL enabled';

    state.activeSessionId = 'session-beta';
    clearPendingPlanAction();
    renderPlanState({
      ...alpha,
      status: 'ready',
      artifact: { ...alpha.artifact, questions: [] },
    });
    togglePlanFeedback();
    document.querySelector<HTMLTextAreaElement>('[data-plan-feedback]')!.value = 'Beta revision';

    state.activeSessionId = 'session-alpha';
    clearPendingPlanAction();
    renderPlanState(alpha);

    expect(document.querySelector<HTMLInputElement>('input[value="sqlite"]')?.checked).toBe(true);
    expect(
      document.querySelector<HTMLTextAreaElement>('[data-plan-question="database"]')?.value,
    ).toBe('Keep WAL enabled');
    expect(document.querySelector<HTMLTextAreaElement>('[data-plan-feedback]')?.value).toBe('');
  });

  it('defers historical plan cards until their message is rendered', () => {
    document.querySelector('.msg-row.assistant')?.setAttribute('data-message-index', '4');
    state.deferredHistory = [{ role: 'assistant', content: 'Earlier plan', message_index: 2 }];
    const base = {
      plan_id: 'plan_deferred',
      status: 'ready' as const,
      created_at: 1710000000,
      updated_at: 1710000001,
      progress: [{ id: 'inspect', title: 'Inspect', status: 'pending' as const }],
    };
    renderPlanHistory([
      {
        ...base,
        revision: 1,
        message_index: 2,
        historical: true,
        artifact: {
          title: 'Deferred revision',
          goal: 'Inspect the earlier state',
          steps: [{ id: 'inspect', title: 'Inspect' }],
        },
      },
      {
        ...base,
        revision: 2,
        message_index: 4,
        artifact: {
          title: 'Current revision',
          goal: 'Inspect the current state',
          steps: [{ id: 'inspect', title: 'Inspect' }],
        },
      },
    ]);

    expect(document.querySelectorAll('.plan-artifact-card')).toHaveLength(1);
    expect(document.querySelector('.plan-artifact-row')).toBeNull();

    const earlier = document.createElement('div');
    earlier.className = 'msg-row assistant';
    earlier.dataset.messageIndex = '2';
    earlier.innerHTML =
      '<div class="msg-content"><div class="msg assistant">Earlier plan</div></div>';
    dom.chat?.prepend(earlier);
    state.deferredHistory = [];
    refreshPlanMounts();

    expect(document.querySelectorAll('.plan-artifact-card')).toHaveLength(2);
    expect(earlier.querySelector<HTMLElement>('.msg')?.hidden).toBe(true);
    expect(document.querySelector('.plan-artifact-row')).toBeNull();
  });

  it('defers the primary terminal plan until its older message anchor is rendered', () => {
    document.querySelector('.msg-row.assistant')?.setAttribute('data-message-index', '240');
    state.deferredHistory = [{ role: 'assistant', content: 'Completed plan', message_index: 12 }];

    renderPlanHistory([
      {
        plan_id: 'plan_terminal_deferred',
        revision: 1,
        status: 'completed',
        message_index: 12,
        created_at: 1710000000,
        updated_at: 1710000001,
        artifact: {
          title: 'Earlier completed plan',
          goal: 'Keep this plan in chronological order',
          steps: [{ id: 'done', title: 'Finish' }],
        },
        progress: [{ id: 'done', title: 'Finish', status: 'completed' }],
      },
    ]);

    expect(document.querySelector('.plan-artifact-card')).toBeNull();
    expect(document.querySelector('.plan-artifact-row')).toBeNull();

    const earlier = document.createElement('div');
    earlier.className = 'msg-row assistant';
    earlier.dataset.messageIndex = '12';
    earlier.innerHTML =
      '<div class="msg-content"><div class="msg assistant">Completed plan</div></div>';
    dom.chat?.prepend(earlier);
    state.deferredHistory = [];
    refreshPlanMounts();

    expect(earlier.querySelector('.plan-artifact-card')).not.toBeNull();
    expect(earlier.querySelector<HTMLElement>('.msg')?.hidden).toBe(true);
    expect(document.querySelector('.plan-artifact-row')).toBeNull();
  });

  it('keeps historical revisions whose message anchor was pruned', () => {
    document.querySelector('.msg-row.assistant')?.setAttribute('data-message-index', '4');
    const base = {
      plan_id: 'plan_pruned',
      status: 'ready' as const,
      created_at: 1710000000,
      updated_at: 1710000001,
      progress: [{ id: 'inspect', title: 'Inspect', status: 'pending' as const }],
    };

    renderPlanHistory([
      {
        ...base,
        revision: 1,
        message_index: 0,
        historical: true,
        artifact: {
          title: 'Pruned revision',
          goal: 'Preserve this revision',
          steps: [{ id: 'inspect', title: 'Inspect' }],
        },
      },
      {
        ...base,
        revision: 2,
        message_index: 4,
        artifact: {
          title: 'Current revision',
          goal: 'Use the current revision',
          steps: [{ id: 'inspect', title: 'Inspect' }],
        },
      },
    ]);

    const fallback = document.querySelector<HTMLElement>(
      '.plan-artifact-row[data-historical="true"]',
    );
    const currentRow = document.querySelector<HTMLElement>(
      '.msg-row.assistant[data-message-index="4"]',
    );
    expect(fallback).not.toBeNull();
    expect(fallback?.querySelector('summary')?.textContent).toContain('Pruned revision');
    expect(fallback?.nextElementSibling).toBe(currentRow);
  });

  it('removes streamed prose when a structured plan submission completes', () => {
    const streamRow = document.createElement('div');
    streamRow.className = 'msg-row assistant';
    const streamMessage = document.createElement('div');
    streamMessage.className = 'msg assistant typing';
    streamMessage.textContent = 'I also wrote an ordinary plan.';
    streamRow.appendChild(streamMessage);
    dom.chat?.appendChild(streamRow);
    state.currentMsg = streamMessage;
    state.pendingAssistantText = ' trailing text';
    const planning = {
      plan_id: 'plan_streamed',
      revision: 1,
      status: 'planning' as const,
      message_index: 2,
      created_at: 1710000000,
      updated_at: 1710000001,
      artifact: {
        title: 'Planning',
        goal: 'Create the structured plan',
        steps: [] as { id: string; title: string }[],
      },
      progress: [],
    };
    state.activePlan = planning;

    renderPlanState({
      ...planning,
      status: 'ready',
      artifact: {
        title: 'Structured plan',
        goal: 'Show only the structured card',
        steps: [{ id: 'implement', title: 'Implement' }],
      },
      progress: [{ id: 'implement', title: 'Implement', status: 'pending' }],
    });

    expect(streamRow.isConnected).toBe(false);
    expect(state.currentMsg).toBeNull();
    expect(state.pendingAssistantText).toBe('');
    expect(document.querySelectorAll('.plan-artifact-card')).toHaveLength(1);
  });

  it('folds superseded revisions and keeps actions on the current revision only', () => {
    document.body.innerHTML = `
      <div id="chat">
        <div class="msg-row assistant" data-message-index="2">
          <div class="msg-content"><div class="msg assistant">First plan</div></div>
        </div>
        <div class="msg-row assistant" data-message-index="4">
          <div class="msg-content"><div class="msg assistant">Second plan</div></div>
        </div>
      </div>
      <textarea id="input"></textarea>
      <button id="stop"></button>
      <button id="send"></button>
      <span id="send-icon"></span>
      <button id="execute-mode-toggle"></button>
      <button id="plan-mode-toggle"></button>
      <div id="plan-progress" hidden></div>
    `;
    dom.chat = document.getElementById('chat') as HTMLElement;
    dom.input = document.getElementById('input') as HTMLTextAreaElement;
    dom.stopBtn = document.getElementById('stop') as HTMLButtonElement;
    dom.sendBtn = document.getElementById('send') as HTMLButtonElement;
    dom.sendIcon = document.getElementById('send-icon');
    dom.executeModeToggle = document.getElementById('execute-mode-toggle') as HTMLButtonElement;
    dom.planModeToggle = document.getElementById('plan-mode-toggle') as HTMLButtonElement;
    dom.planProgress = document.getElementById('plan-progress') as HTMLElement;

    const base = {
      plan_id: 'plan_revisions',
      status: 'ready' as const,
      created_at: 1710000000,
      updated_at: 1710000001,
      progress: [{ id: 'inspect', title: 'Inspect', status: 'pending' as const }],
    };
    renderPlanHistory([
      {
        ...base,
        revision: 1,
        message_index: 2,
        historical: true,
        artifact: {
          title: 'First revision',
          goal: 'Inspect the project',
          steps: [{ id: 'inspect', title: 'Inspect' }],
          questions: [
            {
              id: 'scope',
              prompt: 'Which scope?',
              options: [{ id: 'focused', label: 'Focused' }],
            },
          ],
        },
      },
      {
        ...base,
        revision: 2,
        message_index: 4,
        artifact: {
          title: 'Second revision',
          goal: 'Inspect and verify the project',
          steps: [{ id: 'inspect', title: 'Inspect' }],
          questions: [
            {
              id: 'scope',
              prompt: 'Which scope?',
              options: [{ id: 'complete', label: 'Complete' }],
            },
          ],
        },
      },
    ]);

    const previous = document.querySelector<HTMLDetailsElement>('.plan-revision-history');
    expect(previous).not.toBeNull();
    expect(previous?.open).toBe(false);
    expect(previous?.querySelector('summary')?.textContent).toContain('Previous revision · v1');
    expect(previous?.querySelector('[data-action]')).toBeNull();
    expect(document.querySelectorAll('[data-action="execute-plan"]')).toHaveLength(1);
    const historicalQuestions = previous?.querySelector<HTMLFieldSetElement>('.plan-questions');
    const currentQuestions = document.querySelector<HTMLFieldSetElement>(
      '.plan-artifact-card:not([data-historical="true"]) .plan-questions',
    );
    expect(historicalQuestions?.disabled).toBe(true);
    expect(historicalQuestions?.querySelector('input')?.matches(':disabled')).toBe(true);
    expect(historicalQuestions?.querySelector<HTMLInputElement>('input')?.name).not.toBe(
      currentQuestions?.querySelector<HTMLInputElement>('input')?.name,
    );
    expect(state.activePlan?.revision).toBe(2);
    expect(state.planHistory).toHaveLength(1);
  });

  it('focuses the replacement feedback editor after the plan card rerenders', () => {
    renderPlanState({
      plan_id: 'plan_feedback_focus',
      revision: 1,
      status: 'ready',
      message_index: 2,
      created_at: 1710000000,
      updated_at: 1710000001,
      artifact: {
        title: 'Revise safely',
        goal: 'Keep keyboard focus in the current card',
        steps: [{ id: 'revise', title: 'Revise' }],
      },
      progress: [{ id: 'revise', title: 'Revise', status: 'pending' }],
    });

    const oldToggle = document.querySelector<HTMLElement>('[data-action="plan-toggle-feedback"]');
    togglePlanFeedback(oldToggle);

    const feedback = document.querySelector<HTMLTextAreaElement>('[data-plan-feedback]');
    expect(oldToggle?.isConnected).toBe(false);
    expect(feedback?.isConnected).toBe(true);
    expect(document.activeElement).toBe(feedback);
  });

  it.each(['completed', 'discarded'] as const)(
    'keeps a %s plan read-only instead of offering an invalid revision action',
    (status) => {
      renderPlanState({
        plan_id: `plan_${status}`,
        revision: 1,
        status,
        message_index: 2,
        created_at: 1710000000,
        updated_at: 1710000001,
        artifact: {
          title: 'Terminal plan',
          goal: 'Finish once',
          steps: [{ id: 'finish', title: 'Finish' }],
        },
        progress: [{ id: 'finish', title: 'Finish', status: 'completed' }],
      });

      expect(document.querySelector('[data-action="plan-toggle-feedback"]')).toBeNull();
      expect(document.querySelector('[data-action="plan-copy"]')).not.toBeNull();
    },
  );

  it('keeps Execute mode unavailable while an unresolved plan is active', () => {
    const plan = {
      plan_id: 'plan_mode_lock',
      revision: 1,
      status: 'ready' as const,
      message_index: 2,
      created_at: 1710000000,
      updated_at: 1710000001,
      artifact: {
        title: 'Plan before execution',
        goal: 'Resolve the active plan first',
        steps: [{ id: 'finish', title: 'Finish the plan' }],
      },
      progress: [{ id: 'finish', title: 'Finish the plan', status: 'pending' as const }],
    };

    renderPlanState(plan);

    expect(state.planModeEnabled).toBe(true);
    expect(dom.planModeToggle?.getAttribute('aria-pressed')).toBe('true');
    expect(dom.executeModeToggle?.getAttribute('aria-pressed')).toBe('false');
    expect(dom.executeModeToggle?.disabled).toBe(true);
    expect(dom.executeModeToggle?.title).not.toBe('');

    renderPlanState({
      ...plan,
      status: 'completed',
      progress: [{ id: 'finish', title: 'Finish the plan', status: 'completed' }],
    });

    expect(state.planModeEnabled).toBe(false);
    expect(dom.executeModeToggle?.disabled).toBe(false);
    expect(dom.executeModeToggle?.getAttribute('aria-pressed')).toBe('true');
    expect(dom.executeModeToggle?.title).toBe('');
  });

  it('offers revision instead of execution for a stopped unapproved planning run', () => {
    renderPlanState({
      plan_id: 'plan_interrupted_while_planning',
      revision: 1,
      status: 'stopped',
      message_index: 2,
      created_at: 1710000000,
      updated_at: 1710000001,
      artifact: {
        title: 'Interrupted planning',
        goal: 'Finish drafting the plan',
        steps: [{ id: 'draft', title: 'Draft the plan' }],
      },
      progress: [{ id: 'draft', title: 'Draft the plan', status: 'pending' }],
      execution_attempt: 0,
    });

    expect(document.querySelector('[data-action="plan-resume"]')).toBeNull();
    expect(document.querySelector('[data-action="plan-toggle-feedback"]')).not.toBeNull();
    expect(document.querySelector('[data-action="plan-discard"]')).not.toBeNull();
  });

  it('offers resume only for a stopped plan with an approved execution attempt', () => {
    renderPlanState({
      plan_id: 'plan_interrupted_while_executing',
      revision: 2,
      status: 'stopped',
      message_index: 2,
      created_at: 1710000000,
      updated_at: 1710000001,
      approved_at: 1710000000,
      execution_attempt: 1,
      artifact: {
        title: 'Interrupted execution',
        goal: 'Resume the approved work',
        steps: [{ id: 'execute', title: 'Execute the plan' }],
      },
      progress: [{ id: 'execute', title: 'Execute the plan', status: 'pending' }],
    });

    expect(document.querySelector('[data-action="plan-resume"]')).not.toBeNull();
    expect(document.querySelector('[data-action="plan-toggle-feedback"]')).not.toBeNull();
    expect(document.querySelector('[data-action="plan-discard"]')).toBeNull();
  });

  it('marks historical cards and jumps to the current revision', () => {
    if (dom.chat) {
      dom.chat.innerHTML = `
        <div class="msg-row assistant" data-message-index="2">
          <div class="msg-content"><div class="msg assistant">Old plan</div></div>
        </div>
        <div class="msg-row assistant" data-message-index="4">
          <div class="msg-content"><div class="msg assistant">Current plan</div></div>
        </div>
      `;
    }
    const base = {
      plan_id: 'plan_focus',
      status: 'ready' as const,
      created_at: 1710000000,
      updated_at: 1710000001,
      progress: [{ id: 'inspect', title: 'Inspect', status: 'pending' as const }],
    };
    renderPlanHistory([
      {
        ...base,
        revision: 1,
        message_index: 2,
        historical: true,
        artifact: {
          title: 'Old revision',
          goal: 'Old goal',
          steps: [{ id: 'inspect', title: 'Inspect' }],
        },
      },
      {
        ...base,
        revision: 2,
        message_index: 4,
        artifact: {
          title: 'Current revision',
          goal: 'Current goal',
          steps: [{ id: 'inspect', title: 'Inspect' }],
        },
      },
    ]);

    const historical = document.querySelector<HTMLElement>('.plan-artifact-card.is-historical');
    const current = document.querySelector<HTMLElement>(
      '.plan-artifact-card:not([data-historical="true"])',
    );
    expect(historical?.dataset.historical).toBe('true');
    expect(current?.dataset.historical).toBe('false');
    if (historical) historical.scrollIntoView = vi.fn();
    if (current) current.scrollIntoView = vi.fn();

    jumpToPlan();

    expect(current?.scrollIntoView).toHaveBeenCalled();
    expect(historical?.scrollIntoView).not.toHaveBeenCalled();
    expect(document.activeElement).toBe(current);
  });
});
