import { dom, state } from '../state.js';
import type { PlanReadyPayload, PlanStatePayload, PlanStatus } from '../types.js';
import { invalidateChatScrollCache, scrollDown } from '../scroll.js';
import { setBusy } from './chat.js';
import { isComposerModelReady, syncComposerAvailability } from '../composerAvailability.js';
import { createIcon } from '../icons.js';
import { tr } from '../i18n.js';
import { setPlanMode, syncPlanModeToggle } from '../images.js';
import { discardAssistantStream } from '../handlers/stream.js';

interface PlanFormDraft {
  feedbackOpen: boolean;
  feedbackText: string;
  answers: Record<string, { optionId: string; freeText: string }>;
}

const planFormDrafts = new Map<string, PlanFormDraft>();
const PLAN_WRITE_ACTIONS = new Set([
  'execute-plan',
  'plan-execute-stale',
  'plan-discard',
  'plan-refresh',
  'plan-resume',
  'plan-submit-feedback',
]);
const PLAN_MODEL_RUN_ACTIONS = new Set([
  'execute-plan',
  'plan-execute-stale',
  'plan-refresh',
  'plan-resume',
  'plan-submit-feedback',
]);
let feedbackOpen = false;
let activePlanSessionId = '';
let planActionInFlight = false;

function setPlanActionInFlight(inFlight: boolean): void {
  planActionInFlight = inFlight;
  document
    .querySelectorAll<HTMLElement>('.plan-artifact-card:not([data-historical="true"])')
    .forEach((card) => {
      if (inFlight) card.setAttribute('aria-busy', 'true');
      else card.removeAttribute('aria-busy');
    });
  if (!inFlight) return;
  document.querySelectorAll<HTMLButtonElement>('[data-action]').forEach((button) => {
    if (PLAN_WRITE_ACTIONS.has(button.dataset.action || '')) button.disabled = true;
  });
}

function planDraftKey(plan: PlanStatePayload, sessionId = state.activeSessionId || 'main'): string {
  return `${sessionId}:${plan.plan_id}:${plan.revision}`;
}

function planQuestionGroupName(plan: PlanStatePayload, questionId: string): string {
  return `plan-question-${plan.plan_id}-${plan.revision}-${questionId}`;
}

function captureCurrentPlanDraft(): void {
  const plan = state.activePlan;
  if (!plan) return;
  const card = document.querySelector<HTMLElement>(
    '.plan-artifact-card:not([data-historical="true"])',
  );
  if (!card) return;

  const answers: PlanFormDraft['answers'] = {};
  card.querySelectorAll<HTMLTextAreaElement>('textarea[data-plan-question]').forEach((input) => {
    const questionId = input.dataset.planQuestion;
    if (!questionId) return;
    const selected = Array.from(
      card.querySelectorAll<HTMLInputElement>('input[type="radio"]:checked'),
    ).find((option) => option.name === planQuestionGroupName(plan, questionId));
    answers[questionId] = {
      optionId: selected?.value || '',
      freeText: input.value,
    };
  });
  planFormDrafts.set(planDraftKey(plan, activePlanSessionId || state.activeSessionId || 'main'), {
    feedbackOpen,
    feedbackText: card.querySelector<HTMLTextAreaElement>('[data-plan-feedback]')?.value || '',
    answers,
  });
}

function restorePlanFormDraft(card: HTMLElement, plan: PlanStatePayload): void {
  const draft = planFormDrafts.get(planDraftKey(plan));
  const feedback = card.querySelector<HTMLTextAreaElement>('[data-plan-feedback]');
  if (!draft) {
    if (feedback && plan.pending_feedback) feedback.value = plan.pending_feedback;
    return;
  }
  if (feedback) feedback.value = draft.feedbackText;
  for (const [questionId, answer] of Object.entries(draft.answers)) {
    const freeText = Array.from(
      card.querySelectorAll<HTMLTextAreaElement>('textarea[data-plan-question]'),
    ).find((input) => input.dataset.planQuestion === questionId);
    if (freeText) freeText.value = answer.freeText;
    const selected = Array.from(
      card.querySelectorAll<HTMLInputElement>('input[type="radio"]'),
    ).find(
      (option) =>
        option.name === planQuestionGroupName(plan, questionId) && option.value === answer.optionId,
    );
    if (selected) selected.checked = true;
  }
}

function planIdentityTransitionInFlight(): boolean {
  return (
    state.sessionSwitchInFlight ||
    state.sessionIdentityMutationInFlight ||
    state.composerSessionTransitionPending ||
    state.composerSessionIdentityPending
  );
}

function activePlanTargetsCurrentSession(): boolean {
  return Boolean(
    state.activePlan &&
    activePlanSessionId &&
    !state.activeGroupId &&
    activePlanSessionId === (state.activeSessionId || 'main'),
  );
}

function canExecutePendingPlan(): boolean {
  return (
    activePlanTargetsCurrentSession() &&
    state.storageMode !== 'protected' &&
    isComposerModelReady() &&
    !state.imageUploadInFlight &&
    !planIdentityTransitionInFlight()
  );
}

function statusLabel(status: PlanStatus): string {
  return tr(`plan.status.${status}`);
}

function isApprovedExecution(plan: PlanStatePayload): boolean {
  return Boolean(plan.approved_at) && (plan.execution_attempt || 0) > 0;
}

function setSessionPlanModeForStatus(status: PlanStatus): void {
  const planning = status === 'planning' || status === 'needs_input' || status === 'ready';
  state.planModeEnabled = planning && !state.activeGroupId;
  state.planModesBySession.set(state.activeSessionId || 'main', state.planModeEnabled);
  syncPlanModeToggle();
}

function restoreHiddenPlanMessages(): void {
  document
    .querySelectorAll('.plan-artifact-card, .plan-revision-history')
    .forEach((node) => node.remove());
  document
    .querySelectorAll<HTMLElement>('.msg-row.plan-artifact-message .msg')
    .forEach((message) => {
      message.hidden = false;
    });
  document
    .querySelectorAll<HTMLElement>('.msg-row.plan-artifact-message')
    .forEach((row) => row.classList.remove('plan-artifact-message'));
}

export function clearPendingPlanAction(): void {
  captureCurrentPlanDraft();
  setPlanActionInFlight(false);
  state.pendingPlanId = '';
  state.pendingPlanMessageIndex = null;
  state.pendingPlanExecutionId = '';
  state.activePlan = null;
  state.planHistory = [];
  state.planStalePaths = [];
  state.planStaleConfirmationToken = '';
  feedbackOpen = false;
  activePlanSessionId = '';
  restoreHiddenPlanMessages();
  document
    .querySelectorAll('.plan-execute-action, .plan-artifact-row')
    .forEach((node) => node.remove());
  if (dom.planProgress) {
    dom.planProgress.hidden = true;
    dom.planProgress.replaceChildren();
  }
  syncPlanModeToggle();
}

export function clearPlanStateForSessionTransition(nextSessionId: string): void {
  if (!state.activePlan) return;
  const targetSessionId = String(nextSessionId || 'main').trim() || 'main';
  if (activePlanSessionId === targetSessionId) return;
  clearPendingPlanAction();
}

export function restorePendingPlanAction(): void {
  setPlanActionInFlight(false);
  state.pendingPlanExecutionId = '';
  if (state.activePlan) renderPlanState(state.activePlan, false);
}

// Approval is a control action, not a synthetic user message.
export function confirmPendingPlanExecution(): void {
  setPlanActionInFlight(false);
  state.pendingPlanExecutionId = '';
}

function assistantContentForPlan(messageIndex: number | null): Element | null {
  if (!dom.chat || typeof messageIndex !== 'number') return null;
  return dom.chat.querySelector(
    `.msg-row.assistant[data-message-index="${messageIndex}"] .msg-content`,
  );
}

function appendTextSection(
  card: HTMLElement,
  title: string,
  values: string[] | undefined,
  className = '',
): void {
  if (!values?.length) return;
  const section = document.createElement('section');
  section.className = `plan-card-section ${className}`.trim();
  const heading = document.createElement('h4');
  heading.textContent = title;
  section.appendChild(heading);
  const list = document.createElement('ul');
  values.forEach((value) => {
    const item = document.createElement('li');
    item.textContent = value;
    list.appendChild(item);
  });
  section.appendChild(list);
  card.appendChild(section);
}

function buildPlanSteps(plan: PlanStatePayload): HTMLElement {
  const section = document.createElement('section');
  section.className = 'plan-card-section plan-card-steps';
  const heading = document.createElement('h4');
  heading.textContent = tr('plan.steps');
  section.appendChild(heading);
  const list = document.createElement('ol');
  const progress = new Map(plan.progress.map((step) => [step.id, step]));
  const artifactSteps = plan.artifact.steps ?? [];
  const plannedIds = new Set(artifactSteps.map((step) => step.id));
  const steps = [
    ...artifactSteps,
    ...plan.progress
      .filter((step) => !plannedIds.has(step.id))
      .map((step) => ({ id: step.id, title: step.title, description: '' })),
  ];
  steps.forEach((step) => {
    const current = progress.get(step.id);
    const item = document.createElement('li');
    item.className = `plan-step is-${current?.status || 'pending'}`;
    const marker = document.createElement('span');
    marker.className = 'plan-step-marker';
    marker.appendChild(
      createIcon(
        current?.status === 'completed'
          ? 'check-circle'
          : current?.status === 'blocked'
            ? 'alert-triangle'
            : current?.status === 'in_progress'
              ? 'activity'
              : 'circle-dot',
      ),
    );
    const body = document.createElement('div');
    const title = document.createElement('strong');
    title.textContent = step.title;
    body.appendChild(title);
    const status = document.createElement('span');
    status.className = 'plan-step-status';
    status.textContent = tr(`plan.stepStatus.${current?.status || 'pending'}`);
    body.appendChild(status);
    if (step.description) {
      const description = document.createElement('p');
      description.textContent = step.description;
      body.appendChild(description);
    }
    if (current?.note) {
      const note = document.createElement('p');
      note.className = 'plan-step-note';
      note.textContent = current.note;
      body.appendChild(note);
    }
    if (current?.deviation_reason) {
      const deviation = document.createElement('p');
      deviation.className = 'plan-step-deviation';
      deviation.textContent = `${tr('plan.deviation')}: ${current.deviation_reason}`;
      body.appendChild(deviation);
    }
    item.append(marker, body);
    list.appendChild(item);
  });
  section.appendChild(list);
  return section;
}

function displayPlanTitle(plan: PlanStatePayload): string {
  return plan.initial_submission_pending
    ? tr('plan.status.planning')
    : plan.artifact.title || tr('plan.title');
}

function displayPlanGoal(plan: PlanStatePayload): string {
  return plan.initial_request_image_only ? tr('plan.initialImageGoal') : plan.artifact.goal;
}

function buildQuestions(plan: PlanStatePayload, historical: boolean): HTMLElement | null {
  const questions = plan.artifact.questions || [];
  if (!questions.length) return null;
  const section = document.createElement('fieldset');
  section.className = 'plan-card-section plan-questions';
  section.disabled = historical;
  const legend = document.createElement('legend');
  legend.textContent = tr('plan.questions');
  section.appendChild(legend);
  questions.forEach((question) => {
    const group = document.createElement('div');
    group.className = 'plan-question';
    const prompt = document.createElement('p');
    prompt.textContent = question.prompt;
    group.appendChild(prompt);
    if (question.options?.length) {
      question.options.forEach((option) => {
        const label = document.createElement('label');
        const input = document.createElement('input');
        input.type = 'radio';
        input.name = planQuestionGroupName(plan, question.id);
        input.value = option.id;
        input.dataset.answerLabel = option.label;
        label.append(input, document.createTextNode(option.label));
        if (option.description) label.title = option.description;
        group.appendChild(label);
      });
    }
    const input = document.createElement('textarea');
    input.rows = 2;
    input.dataset.planQuestion = question.id;
    input.placeholder = tr('plan.answerPlaceholder');
    input.setAttribute('aria-label', tr('plan.freeTextAnswer', { question: question.prompt }));
    group.appendChild(input);
    section.appendChild(group);
  });
  return section;
}

function actionButton(action: string, label: string, primary = false): HTMLButtonElement {
  const button = document.createElement('button');
  button.type = 'button';
  button.dataset.action = action;
  button.textContent = label;
  if (PLAN_WRITE_ACTIONS.has(action)) {
    button.classList.add('plan-write-btn');
    button.dataset.planSessionId = activePlanSessionId || state.activeSessionId || 'main';
    button.dataset.planId = state.activePlan?.plan_id || state.pendingPlanId;
    if (PLAN_MODEL_RUN_ACTIONS.has(action)) {
      button.dataset.planRequiresModel = 'true';
    }
    if (
      state.storageMode === 'protected' ||
      planActionInFlight ||
      planIdentityTransitionInFlight() ||
      !activePlanTargetsCurrentSession() ||
      (PLAN_MODEL_RUN_ACTIONS.has(action) && !canExecutePendingPlan())
    ) {
      button.disabled = true;
    }
  }
  if (primary) button.classList.add('is-primary');
  return button;
}

function buildFeedbackEditor(): HTMLElement {
  const editor = document.createElement('div');
  editor.className = 'plan-feedback-editor';
  editor.hidden = !feedbackOpen;
  const input = document.createElement('textarea');
  input.rows = 3;
  input.dataset.planFeedback = 'true';
  input.placeholder = tr('plan.feedbackPlaceholder');
  input.setAttribute('aria-label', tr('plan.feedbackPlaceholder'));
  const actions = document.createElement('div');
  actions.className = 'plan-feedback-actions';
  actions.append(
    actionButton('plan-submit-feedback', tr('plan.submitRevision'), true),
    actionButton('plan-toggle-feedback', tr('common.cancel')),
  );
  editor.append(input, actions);
  return editor;
}

function buildActions(plan: PlanStatePayload): HTMLElement {
  const actions = document.createElement('div');
  actions.className = 'plan-card-actions';
  if (plan.status === 'ready') {
    const execute = actionButton('execute-plan', tr('plan.execute'), true);
    execute.disabled = execute.disabled || !canExecutePendingPlan();
    execute.dataset.planId = plan.plan_id;
    actions.append(
      execute,
      actionButton('plan-toggle-feedback', tr('plan.revise')),
      actionButton('plan-discard', tr('plan.discard')),
      actionButton('plan-copy', tr('common.copy')),
    );
  } else if (plan.status === 'needs_input') {
    actions.append(
      actionButton('plan-submit-feedback', tr('plan.answerAndRevise'), true),
      actionButton('plan-discard', tr('plan.discard')),
      actionButton('plan-copy', tr('common.copy')),
    );
  } else if (plan.status === 'failed' || plan.status === 'stopped') {
    const wasApprovedExecution = isApprovedExecution(plan);
    if (wasApprovedExecution) {
      actions.append(actionButton('plan-resume', tr('plan.resume'), true));
    }
    actions.append(actionButton('plan-toggle-feedback', tr('plan.revise')));
    if (!wasApprovedExecution) {
      actions.append(actionButton('plan-discard', tr('plan.discard')));
    }
    actions.append(actionButton('plan-copy', tr('common.copy')));
  } else if (plan.status === 'completed' || plan.status === 'discarded') {
    actions.append(actionButton('plan-copy', tr('common.copy')));
  }
  return actions;
}

function buildStaleNotice(): HTMLElement | null {
  const evidenceIncomplete = Boolean(
    state.planStaleConfirmationToken && state.activePlan?.evidence_truncated,
  );
  if (!state.planStalePaths.length && !evidenceIncomplete) return null;
  const notice = document.createElement('div');
  notice.className = 'plan-stale-notice';
  const text = document.createElement('p');
  text.textContent = evidenceIncomplete
    ? tr('plan.evidenceIncomplete')
    : tr('plan.staleDescription', { count: state.planStalePaths.length });
  const actions = document.createElement('div');
  actions.append(
    actionButton('plan-refresh', tr('plan.refresh'), true),
    actionButton('plan-execute-stale', tr('plan.executeAnyway')),
  );
  notice.appendChild(text);
  if (state.planStalePaths.length) {
    const paths = document.createElement('code');
    paths.textContent = state.planStalePaths.slice(0, 6).join(', ');
    notice.appendChild(paths);
  }
  notice.appendChild(actions);
  return notice;
}

function buildPlanCard(plan: PlanStatePayload, historical = false): HTMLElement {
  const card = document.createElement('article');
  card.className = `plan-artifact-card is-${plan.status}`;
  if (historical) card.classList.add('is-historical');
  card.dataset.historical = String(historical);
  card.dataset.planId = plan.plan_id;
  card.dataset.planRevision = String(plan.revision);
  card.tabIndex = -1;
  const displayTitle = displayPlanTitle(plan);
  card.setAttribute('aria-label', tr('plan.cardLabel', { title: displayTitle }));
  if (!historical && planActionInFlight) card.setAttribute('aria-busy', 'true');

  const header = document.createElement('header');
  const headingGroup = document.createElement('div');
  const eyebrow = document.createElement('span');
  eyebrow.className = 'plan-card-eyebrow';
  eyebrow.textContent = `${tr('plan.title')} · v${plan.revision}`;
  const title = document.createElement('h3');
  title.textContent = displayTitle;
  headingGroup.append(eyebrow, title);
  const status = document.createElement('span');
  status.className = 'plan-card-status';
  status.textContent = historical ? tr('plan.previousRevision') : statusLabel(plan.status);
  header.append(headingGroup, status);
  card.appendChild(header);

  const displayGoal = displayPlanGoal(plan);
  if (displayGoal) {
    const goal = document.createElement('p');
    goal.className = 'plan-card-goal';
    goal.textContent = displayGoal;
    card.appendChild(goal);
  }
  if (plan.artifact.summary) {
    const summary = document.createElement('p');
    summary.className = 'plan-card-summary';
    summary.textContent = plan.artifact.summary;
    card.appendChild(summary);
  }
  if (plan.run_finished_with_unreported_steps) {
    const warning = document.createElement('p');
    warning.className = 'plan-unreported-warning';
    warning.textContent = tr('plan.unreportedSteps', {
      count: plan.unfinished_steps || 0,
    });
    card.appendChild(warning);
  }
  if (plan.artifact.steps?.length) card.appendChild(buildPlanSteps(plan));
  appendTextSection(card, tr('plan.assumptions'), plan.artifact.assumptions);
  appendTextSection(card, tr('plan.risks'), plan.artifact.risks, 'is-risk');
  appendTextSection(card, tr('plan.acceptance'), plan.artifact.acceptance_criteria);
  appendTextSection(card, tr('plan.verification'), plan.artifact.verification);
  const questions = buildQuestions(plan, historical);
  if (questions) card.appendChild(questions);
  if (!historical) {
    const stale = buildStaleNotice();
    if (stale) card.appendChild(stale);
    card.appendChild(buildActions(plan));
    if (['ready', 'needs_input', 'failed', 'stopped'].includes(plan.status)) {
      card.appendChild(buildFeedbackEditor());
    }
  }
  return card;
}

function renderPlanProgress(plan: PlanStatePayload): void {
  if (!dom.planProgress) return;
  if (plan.status !== 'executing') {
    dom.planProgress.hidden = true;
    dom.planProgress.replaceChildren();
    return;
  }
  const total = plan.progress.length;
  const complete = plan.progress.filter(
    (step) => step.status === 'completed' || step.status === 'skipped',
  ).length;
  const ratio = total ? Math.round((complete / total) * 100) : 0;
  const label = document.createElement('span');
  label.textContent = tr('plan.progress', { complete, total });
  const track = document.createElement('span');
  track.className = 'plan-progress-track';
  const value = document.createElement('span');
  value.style.width = `${ratio}%`;
  track.appendChild(value);
  const jump = actionButton('plan-jump', tr('plan.jump'));
  dom.planProgress.replaceChildren(label, track, jump);
  dom.planProgress.hidden = false;
}

function planRevisionKey(plan: PlanStatePayload): string {
  return `${plan.plan_id}:${plan.revision}`;
}

function mountPlanCard(plan: PlanStatePayload, historical: boolean): HTMLElement | null {
  const card = buildPlanCard(plan, historical);
  if (!historical) restorePlanFormDraft(card, plan);
  const content = assistantContentForPlan(plan.message_index);
  const deferred = state.deferredHistory.some(
    (message) =>
      message?.role === 'assistant' && Number(message.message_index) === plan.message_index,
  );
  // A plan whose Assistant anchor is still in deferredHistory must remain
  // deferred as well. Appending the current terminal plan at the bottom would
  // move an older plan past all of the newer visible conversation.
  if (!content && deferred) return null;
  let mounted: HTMLElement = card;
  if (historical) {
    const details = document.createElement('details');
    details.className = 'plan-revision-history';
    const summary = document.createElement('summary');
    summary.textContent = `${tr('plan.previousRevision')} · v${plan.revision} · ${displayPlanTitle(plan)}`;
    details.append(summary, card);
    mounted = details;
  }
  if (content) {
    const row = content.closest('.msg-row');
    row?.classList.add('plan-artifact-message');
    const markdown = content.querySelector<HTMLElement>('.msg');
    if (markdown) markdown.hidden = true;
    content.insertBefore(mounted, content.querySelector('.msg-time'));
  } else {
    const row = document.createElement('div');
    row.className = 'msg-row plan-artifact-row';
    row.dataset.historical = String(historical);
    row.appendChild(mounted);
    dom.chat.appendChild(row);
  }
  return card;
}

function renderPlanCollection(
  plan: PlanStatePayload,
  focusedAction?: string,
  followScroll = true,
): void {
  restoreHiddenPlanMessages();
  document
    .querySelectorAll('.plan-execute-action, .plan-artifact-row')
    .forEach((node) => node.remove());
  const historicalCards = state.planHistory
    .slice()
    .sort(
      (left, right) => left.message_index - right.message_index || left.revision - right.revision,
    )
    .map((revision) => mountPlanCard(revision, true))
    .filter((card): card is HTMLElement => Boolean(card));
  const card = mountPlanCard(plan, false);
  const currentRow = card?.closest('.msg-row');
  if (currentRow?.parentElement === dom.chat) {
    historicalCards.forEach((historicalCard) => {
      const historicalRow = historicalCard.closest('.plan-artifact-row');
      if (historicalRow) dom.chat.insertBefore(historicalRow, currentRow);
    });
  }
  renderPlanProgress(plan);
  syncComposerAvailability();
  invalidateChatScrollCache();
  if (focusedAction && card) {
    card.querySelector<HTMLElement>(`[data-action="${focusedAction}"]`)?.focus();
  }
  if (followScroll) scrollDown();
}

export function refreshPlanMounts(): void {
  if (state.activePlan) {
    captureCurrentPlanDraft();
    renderPlanCollection(state.activePlan, undefined, false);
  }
}

export function renderPlanState(plan: PlanStatePayload, acknowledgeAction = true): void {
  if (!plan?.plan_id || !dom.chat) return;
  if (acknowledgeAction) setPlanActionInFlight(false);
  captureCurrentPlanDraft();
  const focusedAction =
    document.activeElement instanceof HTMLElement
      ? document.activeElement.closest<HTMLElement>('[data-action]')?.dataset.action
      : undefined;
  const previous = state.activePlan;
  if (
    previous?.status === 'planning' &&
    (plan.status === 'ready' || plan.status === 'needs_input')
  ) {
    discardAssistantStream();
  }
  const revisionChanged = Boolean(previous && planRevisionKey(previous) !== planRevisionKey(plan));
  if (previous && revisionChanged) {
    const previousRevision = { ...previous, historical: true };
    const key = planRevisionKey(previousRevision);
    planFormDrafts.delete(planDraftKey(previous));
    state.planHistory = [
      ...state.planHistory.filter((entry) => planRevisionKey(entry) !== key),
      previousRevision,
    ];
  }
  state.planHistory = state.planHistory.filter(
    (entry) => planRevisionKey(entry) !== planRevisionKey(plan),
  );
  state.activePlan = { ...plan, historical: false };
  activePlanSessionId = state.activeSessionId || 'main';
  state.pendingPlanId = plan.plan_id;
  state.pendingPlanMessageIndex = Number.isFinite(plan.message_index) ? plan.message_index : null;
  state.pendingPlanExecutionId = plan.status === 'executing' ? plan.plan_id : '';
  if (revisionChanged || !['ready', 'failed', 'stopped'].includes(plan.status)) {
    state.planStalePaths = [];
    state.planStaleConfirmationToken = '';
  }
  feedbackOpen = ['ready', 'needs_input', 'failed', 'stopped'].includes(plan.status)
    ? (planFormDrafts.get(planDraftKey(state.activePlan))?.feedbackOpen ??
      Boolean(plan.pending_feedback))
    : false;
  setSessionPlanModeForStatus(plan.status);
  renderPlanCollection(state.activePlan, focusedAction);
}

export function renderPlanHistory(plans: PlanStatePayload[]): void {
  captureCurrentPlanDraft();
  const valid = plans.filter((plan) => plan?.plan_id && Number.isFinite(plan.revision));
  if (!valid.length) return;
  setPlanActionInFlight(false);
  const current =
    valid
      .slice()
      .reverse()
      .find((plan) => !plan.historical) || valid[valid.length - 1];
  state.planHistory = valid
    .filter((plan) => plan !== current)
    .map((plan) => ({ ...plan, historical: true }));
  state.activePlan = { ...current, historical: false };
  activePlanSessionId = state.activeSessionId || 'main';
  state.pendingPlanId = current.plan_id;
  state.pendingPlanMessageIndex = Number.isFinite(current.message_index)
    ? current.message_index
    : null;
  state.pendingPlanExecutionId = current.status === 'executing' ? current.plan_id : '';
  state.planStalePaths = [];
  state.planStaleConfirmationToken = '';
  feedbackOpen = ['ready', 'needs_input', 'failed', 'stopped'].includes(current.status)
    ? (planFormDrafts.get(planDraftKey(state.activePlan))?.feedbackOpen ??
      Boolean(current.pending_feedback))
    : false;
  setSessionPlanModeForStatus(current.status);
  renderPlanCollection(state.activePlan);
}

export function renderPendingPlanAction(plan: PlanReadyPayload | null | undefined): void {
  if (!plan?.plan_id) return;
  if (state.activePlan?.plan_id === plan.plan_id) {
    renderPlanState(state.activePlan);
    return;
  }
  state.pendingPlanId = plan.plan_id;
  state.pendingPlanMessageIndex =
    typeof plan.message_index === 'number' ? plan.message_index : null;
  const fallback: PlanStatePayload = {
    plan_id: plan.plan_id,
    revision: plan.revision || 1,
    status: 'ready',
    message_index: plan.message_index,
    created_at: plan.created_at,
    updated_at: plan.created_at,
    artifact: {
      title: tr('plan.title'),
      goal: tr('plan.legacyGoal'),
      steps: [{ id: 'legacy-plan', title: tr('plan.legacyStep') }],
    },
    progress: [{ id: 'legacy-plan', title: tr('plan.legacyStep'), status: 'pending' }],
  };
  renderPlanState(fallback);
}

function sendPlanRequest(payload: Record<string, unknown>): boolean {
  if (
    planActionInFlight ||
    state.busy ||
    state.storageMode === 'protected' ||
    planIdentityTransitionInFlight() ||
    !activePlanTargetsCurrentSession() ||
    !state.ws ||
    state.ws.readyState !== WebSocket.OPEN
  )
    return false;
  state.ws.send(JSON.stringify(payload));
  setPlanActionInFlight(true);
  return true;
}

function sendPlanAction(
  action: 'feedback' | 'execute' | 'refresh' | 'discard' | 'resume',
  extra: Record<string, unknown> = {},
): boolean {
  const plan = state.activePlan;
  if (!plan) return false;
  return sendPlanRequest({
    plan_action: {
      action,
      plan_id: plan.plan_id,
      revision: plan.revision,
      ...extra,
    },
  });
}

export function executePendingPlan(button: HTMLButtonElement | null | undefined): void {
  const planId = button?.dataset?.planId || state.pendingPlanId;
  if (
    !planId ||
    !canExecutePendingPlan() ||
    state.busy ||
    !state.ws ||
    state.ws.readyState !== WebSocket.OPEN
  )
    return;
  const sent = state.activePlan
    ? sendPlanAction('execute')
    : sendPlanRequest({ execute_plan_id: planId });
  if (!sent) return;
  if (button) button.disabled = true;
  state.pendingPlanExecutionId = planId;
  setPlanMode(false);
  setBusy(true);
}

export function executeStalePlan(): void {
  if (!canExecutePendingPlan() || state.busy || !state.planStaleConfirmationToken) return;
  const action =
    state.activePlan &&
    (state.activePlan.status === 'failed' || state.activePlan.status === 'stopped') &&
    isApprovedExecution(state.activePlan)
      ? 'resume'
      : 'execute';
  if (
    sendPlanAction(action, {
      allow_stale: true,
      stale_confirmation_token: state.planStaleConfirmationToken,
    })
  ) {
    state.planStalePaths = [];
    state.planStaleConfirmationToken = '';
    setPlanMode(false);
    setBusy(true);
  }
}

export function discardPlan(): void {
  if (sendPlanAction('discard')) {
    if (state.activePlan) planFormDrafts.delete(planDraftKey(state.activePlan));
    feedbackOpen = false;
    setPlanMode(false);
  }
}

export function refreshPlan(): void {
  if (!canExecutePendingPlan() || state.busy) return;
  if (sendPlanAction('refresh')) {
    state.planStalePaths = [];
    state.planStaleConfirmationToken = '';
    setPlanMode(true);
    setBusy(true);
  }
}

export function resumePlan(): void {
  if (!canExecutePendingPlan() || state.busy) return;
  if (sendPlanAction('resume')) {
    setPlanMode(false);
    setBusy(true);
  }
}

export function togglePlanFeedback(_button?: HTMLElement | null): void {
  feedbackOpen = !feedbackOpen;
  if (state.activePlan) renderPlanState(state.activePlan, false);
  if (feedbackOpen) {
    const card = document.querySelector('.plan-artifact-card:not([data-historical="true"])');
    card?.querySelector<HTMLTextAreaElement>('[data-plan-feedback]')?.focus();
  }
}

export function submitPlanFeedback(button: HTMLElement | null | undefined): void {
  const card = button?.closest<HTMLElement>('.plan-artifact-card');
  const plan = state.activePlan;
  if (!card || !plan || state.busy || !canExecutePendingPlan()) return;
  const answers: Record<string, string> = {};
  for (const question of plan.artifact.questions || []) {
    const selected = Array.from(
      card.querySelectorAll<HTMLInputElement>('input[type="radio"]:checked'),
    ).find((input) => input.name === planQuestionGroupName(plan, question.id));
    const freeText = Array.from(
      card.querySelectorAll<HTMLTextAreaElement>('textarea[data-plan-question]'),
    ).find((input) => input.dataset.planQuestion === question.id);
    const answer = freeText?.value.trim() || selected?.dataset.answerLabel || '';
    if (answer) answers[question.id] = answer;
  }
  const text = card.querySelector<HTMLTextAreaElement>('[data-plan-feedback]')?.value.trim() || '';
  if (!text && Object.keys(answers).length === 0) {
    card.querySelector<HTMLTextAreaElement>('[data-plan-feedback]')?.focus();
    return;
  }
  if (sendPlanAction('feedback', { text: text || undefined, answers })) {
    feedbackOpen = false;
    setPlanMode(true);
    setBusy(true);
  }
}

export async function copyPlan(): Promise<void> {
  const plan = state.activePlan;
  if (!plan) return;
  const legacy = plan.artifact.legacy_markdown?.trim();
  if (legacy) {
    await navigator.clipboard?.writeText(legacy);
    return;
  }
  const lines = [`# ${displayPlanTitle(plan)}`, '', displayPlanGoal(plan)];
  if (plan.artifact.summary) lines.push('', plan.artifact.summary);
  const artifactSteps = plan.artifact.steps ?? [];
  if (artifactSteps.length) {
    lines.push('', `## ${tr('plan.steps')}`);
    artifactSteps.forEach((step, index) => {
      lines.push(`${index + 1}. ${step.title}${step.description ? ` — ${step.description}` : ''}`);
      if (step.affected_areas?.length) lines.push(`   - ${step.affected_areas.join(', ')}`);
    });
  }
  const appendSection = (title: string, values: string[] | undefined): void => {
    if (!values?.length) return;
    lines.push('', `## ${title}`, ...values.map((value) => `- ${value}`));
  };
  appendSection(tr('plan.assumptions'), plan.artifact.assumptions);
  appendSection(tr('plan.risks'), plan.artifact.risks);
  appendSection(tr('plan.acceptance'), plan.artifact.acceptance_criteria);
  appendSection(tr('plan.verification'), plan.artifact.verification);
  if (plan.artifact.questions?.length) {
    lines.push('', `## ${tr('plan.questions')}`);
    plan.artifact.questions.forEach((question) => {
      lines.push(`- ${question.prompt}`);
      question.options?.forEach((option) => {
        lines.push(`  - ${option.label}${option.description ? ` — ${option.description}` : ''}`);
      });
    });
  }
  await navigator.clipboard?.writeText(lines.join('\n'));
}

export function jumpToPlan(): void {
  const card = document.querySelector<HTMLElement>(
    '.plan-artifact-card:not([data-historical="true"])',
  );
  card?.scrollIntoView({ behavior: 'smooth', block: 'center' });
  card?.focus({ preventScroll: true });
}

export function handlePlanStale(payload: {
  plan_id?: string;
  revision?: number;
  paths?: string[];
  evidence_incomplete?: boolean;
  confirmation_token?: string;
}): void {
  if (
    !state.activePlan ||
    payload.plan_id !== state.activePlan.plan_id ||
    payload.revision !== state.activePlan.revision
  )
    return;
  setPlanActionInFlight(false);
  state.pendingPlanExecutionId = '';
  state.planStalePaths = Array.isArray(payload.paths) ? payload.paths.filter(Boolean) : [];
  state.planStaleConfirmationToken = String(payload.confirmation_token || '');
  if (payload.evidence_incomplete) state.activePlan.evidence_truncated = true;
  setBusy(false);
  renderPlanState(state.activePlan, false);
}

export function handlePlanRevisionConflict(plan: PlanStatePayload | null | undefined): void {
  if (!plan?.plan_id || !Number.isFinite(plan.revision)) return;
  state.pendingPlanExecutionId = '';
  state.planStalePaths = [];
  state.planStaleConfirmationToken = '';
  renderPlanState(plan);
}

export function refreshPlanLanguage(): void {
  if (state.activePlan) renderPlanState(state.activePlan, false);
}
