import { iconMarkup, type IconName } from '../icons.js';
import { tr } from '../i18n.js';
import { invalidateChatScrollCache } from '../scroll.js';
import { dom, state } from '../state.js';
import type { PlanStatePayload } from '../types.js';
import { formatToolDuration } from '../utils.js';
import {
  normalizeRunDiagnostic,
  runDiagnosticDetail,
  runDiagnosticHasModelsRecovery,
  type RunDiagnostic,
} from '../runDiagnostics.js';

export type ExecutionStepType =
  | 'reasoning'
  | 'tool'
  | 'result'
  | 'task-plan'
  | 'subagent'
  | 'orchestrate'
  | 'react';

export type ExecutionStatus =
  | 'running'
  | 'completed'
  | 'failed'
  | 'blocked'
  | 'waiting_user'
  | 'partial'
  | 'stopped'
  | 'incomplete'
  | 'discarded';

type CompleteExecutionStackOptions = {
  durationMs?: number | null;
  failed?: boolean;
  immediate?: boolean;
  mergeWithExisting?: boolean;
  stack?: HTMLElement | null;
  status?: Exclude<ExecutionStatus, 'running'>;
  summary?: string;
  summaryKey?: string;
  summaryParams?: Record<string, string | number>;
  recoveryLabel?: string;
  recoveryLabelKey?: string;
  recoveryLabelParams?: Record<string, string | number>;
  recoveryTarget?: HTMLElement | null;
  recoveryPlanId?: string;
  recoveryPlanRevision?: number;
  diagnostic?: RunDiagnostic;
  terminalSource?: ExecutionTerminalSource;
};

type ExecutionTerminalSource = 'generic' | 'history' | 'error' | 'done' | 'plan';

export type DoneExecutionOutcome = Pick<
  CompleteExecutionStackOptions,
  | 'status'
  | 'summary'
  | 'summaryKey'
  | 'summaryParams'
  | 'recoveryLabel'
  | 'recoveryLabelKey'
  | 'recoveryLabelParams'
>;

interface StepDescriptor {
  action: string;
  object: string;
  result: string;
  state: string;
  type: string;
  panel: HTMLElement | null;
  retryKey: string;
  step: HTMLElement;
}

let executionStackId = 0;
const collapseTimers = new WeakMap<HTMLElement, number>();
type RecoveryHint = {
  target?: HTMLElement | null;
  planId?: string;
  planRevision?: number;
};

const recoveryHints = new WeakMap<HTMLElement, RecoveryHint>();

function getBody(stack: HTMLElement): HTMLElement | null {
  return stack.querySelector<HTMLElement>('.execution-stack-body');
}

function getSteps(stack: HTMLElement): HTMLElement[] {
  return Array.from(
    stack.querySelectorAll<HTMLElement>(':scope > .execution-stack-body > .execution-step'),
  );
}

function stepIsVisible(step: HTMLElement): boolean {
  const type = step.dataset.executionStep || '';
  if (type === 'react') return true;
  return type === 'reasoning' ? state.showReasoning : state.showTools;
}

function stepHasFailure(step: HTMLElement): boolean {
  return Boolean(
    step.querySelector(
      '.tool-panel-failed, .subagent-failed, .orchestrate-aborted, .orchestrate-task-failed',
    ),
  );
}

function descriptorState(panel: HTMLElement | null, step: HTMLElement): string {
  if (panel?.dataset.executionState) return panel.dataset.executionState;
  if (stepHasFailure(step)) return 'failed';
  if (
    panel?.classList.contains('tool-panel-ready') ||
    panel?.classList.contains('subagent-done') ||
    panel?.classList.contains('orchestrate-done') ||
    panel?.classList.contains('task-plan-complete')
  ) {
    return 'completed';
  }
  if (panel?.classList.contains('subagent-skipped')) return 'skipped';
  return 'running';
}

function stepDescriptor(step: HTMLElement): StepDescriptor {
  const panel = step.firstElementChild as HTMLElement | null;
  const type = step.dataset.executionStep || '';
  const fallbackAction =
    panel?.dataset.toolName ||
    panel?.querySelector<HTMLElement>(
      '.tool-name, .subagent-label, .orchestrate-label, .reasoning-label, .react-status-phase',
    )?.textContent ||
    '';
  return {
    action: panel?.dataset.executionAction || fallbackAction.trim(),
    object:
      panel?.dataset.executionObject ||
      panel?.querySelector<HTMLElement>('.tool-args-preview')?.textContent?.trim() ||
      '',
    result:
      panel?.dataset.executionResult ||
      panel
        ?.querySelector<HTMLElement>(
          '.tool-status, .subagent-status, .orchestrate-status, .reasoning-status, .react-status-detail',
        )
        ?.textContent?.trim() ||
      '',
    state: descriptorState(panel, step),
    type,
    panel,
    retryKey: panel?.dataset.executionRetryKey || '',
    step,
  };
}

function markRecoveredRetries(descriptors: StepDescriptor[]): number {
  const laterSuccess = new Set<string>();
  let recovered = 0;
  for (let index = descriptors.length - 1; index >= 0; index -= 1) {
    const descriptor = descriptors[index];
    const key = descriptor.retryKey;
    const isRecovered = Boolean(key && descriptor.state === 'failed' && laterSuccess.has(key));
    descriptor.panel?.classList.toggle('execution-step-recovered', isRecovered);
    if (descriptor.panel) descriptor.panel.dataset.executionRecovered = String(isRecovered);
    if (isRecovered) {
      descriptor.state = 'recovered';
      recovered += 1;
    }
    if (key && descriptor.state === 'completed') laterSuccess.add(key);
  }
  return recovered;
}

export function linkExecutionDelegate(panel: HTMLElement, toolCallId?: string): void {
  if (!toolCallId) return;
  panel.dataset.executionDelegateToolId = toolCallId;
  const stack = panel.closest<HTMLElement>('.execution-stack');
  const tool = Array.from(stack?.querySelectorAll<HTMLElement>('.tool-panel') || []).find(
    (candidate) => candidate.dataset.toolId === toolCallId,
  );
  if (tool) panel.dataset.executionRetryKey = tool.dataset.executionRetryKey || '';
}

function stackDescriptors(stack: HTMLElement): StepDescriptor[] {
  const descriptors = getSteps(stack)
    .filter((step) => step.dataset.executionStep !== 'react')
    .map(stepDescriptor);
  // A delegated Tool inspector and its richer task panel describe the same
  // call. Keep both inspectable, but count the work only once.
  return descriptors.filter(
    (descriptor) =>
      !descriptors.some(
        (delegate) =>
          descriptor.type === 'tool' &&
          descriptor.panel?.dataset.toolId &&
          delegate.panel?.dataset.executionDelegateToolId === descriptor.panel.dataset.toolId &&
          delegate.state === descriptor.state,
      ),
  );
}

function descriptorProgress(descriptor: StepDescriptor): { completed: number; total: number } {
  if (descriptor.type === 'orchestrate') {
    const total = Number(descriptor.panel?.dataset.taskCount || 0);
    if (total > 0)
      return {
        total,
        completed:
          descriptor.state === 'recovered'
            ? total
            : Number(descriptor.panel?.dataset.completedCount || 0),
      };
  }
  return {
    total: 1,
    completed: ['completed', 'skipped', 'recovered'].includes(descriptor.state) ? 1 : 0,
  };
}

function descriptorLabel(descriptor: StepDescriptor): string {
  return [descriptor.action, descriptor.object].filter(Boolean).join(' · ');
}

function stepCountText(count: number): string {
  return tr(count === 1 ? 'execution.stepCountOne' : 'execution.stepCount', { count });
}

function stackStatus(stack: HTMLElement): ExecutionStatus {
  const raw = stack.dataset.executionStatus;
  if (
    raw === 'completed' ||
    raw === 'failed' ||
    raw === 'blocked' ||
    raw === 'waiting_user' ||
    raw === 'partial' ||
    raw === 'stopped' ||
    raw === 'incomplete' ||
    raw === 'discarded'
  ) {
    return raw;
  }
  return 'running';
}

function terminalSource(stack: HTMLElement): ExecutionTerminalSource {
  const source = stack.dataset.executionTerminalSource;
  return source === 'history' || source === 'error' || source === 'done' || source === 'plan'
    ? source
    : 'generic';
}

// A Plan state carries the most specific domain outcome. A final `done` event
// is authoritative for an ordinary run, while a preceding `error` is only an
// interim detail. Lower-ranked events may enrich the timeline but must not
// replace a higher-ranked status, summary, or recovery target.
const terminalSourcePriority: Record<ExecutionTerminalSource, number> = {
  generic: 0,
  history: 1,
  error: 2,
  done: 3,
  plan: 4,
};

function statusTitle(status: ExecutionStatus, action: string): string {
  const fallback =
    status === 'running'
      ? tr('execution.working')
      : status === 'completed'
        ? tr('execution.worked')
        : status === 'failed'
          ? tr('execution.failed')
          : status === 'blocked'
            ? tr('execution.blocked')
            : status === 'waiting_user'
              ? tr('execution.waitingUser')
              : status === 'partial'
                ? tr('execution.partial')
                : status === 'stopped'
                  ? tr('execution.stopped')
                  : status === 'discarded'
                    ? tr('execution.discarded')
                    : tr('execution.incomplete');
  return action ? tr(`execution.statusAction.${status}`, { action }) : fallback;
}

function statusIconName(status: ExecutionStatus, hasFailure: boolean): IconName {
  if (status === 'failed' || hasFailure) return 'alert-triangle';
  if (status === 'blocked' || status === 'waiting_user' || status === 'stopped')
    return 'circle-dot';
  if (status === 'partial' || status === 'incomplete') return 'info';
  if (status === 'discarded') return 'circle-dot';
  return status === 'completed' ? 'check' : 'activity';
}

export function doneExecutionOutcome(
  phaseValue: unknown,
  reasonValue: unknown,
): DoneExecutionOutcome {
  const phase = String(phaseValue ?? '')
    .trim()
    .toLowerCase();
  const reason = String(reasonValue ?? '')
    .trim()
    .toLowerCase();

  if (
    phase === 'stopped' ||
    ['user_stop', 'stopped', 'cancelled', 'canceled', 'shutdown'].includes(reason)
  ) {
    return {
      status: 'stopped',
      summary: tr('execution.stoppedSummary'),
      summaryKey: 'execution.stoppedSummary',
      recoveryLabel: tr('execution.reviewInterrupted'),
      recoveryLabelKey: 'execution.reviewInterrupted',
    };
  }
  if (
    ['hard_cap', 'incomplete'].includes(phase) ||
    ['hard_cap', 'incomplete_plan', 'empty', 'empty_response', 'incomplete'].includes(reason)
  ) {
    return {
      status: 'incomplete',
      summary:
        reason === 'hard_cap' || phase === 'hard_cap'
          ? tr('execution.hardCapSummary')
          : tr('execution.incompleteSummary'),
      summaryKey:
        reason === 'hard_cap' || phase === 'hard_cap'
          ? 'execution.hardCapSummary'
          : 'execution.incompleteSummary',
      recoveryLabel: tr('execution.reviewIncomplete'),
      recoveryLabelKey: 'execution.reviewIncomplete',
    };
  }
  if (phase === 'failed' || ['failed', 'completion_contract_failed'].includes(reason)) {
    return {
      status: 'failed',
      summary:
        reason === 'completion_contract_failed'
          ? tr('execution.completionContractFailedSummary')
          : tr('execution.failedSummary'),
      summaryKey:
        reason === 'completion_contract_failed'
          ? 'execution.completionContractFailedSummary'
          : 'execution.failedSummary',
      recoveryLabel: tr('execution.reviewError'),
      recoveryLabelKey: 'execution.reviewError',
    };
  }
  if (phase === 'blocked' || reason === 'blocked') {
    return {
      status: 'blocked',
      summary: tr('execution.blockedSummary'),
      summaryKey: 'execution.blockedSummary',
      recoveryLabel: tr('execution.reviewDetails'),
      recoveryLabelKey: 'execution.reviewDetails',
    };
  }
  if (phase === 'waiting_user' || ['waiting_user', 'needs_input'].includes(reason)) {
    return {
      status: 'waiting_user',
      summary: tr('execution.waiting_userSummary'),
      summaryKey: 'execution.waiting_userSummary',
      recoveryLabel: tr('execution.reviewDetails'),
      recoveryLabelKey: 'execution.reviewDetails',
    };
  }
  if (phase === 'finish' && reason === 'complete') {
    return { status: 'completed' };
  }
  if (phase === 'partial' || reason === 'partial') {
    return {
      status: 'partial',
      summary: tr('execution.partialSummary'),
      summaryKey: 'execution.partialSummary',
      recoveryLabel: tr('execution.reviewDetails'),
      recoveryLabelKey: 'execution.reviewDetails',
    };
  }
  return {
    status: 'partial',
    summary: tr('execution.unknownTerminalSummary'),
    summaryKey: 'execution.unknownTerminalSummary',
    recoveryLabel: tr('execution.reviewDetails'),
    recoveryLabelKey: 'execution.reviewDetails',
  };
}

function isVerificationDescriptor(descriptor: StepDescriptor): boolean {
  const value = `${descriptor.action} ${descriptor.panel?.dataset.toolName || ''}`.toLowerCase();
  return /(?:test|check|verify|lint|clippy|build|validate)/.test(value);
}

function isArtifactDescriptor(descriptor: StepDescriptor): boolean {
  const value = `${descriptor.action} ${descriptor.panel?.dataset.toolName || ''}`.toLowerCase();
  return /(?:write|create|patch|edit|generate|save)/.test(value);
}

function localizedStackDataset(
  stack: HTMLElement,
  keyName: 'executionOutcome' | 'executionRecovery',
  fallback: string,
): string {
  const key = stack.dataset[`${keyName}Key`];
  if (!key) return fallback;
  let params: Record<string, string | number> | undefined;
  const rawParams = stack.dataset[`${keyName}Params`];
  if (rawParams) {
    try {
      params = JSON.parse(rawParams) as Record<string, string | number>;
    } catch {
      params = undefined;
    }
  }
  return tr(key, params);
}

function syncRecoveryPresentation(stack: HTMLElement, status: ExecutionStatus): void {
  const recovery = stack.querySelector<HTMLElement>('.execution-stack-recovery');
  const summary = recovery?.querySelector<HTMLElement>('.execution-stack-recovery-summary');
  const button = recovery?.querySelector<HTMLButtonElement>('.execution-stack-recovery-action');
  const needsAttention =
    status === 'failed' ||
    status === 'blocked' ||
    status === 'waiting_user' ||
    status === 'partial' ||
    status === 'stopped' ||
    status === 'incomplete';
  if (!recovery || !summary || !button) return;
  recovery.hidden = !needsAttention;
  if (!needsAttention) {
    summary.textContent = '';
    button.textContent = '';
    return;
  }
  summary.textContent = tr('execution.recoveryPrompt');
  button.textContent = localizedStackDataset(
    stack,
    'executionRecovery',
    stack.dataset.executionRecoveryLabel || tr('execution.reviewDetails'),
  );
  const hint = recoveryHints.get(stack);
  if (
    hint?.planId &&
    hint.planRevision != null &&
    (state.activePlan?.plan_id !== hint.planId || state.activePlan.revision !== hint.planRevision)
  )
    button.textContent = tr('execution.reviewDetails');
}

function syncStackSummary(stack: HTMLElement): void {
  const steps = getSteps(stack);
  const visibleSteps = steps.filter((step) => !step.hidden);
  const allDescriptors = stackDescriptors(stack);
  const recoveredCount = markRecoveredRetries(allDescriptors);
  const descriptors = allDescriptors.filter((descriptor) => !descriptor.step.hidden);
  const actionable = descriptors.filter((descriptor) => descriptor.type !== 'reasoning');
  const candidates = actionable.length ? actionable : descriptors;
  const running = candidates.filter((descriptor) => descriptor.state === 'running');
  const failures = allDescriptors.filter((descriptor) => descriptor.state === 'failed');
  const currentRunning = running.filter((descriptor) => descriptorLabel(descriptor)).at(-1);
  const failurePoint = failures.at(-1);
  const status = stackStatus(stack);
  const hasFailure = failures.length > 0 || stack.dataset.executionFailed === 'true';
  const presentationStatus =
    status === 'running' && hasFailure && !currentRunning ? 'failed' : status;
  const current =
    status === 'running'
      ? currentRunning || failurePoint || candidates.at(-1)
      : failurePoint || candidates.at(-1);
  const title = stack.querySelector<HTMLElement>('.execution-stack-title');
  const summary = stack.querySelector<HTMLElement>('.execution-stack-summary');
  const meta = stack.querySelector<HTMLElement>('.execution-stack-meta');
  const statusIcon = stack.querySelector<HTMLElement>('.execution-stack-status-icon');
  const duration = Number(stack.dataset.executionDuration || '');
  const progress = candidates.map(descriptorProgress);
  const completedCount = progress.reduce((sum, item) => sum + item.completed, 0);
  const totalCount = progress.reduce((sum, item) => sum + item.total, 0);
  const unresolvedCount = totalCount - completedCount;
  const parts = [
    candidates.length
      ? tr('execution.progressCount', { completed: completedCount, total: totalCount })
      : stepCountText(visibleSteps.length),
  ];
  const verificationCount = candidates.filter(isVerificationDescriptor).length;
  const artifactCount = candidates.filter(isArtifactDescriptor).length;
  if (verificationCount)
    parts.push(tr('execution.verificationCount', { count: verificationCount }));
  if (artifactCount) parts.push(tr('execution.artifactCount', { count: artifactCount }));
  if (recoveredCount) {
    parts.push(
      tr(recoveredCount === 1 ? 'execution.recoveredRetryOne' : 'execution.recoveredRetries', {
        count: recoveredCount,
      }),
    );
  }
  if (unresolvedCount && status !== 'running') {
    parts.push(tr('execution.unresolvedCount', { count: unresolvedCount }));
  }
  const imageCount = visibleSteps.reduce(
    (total, step) =>
      total +
      Array.from(step.querySelectorAll<HTMLElement>('[data-tool-image-count]')).reduce(
        (stepTotal, panel) => stepTotal + Number(panel.dataset.toolImageCount || 0),
        0,
      ),
    0,
  );
  if (imageCount > 0) {
    parts.push(
      tr(imageCount === 1 ? 'tool.imageCountOne' : 'tool.imageCount', { count: imageCount }),
    );
  }
  if (Number.isFinite(duration) && duration > 0) parts.push(formatToolDuration(duration));

  const action = current ? descriptorLabel(current) : '';
  if (title) title.textContent = statusTitle(presentationStatus, action);
  if (summary) {
    const liveFailureSummary =
      status === 'running' && failurePoint && currentRunning
        ? tr('execution.failurePoint', { action: descriptorLabel(failurePoint) })
        : '';
    summary.textContent =
      localizedStackDataset(
        stack,
        'executionOutcome',
        stack.dataset.executionOutcomeSummary || '',
      ) ||
      stack.dataset.executionTransientProgress ||
      [liveFailureSummary, current?.result].filter(Boolean).join(' · ') ||
      tr(`execution.${status}Summary`);
  }
  if (meta) meta.textContent = parts.join(' · ');
  if (statusIcon) statusIcon.innerHTML = iconMarkup(statusIconName(status, hasFailure));

  const header = stack.querySelector<HTMLButtonElement>('.execution-stack-header');
  if (header) {
    header.setAttribute(
      'aria-label',
      [title?.textContent, summary?.textContent, meta?.textContent].filter(Boolean).join(', '),
    );
  }
  stack.classList.toggle('is-failed', status === 'failed' || hasFailure);
  // View filters may hide the step inspectors, but they must never erase an
  // outcome that still needs attention. The compact header and recovery strip
  // remain the durable way back to the failure/blocked details.
  stack.hidden =
    visibleSteps.length === 0 &&
    !stack.dataset.executionTransientProgress &&
    (status === 'running' || status === 'completed');
  syncRecoveryPresentation(stack, status);
  syncDiagnosticPresentation(stack, status);
}

function syncDiagnosticPresentation(stack: HTMLElement, status: ExecutionStatus): void {
  const diagnostic = normalizeRunDiagnostic({ code: stack.dataset.executionDiagnosticCode });
  const details = stack.querySelector<HTMLDetailsElement>('.execution-stack-diagnostic');
  if (!details) return;
  const content = details.querySelector<HTMLElement>('.execution-stack-diagnostic-content');
  const summary = details.querySelector('summary');
  const models = details.querySelector<HTMLButtonElement>(
    '[data-action="execution-diagnostic-models"]',
  );
  const visible = Boolean(diagnostic && status !== 'completed' && status !== 'discarded');
  details.hidden = !visible;
  if (!visible) details.open = false;
  if (summary) summary.textContent = visible ? tr('execution.diagnosticDetails') : '';
  if (content) content.textContent = diagnostic && visible ? runDiagnosticDetail(diagnostic) : '';
  if (models) {
    models.hidden = !diagnostic || !visible || !runDiagnosticHasModelsRecovery(diagnostic);
    models.textContent = visible ? tr('execution.diagnosticModels') : '';
  }
}

function stackHasOpenDetail(stack: HTMLElement): boolean {
  const activeToolPanel = state.activeToolPanel;
  if (activeToolPanel?.closest('.execution-stack') === stack) return true;
  return Boolean(stack.querySelector('.subagent-modal-placeholder'));
}

function executionStackForPanel(panel: Element | null): HTMLElement | null {
  const directStack = panel?.closest<HTMLElement>('.execution-stack');
  const modalStep = panel?.closest<HTMLElement & { _modalHostPlaceholder?: HTMLElement | null }>(
    '.execution-step',
  );
  return (
    directStack ||
    modalStep?._modalHostPlaceholder?.closest<HTMLElement>('.execution-stack') ||
    null
  );
}

function setStackExpanded(stack: HTMLElement, expanded: boolean): void {
  const header = stack.querySelector<HTMLButtonElement>('.execution-stack-header');
  const body = getBody(stack);
  const restoreHeaderFocus = !expanded && Boolean(body?.contains(document.activeElement));
  stack.classList.toggle('is-expanded', expanded);
  header?.setAttribute('aria-expanded', String(expanded));
  if (body) body.hidden = !expanded;
  stack.querySelector('.execution-stack-chevron')?.classList.toggle('open', expanded);
  if (restoreHeaderFocus) header?.focus();
  invalidateChatScrollCache();
}

function createExecutionStack(before: Element | null = null): HTMLElement {
  const stack = document.createElement('section');
  const runId = ++executionStackId;
  const bodyId = `execution-stack-body-${runId}`;
  stack.className = 'execution-stack is-running is-expanded';
  stack.dataset.executionRunId = String(runId);
  stack.dataset.executionSessionId = state.activeSessionId || 'main';
  if (state.activeExecutionRunId > 0) {
    stack.dataset.executionClientRunId = String(state.activeExecutionRunId);
  }
  if (state.activeExecutionServerRunId) {
    stack.dataset.executionServerRunId = state.activeExecutionServerRunId;
  }
  if (state.activeExecutionPlanId) {
    stack.dataset.executionPlanId = state.activeExecutionPlanId;
    if (state.activePlan?.plan_id === state.activeExecutionPlanId) {
      stack.dataset.executionPlanRevision = String(state.activePlan.revision);
    }
  }
  stack.dataset.executionStatus = 'running';
  stack.dataset.executionFailed = 'false';
  stack.dataset.executionStartedAt = String(
    state.currentRoundStartedAt ||
      (typeof performance !== 'undefined' ? performance.now() : Date.now()),
  );
  stack.innerHTML = `
    <button type="button" class="execution-stack-header" data-action="toggle-execution-stack" aria-expanded="true" aria-controls="${bodyId}">
      <span class="execution-stack-status-icon">${iconMarkup('activity')}</span>
      <span class="execution-stack-copy">
        <span class="execution-stack-title"></span>
        <span class="execution-stack-summary"></span>
      </span>
      <span class="execution-stack-meta"></span>
      <span class="execution-stack-chevron">${iconMarkup('chevron-right')}</span>
    </button>
    <div class="execution-stack-recovery" hidden>
      <span class="execution-stack-recovery-summary"></span>
      <button type="button" class="execution-stack-recovery-action" data-action="execution-recovery"></button>
    </div>
    <div class="execution-stack-body" id="${bodyId}">
      <details class="execution-stack-diagnostic" hidden>
        <summary></summary>
        <p class="execution-stack-diagnostic-content" tabindex="-1"></p>
        <button type="button" class="execution-stack-diagnostic-models" data-action="execution-diagnostic-models" hidden></button>
      </details>
    </div>
    <span class="execution-stack-announcer" aria-live="polite"></span>
  `;

  if (before?.parentElement === dom.chat) dom.chat.insertBefore(stack, before);
  else dom.chat.appendChild(stack);
  state.activeExecutionStack = stack;
  invalidateChatScrollCache();
  syncStackSummary(stack);
  return stack;
}

export function ensureExecutionStack(before: Element | null = null): HTMLElement {
  const active = state.activeExecutionStack;
  if (active?.isConnected && active.dataset.executionStatus === 'running') return active;
  return createExecutionStack(before);
}

export function updateExecutionRetryProgress(attempt: number, maxAttempts: number): void {
  const stack = ensureExecutionStack();
  stack.dataset.executionTransientProgress = tr('execution.llmRetry', {
    attempt,
    max: maxAttempts,
  });
  syncStackSummary(stack);
  const announcer = stack.querySelector<HTMLElement>('.execution-stack-announcer');
  if (announcer) announcer.textContent = stack.dataset.executionTransientProgress;
}

export function clearExecutionRetryProgress(): void {
  const stack = state.activeExecutionStack;
  if (!stack?.dataset.executionTransientProgress) return;
  delete stack.dataset.executionTransientProgress;
  syncStackSummary(stack);
}

export function mountExecutionPanel(
  panel: HTMLElement,
  type: ExecutionStepType,
  before: Element | null = null,
): HTMLElement {
  const existingStep = panel.closest<HTMLElement>('.execution-step');
  if (existingStep) {
    existingStep.dataset.executionStep = type;
    refreshExecutionStackForPanel(panel);
    return existingStep;
  }

  const stack = ensureExecutionStack(before);
  const body = getBody(stack);
  const step = document.createElement('div');
  step.className = `execution-step execution-step--${type}`;
  step.dataset.executionStep = type;
  step.appendChild(panel);
  body?.appendChild(step);
  syncExecutionStackVisibility(stack);
  return step;
}

export function removeExecutionPanel(panel: Element | null): void {
  if (!panel) return;
  const step = panel.closest<HTMLElement>('.execution-step');
  const stack = step?.closest<HTMLElement>('.execution-stack');
  if (!step || !stack) {
    panel.closest('.timeline-node')?.remove();
    if (panel.isConnected) panel.remove();
    return;
  }
  step.remove();
  if (getSteps(stack).length === 0) {
    const status = stackStatus(stack);
    if (status === 'running' || status === 'completed') {
      if (state.activeExecutionStack === stack) state.activeExecutionStack = null;
      stack.remove();
      invalidateChatScrollCache();
    } else {
      syncExecutionStackVisibility(stack);
    }
    return;
  }
  syncExecutionStackVisibility(stack);
}

export function refreshExecutionStackForPanel(panel: Element | null): void {
  const stack = executionStackForPanel(panel);
  if (stack) syncStackSummary(stack);
}

export function resumeExecutionStackAutoCollapse(panel: Element | null): HTMLButtonElement | null {
  const stack = executionStackForPanel(panel);
  if (
    !stack ||
    stack.dataset.executionStatus !== 'completed' ||
    stack.dataset.executionUserToggled === 'true' ||
    collapseTimers.has(stack) ||
    stackHasOpenDetail(stack)
  ) {
    return null;
  }
  setStackExpanded(stack, false);
  return stack.querySelector<HTMLButtonElement>('.execution-stack-header');
}

export function syncExecutionStackVisibility(stack: HTMLElement): void {
  for (const step of getSteps(stack)) step.hidden = !stepIsVisible(step);
  invalidateChatScrollCache();
  syncStackSummary(stack);
}

export function syncAllExecutionStackVisibility(): void {
  dom.chat
    ?.querySelectorAll<HTMLElement>('.execution-stack')
    .forEach((stack) => syncExecutionStackVisibility(stack));
}

function inferredTerminalStatus(stack: HTMLElement, options: CompleteExecutionStackOptions) {
  if (options.status && options.status !== 'completed') return options.status;
  if (options.failed) return 'failed';
  const descriptors = stackDescriptors(stack);
  markRecoveredRetries(descriptors);
  return descriptors.some((step) => step.state === 'failed') ? 'partial' : 'completed';
}

export function completeExecutionStack(
  options: CompleteExecutionStackOptions = {},
): HTMLElement | null {
  const explicitStatus = options.status || (options.failed ? 'failed' : null);
  const stack =
    options.stack ||
    state.activeExecutionStack ||
    (explicitStatus && explicitStatus !== 'completed' && dom.chat ? createExecutionStack() : null);
  if (!stack?.isConnected) {
    if (!options.stack) state.activeExecutionStack = null;
    return null;
  }
  if (getSteps(stack).length === 0 && (!explicitStatus || explicitStatus === 'completed')) {
    stack.remove();
    if (state.activeExecutionStack === stack) state.activeExecutionStack = null;
    return null;
  }

  let durationMs = options.durationMs;
  if (durationMs === undefined && stackStatus(stack) !== 'running') {
    durationMs = stack.dataset.executionDuration ? Number(stack.dataset.executionDuration) : null;
  } else if (durationMs === undefined) {
    const startedAt = Number(stack.dataset.executionStartedAt || '');
    const now = typeof performance !== 'undefined' ? performance.now() : Date.now();
    if (Number.isFinite(startedAt) && startedAt > 0 && now >= startedAt)
      durationMs = now - startedAt;
  }
  const requestedStatus = inferredTerminalStatus(stack, options);
  const requestedSource = options.terminalSource || 'generic';
  const existingStatus = stackStatus(stack);
  const existingSource = terminalSource(stack);
  const preserveExistingOutcome =
    options.mergeWithExisting === true &&
    existingStatus !== 'running' &&
    terminalSourcePriority[existingSource] > terminalSourcePriority[requestedSource];
  const status = preserveExistingOutcome ? existingStatus : requestedStatus;
  if (!preserveExistingOutcome && options.diagnostic)
    stack.dataset.executionDiagnosticCode = options.diagnostic.code;
  if (status === 'completed' || status === 'discarded')
    delete stack.dataset.executionDiagnosticCode;
  delete stack.dataset.executionTransientProgress;
  stack.dataset.executionStatus = status;
  stack.dataset.executionFailed = String(status === 'failed');
  if (!preserveExistingOutcome) {
    stack.dataset.executionTerminalSource = requestedSource;
  }
  if (!preserveExistingOutcome && options.summary) {
    stack.dataset.executionOutcomeSummary = options.summary;
  }
  if (!preserveExistingOutcome) {
    if (options.summaryKey) stack.dataset.executionOutcomeKey = options.summaryKey;
    else delete stack.dataset.executionOutcomeKey;
    if (options.summaryParams) {
      stack.dataset.executionOutcomeParams = JSON.stringify(options.summaryParams);
    } else {
      delete stack.dataset.executionOutcomeParams;
    }
  }
  if (!preserveExistingOutcome && options.recoveryLabel) {
    stack.dataset.executionRecoveryLabel = options.recoveryLabel;
  }
  if (!preserveExistingOutcome) {
    if (options.recoveryLabelKey) {
      stack.dataset.executionRecoveryKey = options.recoveryLabelKey;
    } else {
      delete stack.dataset.executionRecoveryKey;
    }
    if (options.recoveryLabelParams) {
      stack.dataset.executionRecoveryParams = JSON.stringify(options.recoveryLabelParams);
    } else {
      delete stack.dataset.executionRecoveryParams;
    }
  }
  if (!preserveExistingOutcome && (options.recoveryTarget || options.recoveryPlanId)) {
    recoveryHints.set(stack, {
      target: options.recoveryTarget,
      planId: options.recoveryPlanId,
      planRevision: options.recoveryPlanRevision,
    });
  }
  if (durationMs != null && Number.isFinite(durationMs) && durationMs > 0) {
    stack.dataset.executionDuration = String(durationMs);
  } else {
    delete stack.dataset.executionDuration;
  }
  stack.classList.remove(
    'is-running',
    'is-complete',
    'is-completed',
    'is-failed',
    'is-blocked',
    'is-waiting-user',
    'is-partial',
    'is-stopped',
    'is-incomplete',
    'is-discarded',
  );
  stack.classList.add('is-complete', `is-${status.replace('_', '-')}`);

  const attentionStatus = status !== 'completed' && status !== 'discarded';
  if (attentionStatus && stack.dataset.executionUserToggled !== 'true')
    setStackExpanded(stack, true);
  if (attentionStatus && !options.recoveryTarget) {
    const target = getSteps(stack)
      .find(stepHasFailure)
      ?.querySelector<HTMLElement>('button, [tabindex]');
    if (target && !recoveryHints.has(stack)) recoveryHints.set(stack, { target });
  }
  syncStackSummary(stack);

  const announcer = stack.querySelector<HTMLElement>('.execution-stack-announcer');
  if (announcer) {
    const announcement = [
      stack.querySelector('.execution-stack-title')?.textContent,
      stack.dataset.executionDiagnosticCode
        ? stack.querySelector('.execution-stack-summary')?.textContent
        : '',
    ]
      .filter(Boolean)
      .join(' ');
    if (announcer.textContent !== announcement) announcer.textContent = announcement;
  }
  if (state.activeExecutionStack === stack) state.activeExecutionStack = null;

  const existingTimer = collapseTimers.get(stack);
  if (existingTimer) {
    clearTimeout(existingTimer);
    collapseTimers.delete(stack);
  }
  if (status !== 'completed' || stack.dataset.executionUserToggled === 'true') return stack;

  const collapse = () => {
    collapseTimers.delete(stack);
    if (
      stack.isConnected &&
      stack.dataset.executionStatus === 'completed' &&
      stack.dataset.executionUserToggled !== 'true' &&
      !stackHasOpenDetail(stack)
    ) {
      setStackExpanded(stack, false);
    }
  };
  if (options.immediate) collapse();
  else collapseTimers.set(stack, window.setTimeout(collapse, 600));
  return stack;
}

export function completeExecutionStackForClientRun(
  clientRunId: number,
  options: Omit<CompleteExecutionStackOptions, 'stack'> = {},
): HTMLElement | null {
  const serverRunId = state.activeExecutionServerRunId;
  if (clientRunId <= 0 || state.activeExecutionRunId !== clientRunId || !serverRunId) {
    return null;
  }
  const runId = String(clientRunId);
  const matchingStack = [state.activeExecutionStack, state.terminalExecutionStack].find(
    (stack) =>
      stack?.isConnected &&
      stack.dataset.executionClientRunId === runId &&
      stack.dataset.executionServerRunId === serverRunId,
  );
  const stack = matchingStack || createExecutionStack();
  if (
    stack.dataset.executionClientRunId !== runId ||
    stack.dataset.executionServerRunId !== serverRunId
  ) {
    return null;
  }
  return completeExecutionStack({ ...options, stack });
}

export function completeExecutionStackForPlan(
  plan: PlanStatePayload,
  options: { historical?: boolean } = {},
): void {
  if (!options.historical) associateExecutionStackWithPlan(plan);
  const activeStack = state.activeExecutionStack;
  const activeRunMatches =
    activeStack?.isConnected === true &&
    state.activeExecutionRunId > 0 &&
    Boolean(state.activeExecutionServerRunId) &&
    activeStack.dataset.executionClientRunId === String(state.activeExecutionRunId) &&
    activeStack.dataset.executionServerRunId === state.activeExecutionServerRunId &&
    activeStack.dataset.executionPlanId === plan.plan_id;
  // The DOM owns completed stacks. Look up the latest exact Plan revision in
  // this Session after done has retired its transient run pointers. Detached
  // history and other Sessions cannot be claimed and need no retained Map.
  const historicalMatches = Array.from(
    dom.chat?.querySelectorAll<HTMLElement>('.execution-stack') || [],
  ).filter(
    (stack) =>
      stack.dataset.executionSessionId === (state.activeSessionId || 'main') &&
      stack.dataset.executionPlanId === plan.plan_id &&
      stack.dataset.executionPlanRevision === String(plan.revision) &&
      Boolean(stack.dataset.executionClientRunId || stack.dataset.executionPersistedRunId) &&
      Boolean(stack.dataset.executionServerRunId) &&
      stack.dataset.executionStatus !== 'running',
  );
  const historicalMatch = historicalMatches.at(-1);
  if (options.historical) {
    // Discard retires every attention view owned by this exact Plan revision.
    // Other lifecycle outcomes belong to its latest run, not earlier attempts.
    const targets =
      plan.status === 'discarded' ? historicalMatches : historicalMatch ? [historicalMatch] : [];
    targets.forEach((stack) => applyExecutionStackPlanOutcome(stack, plan));
    return;
  }
  const terminalStack = [state.terminalExecutionStack, historicalMatch].find(
    (stack) =>
      stack?.dataset.executionSessionId === (state.activeSessionId || 'main') &&
      stack.dataset.executionPlanId === plan.plan_id &&
      (!stack.dataset.executionPlanRevision ||
        stack.dataset.executionPlanRevision === String(plan.revision)),
  );
  const terminalRunMatches =
    terminalStack?.isConnected === true &&
    Boolean(
      terminalStack.dataset.executionClientRunId || terminalStack.dataset.executionPersistedRunId,
    ) &&
    Boolean(terminalStack.dataset.executionServerRunId) &&
    terminalStack.dataset.executionPlanId === plan.plan_id &&
    (!terminalStack.dataset.executionPlanRevision ||
      terminalStack.dataset.executionPlanRevision === String(plan.revision));
  const matchingStack = activeRunMatches ? activeStack : terminalRunMatches ? terminalStack : null;
  if (!matchingStack) return;
  if (applyExecutionStackPlanOutcome(matchingStack, plan)) {
    state.terminalExecutionStack = matchingStack;
  }
}

function applyExecutionStackPlanOutcome(
  matchingStack: HTMLElement,
  plan: PlanStatePayload,
): boolean {
  const previousUpdate = Number(matchingStack.dataset.executionPlanUpdatedAt || '-1');
  const previousAttempt = Number(matchingStack.dataset.executionPlanAttempt || '-1');
  if (
    previousUpdate > plan.updated_at ||
    previousAttempt > (plan.execution_attempt || 0) ||
    (matchingStack.dataset.executionPlanStatus === 'discarded' && plan.status !== 'discarded')
  )
    return false;
  const blocked = plan.progress.some((step) => step.status === 'blocked');
  const target = Array.from(document.querySelectorAll<HTMLElement>('.plan-artifact-card')).find(
    (card) =>
      card.dataset.planId === plan.plan_id && card.dataset.planRevision === String(plan.revision),
  );
  if (plan.status === 'discarded') {
    delete matchingStack.dataset.executionRecoveryLabel;
    delete matchingStack.dataset.executionRecoveryKey;
    delete matchingStack.dataset.executionRecoveryParams;
    recoveryHints.delete(matchingStack);
    completeExecutionStack({
      stack: matchingStack,
      status: 'discarded',
      summary: tr('execution.planDiscarded'),
      summaryKey: 'execution.planDiscarded',
      terminalSource: 'plan',
    });
  } else if (plan.status === 'needs_input') {
    completeExecutionStack({
      stack: matchingStack,
      status: 'waiting_user',
      summary: tr('execution.planNeedsInput'),
      summaryKey: 'execution.planNeedsInput',
      recoveryLabel: tr('execution.answerPlan'),
      recoveryLabelKey: 'execution.answerPlan',
      recoveryTarget: target,
      recoveryPlanId: plan.plan_id,
      recoveryPlanRevision: plan.revision,
      terminalSource: 'plan',
    });
  } else if (plan.status === 'failed') {
    completeExecutionStack({
      stack: matchingStack,
      status: blocked ? 'blocked' : 'failed',
      summary: blocked ? tr('execution.planBlocked') : tr('execution.planFailed'),
      summaryKey: blocked ? 'execution.planBlocked' : 'execution.planFailed',
      recoveryLabel: plan.approved_at ? tr('plan.resume') : tr('plan.revise'),
      recoveryLabelKey: plan.approved_at ? 'plan.resume' : 'plan.revise',
      recoveryTarget: target,
      recoveryPlanId: plan.plan_id,
      recoveryPlanRevision: plan.revision,
      terminalSource: 'plan',
    });
  } else if (plan.status === 'stopped') {
    completeExecutionStack({
      stack: matchingStack,
      status: 'stopped',
      summary: tr('execution.planStopped'),
      summaryKey: 'execution.planStopped',
      recoveryLabel: plan.approved_at ? tr('plan.resume') : tr('plan.revise'),
      recoveryLabelKey: plan.approved_at ? 'plan.resume' : 'plan.revise',
      recoveryTarget: target,
      recoveryPlanId: plan.plan_id,
      recoveryPlanRevision: plan.revision,
      terminalSource: 'plan',
    });
  } else if (plan.status === 'completed' && plan.run_finished_with_unreported_steps) {
    completeExecutionStack({
      stack: matchingStack,
      status: 'partial',
      summary: tr('plan.unreportedSteps', { count: plan.unfinished_steps || 0 }),
      summaryKey: 'plan.unreportedSteps',
      summaryParams: { count: plan.unfinished_steps || 0 },
      recoveryLabel: tr('execution.reviewPlan'),
      recoveryLabelKey: 'execution.reviewPlan',
      recoveryTarget: target,
      recoveryPlanId: plan.plan_id,
      recoveryPlanRevision: plan.revision,
      terminalSource: 'plan',
    });
  } else {
    return false;
  }
  if (matchingStack.isConnected) {
    matchingStack.dataset.executionPlanId = plan.plan_id;
    matchingStack.dataset.executionPlanRevision = String(plan.revision);
    matchingStack.dataset.executionPlanUpdatedAt = String(plan.updated_at);
    matchingStack.dataset.executionPlanAttempt = String(plan.execution_attempt || 0);
    matchingStack.dataset.executionPlanStatus = plan.status;
  }
  return true;
}

export function associateExecutionStackWithPlan(plan: PlanStatePayload): void {
  const planId = String(plan?.plan_id || '').trim();
  if (!planId || state.activeExecutionRunId <= 0) return;
  if (state.activeExecutionPlanId && state.activeExecutionPlanId !== planId) return;

  const mayAssociate =
    state.activeExecutionPlanId === planId ||
    state.pendingPlanExecutionId === planId ||
    plan.status === 'planning' ||
    plan.status === 'executing';
  if (!mayAssociate) return;

  state.activeExecutionPlanId = planId;
  const activeStack = state.activeExecutionStack;
  if (
    !activeStack?.isConnected ||
    activeStack.dataset.executionClientRunId !== String(state.activeExecutionRunId)
  ) {
    return;
  }
  if (activeStack.dataset.executionPlanId && activeStack.dataset.executionPlanId !== planId) return;
  activeStack.dataset.executionPlanId = planId;
  activeStack.dataset.executionPlanRevision = String(plan.revision);
}

export function focusExecutionStackRecovery(
  trigger: Element | null,
  revealPlan?: (identity: {
    sessionId: string;
    planId: string;
    revision: number;
  }) => HTMLElement | null,
): void {
  const stack = trigger?.closest<HTMLElement>('.execution-stack');
  if (!stack?.isConnected || stack.dataset.executionSessionId !== (state.activeSessionId || 'main'))
    return;
  setStackExpanded(stack, true);

  const hint = recoveryHints.get(stack);
  let currentPlanTarget = hint?.planId
    ? Array.from(document.querySelectorAll<HTMLElement>('.plan-artifact-card')).find(
        (card) =>
          card.dataset.planId === hint.planId &&
          (hint.planRevision != null
            ? card.dataset.planRevision === String(hint.planRevision)
            : card.dataset.historical !== 'true'),
      )
    : null;
  if (
    !currentPlanTarget &&
    hint?.planId &&
    Number.isSafeInteger(hint.planRevision) &&
    hint.planRevision! > 0
  ) {
    currentPlanTarget =
      revealPlan?.({
        sessionId: stack.dataset.executionSessionId,
        planId: hint.planId,
        revision: hint.planRevision!,
      }) || null;
  }
  let target = currentPlanTarget || (hint?.target?.isConnected ? hint.target : null);
  if (!target) {
    const failureStep = getSteps(stack).find(stepHasFailure);
    target =
      stack.querySelector<HTMLElement>(
        '.execution-stack-diagnostic:not([hidden]) .execution-stack-diagnostic-content',
      ) ||
      failureStep?.querySelector<HTMLElement>('button, [tabindex]') ||
      failureStep ||
      stack.querySelector<HTMLElement>('.execution-stack-recovery-summary') ||
      stack.querySelector<HTMLElement>('.execution-stack-header');
  }
  if (!target?.isConnected) return;

  const targetStep = target.closest<HTMLElement>('.execution-step');
  if (targetStep) targetStep.hidden = false;
  const targetDetails = target.closest<HTMLDetailsElement>('details');
  if (targetDetails) targetDetails.open = true;
  const reducedMotion =
    globalThis.matchMedia?.('(prefers-reduced-motion: reduce)').matches === true;
  target.scrollIntoView?.({ block: 'center', behavior: reducedMotion ? 'auto' : 'smooth' });
  if (!target.matches('button, a, input, select, textarea, [tabindex]')) target.tabIndex = -1;
  target.focus();
}

export function toggleExecutionStack(trigger: Element | null): void {
  const stack = trigger?.closest<HTMLElement>('.execution-stack');
  if (!stack) return;
  const timer = collapseTimers.get(stack);
  if (timer) {
    clearTimeout(timer);
    collapseTimers.delete(stack);
  }
  stack.dataset.executionUserToggled = 'true';
  setStackExpanded(stack, !stack.classList.contains('is-expanded'));
}

export function refreshExecutionStacks(): void {
  dom.chat?.querySelectorAll<HTMLElement>('.execution-stack').forEach(syncExecutionStackVisibility);
}

export function resetExecutionStackState(): HTMLElement | null {
  const active = state.activeExecutionStack;
  state.activeExecutionStack = null;
  return active;
}

export function restoreExecutionStackState(stack: HTMLElement | null): void {
  state.activeExecutionStack =
    stack?.isConnected && stack.dataset.executionStatus === 'running' ? stack : null;
}
