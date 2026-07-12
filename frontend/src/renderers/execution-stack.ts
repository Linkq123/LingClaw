import { iconMarkup } from '../icons.js';
import { tr } from '../i18n.js';
import { dom, state } from '../state.js';
import { formatToolDuration } from '../utils.js';

export type ExecutionStepType =
  | 'reasoning'
  | 'tool'
  | 'result'
  | 'task-plan'
  | 'subagent'
  | 'orchestrate';

type CompleteExecutionStackOptions = {
  durationMs?: number | null;
  failed?: boolean;
  immediate?: boolean;
  stack?: HTMLElement | null;
};

let executionStackId = 0;
const collapseTimers = new WeakMap<HTMLElement, number>();

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
  return type === 'reasoning' ? state.showReasoning : state.showTools;
}

function stepHasFailure(step: HTMLElement): boolean {
  return Boolean(
    step.querySelector(
      '.tool-panel-failed, .subagent-failed, .orchestrate-aborted, .orchestrate-task-failed',
    ),
  );
}

function stepCountText(count: number): string {
  return tr(count === 1 ? 'execution.stepCountOne' : 'execution.stepCount', { count });
}

function syncStackSummary(stack: HTMLElement): void {
  const steps = getSteps(stack);
  const visibleSteps = steps.filter((step) => !step.hidden);
  const failed = stack.dataset.executionFailed === 'true' || steps.some(stepHasFailure);
  const completed = stack.dataset.executionStatus === 'complete';
  const title = stack.querySelector<HTMLElement>('.execution-stack-title');
  const meta = stack.querySelector<HTMLElement>('.execution-stack-meta');
  const statusIcon = stack.querySelector<HTMLElement>('.execution-stack-status-icon');
  const duration = Number(stack.dataset.executionDuration || '');
  const parts = [stepCountText(visibleSteps.length)];

  if (Number.isFinite(duration) && duration > 0) {
    parts.push(formatToolDuration(duration));
  }
  if (title) {
    title.textContent = failed
      ? tr('execution.failed')
      : tr(completed ? 'execution.worked' : 'execution.working');
  }
  if (meta) meta.textContent = parts.join(' · ');
  if (statusIcon) {
    statusIcon.innerHTML = iconMarkup(failed ? 'alert-triangle' : completed ? 'check' : 'activity');
  }

  const header = stack.querySelector<HTMLButtonElement>('.execution-stack-header');
  if (header) {
    header.setAttribute(
      'aria-label',
      [title?.textContent, meta?.textContent].filter(Boolean).join(', '),
    );
  }
  stack.classList.toggle('is-failed', failed);
  stack.hidden = visibleSteps.length === 0;
}

function stackHasOpenDetail(stack: HTMLElement): boolean {
  const activeToolPanel = state.activeToolPanel;
  if (activeToolPanel?.closest('.execution-stack') === stack) return true;

  // Sub-agent and orchestration modals temporarily move their execution step
  // to document.body and leave this placeholder in the owning stack.
  return Boolean(stack.querySelector('.subagent-modal-placeholder'));
}

function setStackExpanded(stack: HTMLElement, expanded: boolean): void {
  const header = stack.querySelector<HTMLButtonElement>('.execution-stack-header');
  const body = getBody(stack);
  stack.classList.toggle('is-expanded', expanded);
  header?.setAttribute('aria-expanded', String(expanded));
  if (body) body.hidden = !expanded;
  stack.querySelector('.execution-stack-chevron')?.classList.toggle('open', expanded);
}

function createExecutionStack(before: Element | null = null): HTMLElement {
  const stack = document.createElement('section');
  const bodyId = `execution-stack-body-${++executionStackId}`;
  stack.className = 'execution-stack is-running is-expanded';
  stack.dataset.executionStatus = 'running';
  stack.dataset.executionFailed = 'false';
  stack.dataset.executionStartedAt = String(
    state.currentRoundStartedAt ||
      (typeof performance !== 'undefined' ? performance.now() : Date.now()),
  );
  stack.innerHTML = `
    <button type="button" class="execution-stack-header" data-action="toggle-execution-stack" aria-expanded="true" aria-controls="${bodyId}">
      <span class="execution-stack-status-icon">${iconMarkup('activity')}</span>
      <span class="execution-stack-title"></span>
      <span class="execution-stack-meta"></span>
      <span class="execution-stack-chevron">${iconMarkup('chevron-right')}</span>
    </button>
    <div class="execution-stack-body" id="${bodyId}"></div>
    <span class="execution-stack-announcer" aria-live="polite"></span>
  `;

  if (before?.parentElement === dom.chat) dom.chat.insertBefore(stack, before);
  else dom.chat.appendChild(stack);
  state.activeExecutionStack = stack;
  syncStackSummary(stack);
  return stack;
}

export function ensureExecutionStack(before: Element | null = null): HTMLElement {
  const active = state.activeExecutionStack;
  if (active?.isConnected && active.dataset.executionStatus === 'running') return active;
  return createExecutionStack(before);
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
    if (state.activeExecutionStack === stack) state.activeExecutionStack = null;
    stack.remove();
    return;
  }
  syncExecutionStackVisibility(stack);
}

export function refreshExecutionStackForPanel(panel: Element | null): void {
  const directStack = panel?.closest<HTMLElement>('.execution-stack');
  const modalStep = panel?.closest<HTMLElement & { _modalHostPlaceholder?: HTMLElement | null }>(
    '.execution-step',
  );
  const stack =
    directStack || modalStep?._modalHostPlaceholder?.closest<HTMLElement>('.execution-stack');
  if (stack) syncStackSummary(stack);
}

export function syncExecutionStackVisibility(stack: HTMLElement): void {
  for (const step of getSteps(stack)) {
    step.hidden = !stepIsVisible(step);
  }
  syncStackSummary(stack);
}

export function syncAllExecutionStackVisibility(): void {
  dom.chat
    ?.querySelectorAll<HTMLElement>('.execution-stack')
    .forEach((stack) => syncExecutionStackVisibility(stack));
}

export function completeExecutionStack(options: CompleteExecutionStackOptions = {}): void {
  const stack = options.stack || state.activeExecutionStack;
  if (!stack?.isConnected) {
    if (!options.stack) state.activeExecutionStack = null;
    return;
  }
  if (getSteps(stack).length === 0) {
    stack.remove();
    if (state.activeExecutionStack === stack) state.activeExecutionStack = null;
    return;
  }

  let durationMs = options.durationMs;
  if (durationMs === undefined) {
    const startedAt = Number(stack.dataset.executionStartedAt || '');
    const now = typeof performance !== 'undefined' ? performance.now() : Date.now();
    if (Number.isFinite(startedAt) && startedAt > 0 && now >= startedAt)
      durationMs = now - startedAt;
  }
  stack.dataset.executionStatus = 'complete';
  stack.dataset.executionFailed = String(options.failed === true);
  if (durationMs != null && Number.isFinite(durationMs) && durationMs > 0) {
    stack.dataset.executionDuration = String(durationMs);
  } else {
    delete stack.dataset.executionDuration;
  }
  stack.classList.remove('is-running');
  stack.classList.add('is-complete');
  syncStackSummary(stack);

  const announcer = stack.querySelector<HTMLElement>('.execution-stack-announcer');
  if (announcer)
    announcer.textContent = stack.querySelector('.execution-stack-title')?.textContent || '';

  if (state.activeExecutionStack === stack) state.activeExecutionStack = null;
  if (stack.dataset.executionUserToggled === 'true') return;

  const collapse = () => {
    collapseTimers.delete(stack);
    if (
      stack.isConnected &&
      stack.dataset.executionUserToggled !== 'true' &&
      !stackHasOpenDetail(stack)
    ) {
      setStackExpanded(stack, false);
    }
  };
  if (options.immediate) collapse();
  else collapseTimers.set(stack, window.setTimeout(collapse, 600));
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
  dom.chat?.querySelectorAll<HTMLElement>('.execution-stack').forEach((stack) => {
    syncExecutionStackVisibility(stack);
  });
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
