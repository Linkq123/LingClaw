import { dom, state } from '../state.js';
import type { ImageAttachment, SubagentHistorySnapshot } from '../types.js';
import {
  escHtml,
  formatToolDuration,
  formatTokenCount,
  formatDetailText,
  stripDelegatedPromptRuntimeContext,
  pulseFocus,
  copyButtonText,
} from '../utils.js';
import { scrollDown } from '../scroll.js';
import { animatePanelIn, animateCollapsibleSection } from './timeline.js';
import {
  mountExecutionPanel,
  refreshExecutionStackForPanel,
  resumeExecutionStackAutoCollapse,
} from './execution-stack.js';
import { pinReactStatusToBottom } from './react-status.js';
import {
  closeToolDrawer,
  openToolDrawer,
  syncToolDrawer,
  mergeToolLiveOutput,
  normalizeToolImages,
} from './tools.js';
import {
  ensureModalBackdrop,
  moveModalHostToBody,
  restoreModalHost,
  syncModalHostPlaceholder,
} from './modalHost.js';
import { iconMarkup } from '../icons.js';
import { tr } from '../i18n.js';
import { trapDialogFocus } from '../pages/dialogFocus.js';

type SubagentPanelRef = {
  task_id?: string;
  agent?: string;
  allowAgentFallback?: boolean;
};

type SubagentStats = {
  cycles?: number;
  tool_calls?: number;
  duration_ms?: number;
  input_tokens?: number;
  output_tokens?: number;
  result_excerpt?: string;
  result_preview?: string;
  error?: string;
  status_label?: string;
  summary_title?: string;
  summary_title_key?: string;
  summary_tone?: 'success' | 'error' | 'muted';
  summary_body?: string;
  summary_body_key?: string;
};

type ToolCounts = {
  total: number;
  settled: number;
  failed: number;
  running: number;
};

type TextNodeHost = HTMLElement & {
  _textNode?: Text;
};

const LABELS = {
  get subagent() {
    return tr('execution.subagent');
  },
  get running() {
    return tr('execution.running');
  },
  get thinking() {
    return tr('execution.thinking');
  },
  get completed() {
    return tr('execution.completed');
  },
  get failed() {
    return tr('tool.failed');
  },
  get waiting() {
    return tr('execution.waiting');
  },
  get reasoning() {
    return tr('common.reasoning');
  },
  get copySummary() {
    return tr('execution.copySummary');
  },
  get taskPrompt() {
    return tr('execution.taskPrompt');
  },
  get toolChain() {
    return tr('execution.toolChain');
  },
  get noToolCallsYet() {
    return tr('execution.noToolCallsYet');
  },
  get noToolCallsInHistory() {
    return tr('execution.noToolCallsInHistory');
  },
  get toolFailedNoOutput() {
    return tr('execution.toolFailedNoOutput');
  },
  get executionSummary() {
    return tr('execution.summary');
  },
  get failureDetails() {
    return tr('execution.failureDetails');
  },
  get duration() {
    return tr('execution.duration');
  },
} as const;

let lastSubagentModalFocus: HTMLElement | null = null;

function setSubagentModalBackgroundInert(modal: boolean): void {
  document.body.classList.toggle('subagent-modal-visible', modal);
  const toolDrawerModalOpen = document.body.classList.contains('tool-drawer-modal-open');
  const mobileNavigationViewport =
    typeof window.matchMedia === 'function'
      ? window.matchMedia('(max-width: 768px)').matches
      : window.innerWidth <= 768;
  const mobileNavigationOpen = mobileNavigationViewport && state.mobileNavigationOpen;
  if (dom.sessionDrawer) {
    dom.sessionDrawer.inert =
      modal || toolDrawerModalOpen || (mobileNavigationViewport && !mobileNavigationOpen);
  }
  const conversation = document.querySelector<HTMLElement>('.conversation-column');
  if (conversation) {
    conversation.inert = modal || toolDrawerModalOpen || mobileNavigationOpen;
  }
}

export function trapSubagentModalFocus(event: KeyboardEvent): boolean {
  const panel = document.querySelector<HTMLElement>('.subagent-panel.subagent-modal-open');
  if (!panel) return false;
  return trapDialogFocus(event, panel);
}

function getToolTrail(panel): HTMLElement | null {
  return (panel as Element).querySelector('[data-subagent-tool-trail]') as HTMLElement | null;
}

function getToolTrailMeta(panel): HTMLElement | null {
  return (panel as Element).querySelector('[data-subagent-tools-meta]') as HTMLElement | null;
}

function getToolTrailEmpty(panel): HTMLElement | null {
  return (panel as Element).querySelector('[data-subagent-tool-empty]') as HTMLElement | null;
}

function getReasoningCard(panel): HTMLElement | null {
  return (panel as Element).querySelector('[data-subagent-reasoning]') as HTMLElement | null;
}

function getReasoningMeta(panel): HTMLElement | null {
  return (panel as Element).querySelector('[data-subagent-reasoning-meta]') as HTMLElement | null;
}

function getReasoningBody(panel): TextNodeHost | null {
  return (panel as Element).querySelector('[data-subagent-reasoning-body]') as TextNodeHost | null;
}

export function ensurePromptCard(panel) {
  if (!panel) return null;

  const body = panel.querySelector('.subagent-body');
  if (!body) return null;

  let card = body.querySelector('[data-subagent-prompt-card]');
  if (card) return card;

  card = document.createElement('div');
  card.className = 'subagent-section-card';
  card.dataset.subagentPromptCard = 'true';
  card.innerHTML = `
    <div class="subagent-section-title" data-i18n="execution.taskPrompt">${LABELS.taskPrompt}</div>
    <div class="subagent-prompt"></div>
  `;

  const toolOverview = body.querySelector('.subagent-tools-overview');
  body.insertBefore(card, toolOverview || body.querySelector('.subagent-summary') || null);
  return card;
}

function ensureReasoningCard(panel) {
  if (!panel) return null;

  let card = getReasoningCard(panel);
  if (card) return card;

  card = document.createElement('div');
  card.className = 'subagent-section-card subagent-reasoning-card';
  card.dataset.subagentReasoning = 'true';
  card.hidden = true;
  card.innerHTML = `
    <div class="subagent-section-head">
      <div class="subagent-section-title" data-i18n="common.reasoning">${LABELS.reasoning}</div>
      <div class="subagent-section-meta" data-subagent-reasoning-meta>${LABELS.waiting}</div>
    </div>
    <pre class="subagent-reasoning-body" data-subagent-reasoning-body></pre>
  `;

  const body = panel.querySelector('.subagent-body');
  const toolOverview = panel.querySelector('.subagent-tools-overview');
  if (body) {
    body.insertBefore(card, toolOverview || body.querySelector('.subagent-summary') || null);
  }
  return card;
}

function reasoningPreview(rawText, fallback = LABELS.completed) {
  const summaryText = (rawText || '').trim().replace(/\n+/g, ' ');
  const preview = summaryText.slice(0, 60);
  return preview ? preview + (summaryText.length > 60 ? '...' : '') : fallback;
}

function setChipText(panel, key, value, extraClass = '') {
  const chip = panel.querySelector(`[data-subagent-chip="${key}"]`);
  if (!chip) return;
  chip.textContent = value;
  chip.className = 'subagent-chip';
  if (extraClass) chip.classList.add(extraClass);
}

function getToolBadges(panel): HTMLButtonElement[] {
  const trail = getToolTrail(panel);
  if (!trail) return [];
  return Array.from(trail.querySelectorAll<HTMLButtonElement>('.subagent-tool-pill'));
}

function hasStableToolId(toolId) {
  return typeof toolId === 'string' && toolId.trim().length > 0;
}

function findPendingEmptyIdToolBadge(panel, toolName = ''): HTMLButtonElement | null {
  if (!panel) return null;

  const badges = getToolBadges(panel);
  const matchesToolName = (badge) => !toolName || (badge.dataset.toolName || '') === toolName;

  return (
    badges.find(
      (badge) =>
        !hasStableToolId(badge.dataset.toolId) &&
        badge.classList.contains('is-running') &&
        matchesToolName(badge),
    ) ||
    badges.find(
      (badge) => !hasStableToolId(badge.dataset.toolId) && badge.classList.contains('is-running'),
    ) ||
    null
  );
}

function findToolBadge(
  panel,
  toolId,
  { allowPendingEmptyId = false, toolName = '' } = {},
): HTMLButtonElement | null {
  if (!panel) return null;
  if (hasStableToolId(toolId)) {
    return getToolBadges(panel).find((badge) => badge.dataset.toolId === toolId) || null;
  }
  if (allowPendingEmptyId) {
    return findPendingEmptyIdToolBadge(panel, toolName);
  }
  return null;
}

function updateToolBadgeState(badge, stateLabel, tone) {
  if (!badge) return;
  badge.classList.remove('is-running', 'is-done', 'is-failed');
  if (tone) badge.classList.add(tone);
  badge.dataset.toolStatus = stateLabel;
  const status = badge.querySelector('.subagent-tool-pill-state');
  if (status) status.textContent = stateLabel;
}

function localizedToolBadgeStatus(badge: HTMLButtonElement): string {
  const duration = badge.dataset.toolDurationMs
    ? formatToolDuration(Number(badge.dataset.toolDurationMs))
    : '';
  const label =
    badge.dataset.toolState === 'failed'
      ? LABELS.failed
      : badge.dataset.toolState === 'completed'
        ? LABELS.completed
        : LABELS.running;
  return duration ? `${label} / ${duration}` : label;
}

function refreshToolBadgeLanguage(badge: HTMLButtonElement): void {
  const status = localizedToolBadgeStatus(badge);
  badge.dataset.toolStatus = status;
  badge.title = [badge.dataset.toolName || 'tool', status].filter(Boolean).join(' / ');
  const statusEl = badge.querySelector<HTMLElement>('.subagent-tool-pill-state');
  if (statusEl) statusEl.textContent = status;
  if (state.activeToolPanel === badge) syncToolDrawer(badge);
}

function syncToolBadgeDataset(
  badge,
  toolName,
  toolArgs = '',
  toolResult = '',
  toolStatus: string = LABELS.running,
  hasResult: boolean = false,
  images: ImageAttachment[] = [],
) {
  if (!badge) return;
  const normalizedImages = normalizeToolImages(images);
  const formattedArgs = formatDetailText(toolArgs || '');
  const formattedResult = formatDetailText(toolResult || '');
  badge.dataset.toolName = toolName || 'tool';
  badge.dataset.toolArgs = formattedArgs;
  badge.dataset.toolResult = formattedResult;
  badge.dataset.toolLiveOutput = badge.dataset.toolLiveOutput || '';
  badge.dataset.toolHasResult = hasResult ? 'true' : 'false';
  badge.dataset.toolStatus = toolStatus;
  badge.dataset.toolImages = JSON.stringify(normalizedImages);
  badge.dataset.toolImageCount = String(normalizedImages.length);
  badge.title = [toolName || 'tool', toolStatus].filter(Boolean).join(' / ');
}

function ensureToolBadge(panel, toolId, toolName) {
  const trail = getToolTrail(panel);
  if (!trail) return null;

  let badge = toolId ? findToolBadge(panel, toolId) : null;
  if (badge) {
    const nameEl = badge.querySelector('.subagent-tool-pill-name');
    if (nameEl) nameEl.textContent = toolName;
    return badge;
  }

  badge = document.createElement('button');
  badge.type = 'button';
  badge.className = 'subagent-tool-pill is-running';
  badge.dataset.action = 'subagent-open-tool-drawer';
  badge.dataset.toolId = toolId || '';
  badge.dataset.toolState = 'running';
  badge.dataset.toolDurationMs = '';
  badge.innerHTML = `
    <span class="subagent-tool-pill-index">${trail.childElementCount + 1}</span>
    <span class="subagent-tool-pill-name">${escHtml(toolName)}</span>
    <span class="subagent-tool-pill-state">${LABELS.running}</span>
  `;
  syncToolBadgeDataset(badge, toolName, '', '', LABELS.running, false);
  trail.appendChild(badge);
  return badge;
}

function syncToolOverview(
  panel,
  fallbackTotal: number | null = null,
  counts: ToolCounts | null = null,
) {
  if (!panel) return;

  const badges = counts ? null : getToolBadges(panel);
  const total = counts ? counts.total : badges.length;
  const settled = counts
    ? counts.settled
    : badges.filter(
        (badge) => badge.classList.contains('is-done') || badge.classList.contains('is-failed'),
      ).length;
  const failed = counts
    ? counts.failed
    : badges.filter((badge) => badge.classList.contains('is-failed')).length;
  const running = counts
    ? counts.running
    : badges.filter((badge) => badge.classList.contains('is-running')).length;
  const succeeded = Math.max(0, settled - failed);

  const meta = getToolTrailMeta(panel);
  const empty = getToolTrailEmpty(panel);
  const trail = getToolTrail(panel);

  if (meta) {
    if (total === 0) {
      meta.textContent =
        fallbackTotal != null && fallbackTotal > 0
          ? tr('execution.historyToolCallsPreserved', { count: fallbackTotal })
          : LABELS.noToolCallsYet;
    } else {
      const parts = [tr('execution.callsCount', { count: total })];
      if (running) parts.push(tr('execution.runningCount', { count: running }));
      if (succeeded) parts.push(tr('execution.completedCount', { count: succeeded }));
      if (failed) parts.push(tr('execution.failedCount', { count: failed }));
      meta.textContent = parts.join(' / ');
    }
  }

  if (empty) {
    empty.hidden = total > 0;
    empty.textContent =
      fallbackTotal != null && fallbackTotal > 0
        ? LABELS.noToolCallsInHistory
        : LABELS.noToolCallsYet;
  }

  if (trail) trail.hidden = total === 0;
}

function summaryCopyText(panel) {
  if (!panel) return '';

  const parts: string[] = [];
  const label = panel.querySelector('.subagent-label')?.textContent?.trim();
  const status = panel.querySelector('.subagent-status')?.textContent?.trim();
  const prompt = panel.querySelector('.subagent-prompt')?.textContent?.trim();
  const metrics = (
    Array.from((panel as Element).querySelectorAll('.subagent-summary-chip')) as HTMLElement[]
  )
    .map((chip) => chip.textContent?.trim() || '')
    .filter(Boolean)
    .join(' / ');
  const summaryBody = panel
    .querySelector(
      '.subagent-summary:not(.hidden) .subagent-preview, .subagent-summary:not(.hidden) .subagent-error, .subagent-summary:not(.hidden) .subagent-note',
    )
    ?.textContent?.trim();
  const badges = getToolBadges(panel);
  const latestOutput = badges
    .map((badge) => badge.dataset.toolResult?.trim() || '')
    .filter(Boolean)
    .slice(-1)[0];
  const toolsUsed = badges
    .map((badge) => {
      const index = badge.querySelector('.subagent-tool-pill-index')?.textContent?.trim();
      const name = badge.dataset.toolName?.trim() || '';
      const statusText = badge.dataset.toolStatus?.trim() || '';
      return [index ? `${index}.` : '', name, statusText ? `(${statusText})` : '']
        .filter(Boolean)
        .join(' ');
    })
    .filter(Boolean)
    .join('\n');

  if (label) parts.push(label);
  if (status) parts.push(status);
  if (prompt) parts.push(`${LABELS.taskPrompt}\n${prompt}`);
  if (toolsUsed) parts.push(`${LABELS.toolChain}\n${toolsUsed}`);
  if (metrics) parts.push(metrics);
  if (summaryBody) parts.push(summaryBody);
  if (!summaryBody && latestOutput) parts.push(latestOutput);

  return parts.join('\n\n').trim();
}

function syncPanelActions(panel) {
  if (!panel) return;

  const copyBtn = panel.querySelector('[data-action="subagent-copy-summary"]');
  if (copyBtn) {
    copyBtn.disabled = !summaryCopyText(panel);
  }
}

function syncToolCount(panel, fallbackTotal: number | null = null) {
  const badges = getToolBadges(panel);
  const total = badges.length;
  const settled = badges.filter(
    (badge) => badge.classList.contains('is-done') || badge.classList.contains('is-failed'),
  ).length;
  const failed = badges.filter((badge) => badge.classList.contains('is-failed')).length;
  const running = badges.filter((badge) => badge.classList.contains('is-running')).length;
  const displayText = total
    ? tr('execution.toolsProgress', { settled, total })
    : fallbackTotal != null
      ? tr('execution.toolsCount', { count: fallbackTotal })
      : tr('execution.toolsCount', { count: 0 });
  setChipText(panel, 'tools', displayText);
  syncToolOverview(panel, fallbackTotal, { total, settled, failed, running });
}

function storeSummaryData(panel: HTMLElement, success: boolean, stats: SubagentStats = {}): void {
  const bodyText = String(
    stats.summary_body ??
      (success ? stats.result_excerpt || stats.result_preview || '' : stats.error || ''),
  ).trim();
  panel.dataset.finalSuccess = String(success);
  panel.dataset.finalInputTokens = stats.input_tokens == null ? '' : String(stats.input_tokens);
  panel.dataset.finalOutputTokens = stats.output_tokens == null ? '' : String(stats.output_tokens);
  panel.dataset.finalSummaryTitle = stats.summary_title || '';
  panel.dataset.finalSummaryTitleKey = stats.summary_title_key || '';
  panel.dataset.finalSummaryTone = stats.summary_tone || (success ? 'success' : 'error');
  panel.dataset.finalSummaryBody = bodyText;
  panel.dataset.finalSummaryBodyKey = !bodyText ? stats.summary_body_key || '' : '';
}

function renderSummary(panel: HTMLElement): void {
  const summary = panel.querySelector('.subagent-summary');
  if (!summary) return;

  const metrics: string[] = [];
  if (panel.dataset.finalCycles) {
    metrics.push(tr('execution.cyclesMetric', { count: panel.dataset.finalCycles }));
  }
  if (panel.dataset.finalToolCalls) {
    metrics.push(tr('execution.toolsMetric', { count: panel.dataset.finalToolCalls }));
  }
  if (panel.dataset.finalDurationMs) {
    const duration = formatToolDuration(Number(panel.dataset.finalDurationMs));
    if (duration) metrics.push(`${LABELS.duration} ${duration}`);
  }
  if (panel.dataset.finalInputTokens || panel.dataset.finalOutputTokens) {
    const tokens: string[] = [];
    if (panel.dataset.finalInputTokens) {
      tokens.push(
        tr('execution.inputMetric', {
          count: formatTokenCount(Number(panel.dataset.finalInputTokens)),
        }),
      );
    }
    if (panel.dataset.finalOutputTokens) {
      tokens.push(
        tr('execution.outputMetric', {
          count: formatTokenCount(Number(panel.dataset.finalOutputTokens)),
        }),
      );
    }
    if (tokens.length) metrics.push(tokens.join(' / '));
  }

  const bodyText = panel.dataset.finalSummaryBodyKey
    ? tr(panel.dataset.finalSummaryBodyKey)
    : panel.dataset.finalSummaryBody || '';
  const success = panel.dataset.finalSuccess !== 'false';
  const titleText =
    (panel.dataset.finalSummaryTitleKey
      ? tr(panel.dataset.finalSummaryTitleKey)
      : panel.dataset.finalSummaryTitle) ||
    (success ? LABELS.executionSummary : LABELS.failureDetails);
  const tone = panel.dataset.finalSummaryTone || (success ? 'success' : 'error');
  const contentClass =
    tone === 'error' ? 'subagent-error' : tone === 'muted' ? 'subagent-note' : 'subagent-preview';

  const metricHtml = metrics
    .map((metric) => `<span class="subagent-summary-chip">${escHtml(metric)}</span>`)
    .join('');
  const contentHtml = bodyText ? `<pre class="${contentClass}">${escHtml(bodyText)}</pre>` : '';

  if (!metricHtml && !contentHtml) {
    summary.classList.add('hidden');
    summary.innerHTML = '';
    return;
  }

  summary.innerHTML = `
    <div class="subagent-summary-head">
      <div class="subagent-summary-title">${escHtml(titleText)}</div>
      <div class="subagent-summary-metrics">${metricHtml}</div>
    </div>
    ${contentHtml}
  `;
  summary.classList.remove('hidden');
}

function resolvePanel(ref: SubagentPanelRef) {
  if (ref && ref.task_id && state.activeSubagentPanels.has(ref.task_id)) {
    return state.activeSubagentPanels.get(ref.task_id);
  }
  if (
    ref &&
    ref.allowAgentFallback !== false &&
    ref.agent &&
    state.activeSubagentPanels.has(ref.agent)
  ) {
    return state.activeSubagentPanels.get(ref.agent);
  }
  return null;
}

function ensureSubagentBackdrop() {
  return ensureModalBackdrop({
    id: 'subagent-modal-backdrop',
    className: 'subagent-modal-backdrop',
    closeAction: 'close-subagent-modal',
  });
}

function resolveSubagentModalHost(panel) {
  if (!panel) return null;
  return panel.closest('.execution-step, .subagent-modal-anchor') as HTMLElement | null;
}

function syncSubagentModalPlaceholder(panel) {
  const host = resolveSubagentModalHost(panel);
  syncModalHostPlaceholder(host, {
    hostClass: 'subagent-modal-host',
    placeholderClass: 'subagent-modal-placeholder',
  });
}

function syncOwningOrchestrateRowExpansion(panel, expanded) {
  const orchestrateId = panel?.dataset?.orchestrateId || '';
  const taskId = panel?.dataset?.orchestrateTaskId || '';
  if (!orchestrateId || !taskId) return;

  const escapeAttr = (value) =>
    typeof CSS !== 'undefined' && typeof CSS.escape === 'function'
      ? CSS.escape(value)
      : String(value).replace(/"/g, '\\"');

  const row = document.querySelector(
    `.orchestrate-task[data-orchestrate-id="${escapeAttr(orchestrateId)}"][data-task-id="${escapeAttr(taskId)}"] .orchestrate-task-summary`,
  ) as HTMLElement | null;
  row?.setAttribute('aria-expanded', expanded ? 'true' : 'false');
}

export function closeSubagentModal() {
  const panel = document.querySelector('.subagent-panel.subagent-modal-open');
  let shouldRestoreFocus = false;
  let collapsedStackHeader: HTMLButtonElement | null = null;
  if (panel) {
    if (state.activeToolPanel && panel.contains(state.activeToolPanel)) {
      closeToolDrawer();
    }
    shouldRestoreFocus = panel.contains(document.activeElement);
    panel.classList.remove('subagent-modal-open');
    panel.removeAttribute('role');
    panel.removeAttribute('aria-modal');
    panel.removeAttribute('aria-label');
    panel.querySelector('.subagent-header')?.setAttribute('aria-expanded', 'false');
    panel.querySelector('.subagent-modal-close')?.setAttribute('tabindex', '-1');
    panel.querySelector('.subagent-body')?.classList.remove('show');
    const body = panel.querySelector('.subagent-body') as HTMLElement | null;
    if (body) {
      body.style.height = '';
      body.setAttribute('inert', '');
    }
    const host = resolveSubagentModalHost(panel);
    restoreModalHost(host, { hostClass: 'subagent-modal-host' });
    syncOwningOrchestrateRowExpansion(panel, false);
    collapsedStackHeader = resumeExecutionStackAutoCollapse(panel);
  }
  const backdrop = document.getElementById('subagent-modal-backdrop');
  if (backdrop) backdrop.hidden = true;
  setSubagentModalBackgroundInert(false);
  const previousFocus = lastSubagentModalFocus;
  lastSubagentModalFocus = null;
  if (shouldRestoreFocus) {
    if (collapsedStackHeader) collapsedStackHeader.focus();
    else if (previousFocus?.isConnected) previousFocus.focus();
    else document.querySelector<HTMLButtonElement>('.execution-stack-header')?.focus();
  }
}

export function openSubagentPanelModal(panel, trigger: HTMLElement | null = null) {
  if (!panel) return;
  closeSubagentModal();
  lastSubagentModalFocus =
    trigger || (document.activeElement instanceof HTMLElement ? document.activeElement : null);
  closeToolDrawer();
  const backdrop = ensureSubagentBackdrop();
  backdrop.hidden = false;
  const host = resolveSubagentModalHost(panel);
  moveModalHostToBody(host, {
    hostClass: 'subagent-modal-host',
    placeholderClass: 'subagent-modal-placeholder',
  });
  panel.classList.add('subagent-modal-open');
  panel.setAttribute('role', 'dialog');
  panel.setAttribute('aria-modal', 'true');
  panel.setAttribute(
    'aria-label',
    [LABELS.subagent, panel.dataset.agent || tr('execution.subagent')].join(': '),
  );
  setSubagentModalBackgroundInert(true);
  panel.querySelector('.subagent-header')?.setAttribute('aria-expanded', 'true');
  syncOwningOrchestrateRowExpansion(panel, true);
  panel.querySelector('.subagent-modal-close')?.removeAttribute('tabindex');
  const body = panel.querySelector('.subagent-body') as HTMLElement | null;
  if (body) {
    body.removeAttribute('inert');
    body.classList.add('show');
    body.style.height = 'auto';
    body.scrollTop = 0;
  }
  panel.querySelector('.subagent-modal-close')?.focus();
}

export function openSubagentModal(trigger) {
  const panel = trigger?.closest?.('.subagent-panel');
  openSubagentPanelModal(panel, trigger instanceof HTMLElement ? trigger : null);
}

function panelKey(ref: SubagentPanelRef) {
  if (ref && ref.task_id) return ref.task_id;
  return (ref && ref.agent) || '';
}

function registerSubagentPanel(panel, taskId, agentName) {
  state.activeSubagentPanels.set(panelKey({ task_id: taskId, agent: agentName }), panel);
}

function buildSubagentPanel(agentName, prompt, taskId) {
  const displayPrompt = stripDelegatedPromptRuntimeContext(prompt);
  const panel = document.createElement('div');
  panel.className = 'subagent-panel subagent-active';
  panel.dataset.agent = agentName;
  panel.dataset.currentCycle = '1';
  if (taskId) panel.dataset.taskId = taskId;

  const header = document.createElement('button');
  header.type = 'button';
  header.className = 'subagent-header';
  header.dataset.action = 'open-subagent-modal';
  header.setAttribute('aria-expanded', 'false');
  header.innerHTML = `
    <span class="subagent-icon">${iconMarkup('user-node')}</span>
    <span class="subagent-head-copy">
      <span class="subagent-kicker" data-i18n="execution.subagent">${LABELS.subagent}</span>
      <span class="subagent-label">${escHtml(agentName)}</span>
    </span>
    <span class="subagent-status">${LABELS.running}</span>
    <span class="chevron">${iconMarkup('chevron-right')}</span>
  `;

  const closeButton = document.createElement('button');
  closeButton.type = 'button';
  closeButton.className = 'subagent-modal-close';
  closeButton.dataset.action = 'close-subagent-modal';
  closeButton.dataset.i18nAriaLabel = 'execution.closeSubagentDetails';
  closeButton.setAttribute('aria-label', tr('execution.closeSubagentDetails'));
  closeButton.setAttribute('tabindex', '-1');
  closeButton.innerHTML = iconMarkup('close');

  const body = document.createElement('div');
  body.className = 'subagent-body';
  body.setAttribute('inert', '');

  const meta = document.createElement('div');
  meta.className = 'subagent-meta';
  meta.innerHTML = `
    <span class="subagent-chip is-live" data-subagent-chip="state">${LABELS.running}</span>
    <span class="subagent-chip" data-subagent-chip="cycle">${tr('execution.cycle', { cycle: 1 })}</span>
    <span class="subagent-chip" data-subagent-chip="tools">${tr('execution.toolsCount', { count: 0 })}</span>
  `;
  body.appendChild(meta);

  const actions = document.createElement('div');
  actions.className = 'panel-actions subagent-actions';
  actions.innerHTML = `
    <button type="button" class="panel-action-btn" data-action="subagent-copy-summary" data-i18n="execution.copySummary" disabled>${LABELS.copySummary}</button>
  `;
  body.appendChild(actions);

  const toolOverview = document.createElement('div');
  toolOverview.className = 'subagent-section-card subagent-tools-overview';
  toolOverview.innerHTML = `
    <div class="subagent-section-head">
      <div class="subagent-section-title" data-i18n="execution.toolChain">${LABELS.toolChain}</div>
      <div class="subagent-section-meta" data-subagent-tools-meta>${LABELS.noToolCallsYet}</div>
    </div>
    <div class="subagent-tool-empty" data-subagent-tool-empty>${LABELS.noToolCallsYet}</div>
    <div class="subagent-tool-trail" data-subagent-tool-trail hidden></div>
  `;
  body.appendChild(toolOverview);

  const summary = document.createElement('div');
  summary.className = 'subagent-summary hidden';
  body.appendChild(summary);

  panel.appendChild(header);
  panel.appendChild(closeButton);
  panel.appendChild(body);

  if (prompt) {
    const promptCard = ensurePromptCard(panel);
    const promptEl = promptCard?.querySelector('.subagent-prompt');
    if (promptEl) promptEl.textContent = displayPrompt;
  }

  syncToolOverview(panel);
  syncPanelActions(panel);
  return panel;
}

export function createDetachedSubagentPanel(agentName, prompt, taskId) {
  const panel = buildSubagentPanel(agentName, prompt, taskId);
  const anchor = document.createElement('div');
  anchor.className = 'subagent-modal-anchor';
  anchor.appendChild(panel);
  registerSubagentPanel(panel, taskId, agentName);
  return panel;
}

export function createSubagentPanel(agentName, prompt, taskId) {
  const existing = resolvePanel({
    task_id: taskId,
    agent: agentName,
    allowAgentFallback: !taskId,
  });
  if (existing) {
    updateSubagentPrompt({ task_id: taskId, agent: agentName }, prompt);
    return existing;
  }

  const panel = buildSubagentPanel(agentName, prompt, taskId);

  const currentRow = state.currentMsg ? state.currentMsg.closest('.msg-row') : null;
  mountExecutionPanel(panel, 'subagent', currentRow);
  pinReactStatusToBottom();
  animatePanelIn(panel);
  scrollDown();

  registerSubagentPanel(panel, taskId, agentName);
  return panel;
}

export function updateSubagentPrompt(ref: SubagentPanelRef, prompt, { allowBlank = false } = {}) {
  const panel = resolvePanel(ref);
  if (!panel) return;

  const displayPrompt = stripDelegatedPromptRuntimeContext(prompt || '');
  if (!displayPrompt && !allowBlank) return;

  const promptCard = ensurePromptCard(panel);
  const promptEl = promptCard?.querySelector('.subagent-prompt');
  if (promptEl) promptEl.textContent = displayPrompt;
}

export function addSubagentTool(ref: SubagentPanelRef, toolName, toolId, toolArgs = '') {
  const panel = resolvePanel(ref);
  if (!panel) return;

  const formattedArgs = formatDetailText(toolArgs);
  const badge = ensureToolBadge(panel, toolId, toolName);
  syncToolBadgeDataset(badge, toolName, formattedArgs, '', LABELS.running, false);
  updateToolBadgeState(badge, LABELS.running, 'is-running');
  syncToolCount(panel);
  syncPanelActions(panel);
  refreshExecutionStackForPanel(panel);
  scrollDown();
}

export function updateSubagentProgress(ref: SubagentPanelRef, cycle) {
  const panel = resolvePanel(ref);
  if (!panel) return;
  panel.dataset.currentCycle = String(cycle);

  const status = panel.querySelector('.subagent-status');
  if (status) {
    status.textContent = `${LABELS.running} (${tr('execution.cycle', { cycle })})`;
  }
  setChipText(panel, 'cycle', tr('execution.cycle', { cycle }));
}

function subagentCompletionLabel(panel: HTMLElement): string {
  const rawLabel = panel.dataset.finalStatusLabel || '';
  const normalizedLabel = rawLabel.trim().toLowerCase();
  if (normalizedLabel === 'skipped') return tr('execution.skipped');
  if (normalizedLabel === 'completed') return LABELS.completed;
  if (normalizedLabel === 'failed') return LABELS.failed;
  if (rawLabel) return rawLabel;
  if (panel.classList.contains('subagent-failed')) return LABELS.failed;
  if (panel.classList.contains('subagent-skipped')) return tr('execution.skipped');
  return LABELS.completed;
}

function subagentCompletionStatus(panel: HTMLElement): string {
  const label = subagentCompletionLabel(panel);
  const error = panel.dataset.finalError || '';
  if (error && panel.classList.contains('subagent-failed')) {
    return `${label}: ${error.slice(0, 60)}`;
  }

  const parts: string[] = [];
  if (panel.dataset.finalCycles) {
    parts.push(tr('execution.cyclesCount', { count: panel.dataset.finalCycles }));
  }
  if (panel.dataset.finalToolCalls) {
    parts.push(tr('execution.toolsCount', { count: panel.dataset.finalToolCalls }));
  }
  if (panel.dataset.finalDurationMs) {
    const duration = formatToolDuration(Number(panel.dataset.finalDurationMs));
    if (duration) parts.push(duration);
  }
  return parts.length ? `${label} (${parts.join(', ')})` : label;
}

export function refreshSubagentPanelsLanguage(): void {
  document.querySelectorAll<HTMLElement>('.subagent-panel').forEach((panel) => {
    const kicker = panel.querySelector<HTMLElement>('.subagent-kicker');
    if (kicker) kicker.textContent = LABELS.subagent;
    if (panel.classList.contains('subagent-modal-open')) {
      panel.setAttribute(
        'aria-label',
        [LABELS.subagent, panel.dataset.agent || tr('execution.subagent')].join(': '),
      );
    }

    const status = panel.querySelector<HTMLElement>('.subagent-status');
    if (status) {
      if (panel.classList.contains('subagent-active')) {
        const cycle = panel.dataset.currentCycle || '1';
        status.textContent = `${LABELS.running} (${tr('execution.cycle', { cycle })})`;
      } else {
        status.textContent = subagentCompletionStatus(panel);
      }
    }

    const stateChip = panel.querySelector<HTMLElement>('[data-subagent-chip="state"]');
    if (stateChip) {
      stateChip.textContent = panel.classList.contains('subagent-active')
        ? LABELS.running
        : subagentCompletionLabel(panel);
    }

    const cycleChip = panel.querySelector<HTMLElement>('[data-subagent-chip="cycle"]');
    if (cycleChip) {
      const cycle = panel.dataset.finalCycles || panel.dataset.currentCycle || '1';
      cycleChip.textContent = tr('execution.cycle', { cycle });
    }

    const badges = getToolBadges(panel);
    badges.forEach(refreshToolBadgeLanguage);
    const fallbackTools = panel.dataset.finalToolCalls
      ? Number(panel.dataset.finalToolCalls)
      : null;
    syncToolCount(panel, fallbackTools);

    const reasoningCard = getReasoningCard(panel);
    const reasoningMeta = getReasoningMeta(panel);
    if (reasoningCard?.dataset.reasoningActive === 'true' && reasoningMeta) {
      const cycle = panel.dataset.currentCycle || '1';
      reasoningMeta.textContent = `${tr('execution.cycle', { cycle })} / ${LABELS.thinking}`;
      reasoningMeta.title = reasoningMeta.textContent;
    }

    if (!panel.classList.contains('subagent-active')) renderSummary(panel);
    syncPanelActions(panel);
    syncSubagentModalPlaceholder(panel);
  });
}

export function appendSubagentToolOutput(
  ref: SubagentPanelRef,
  toolId,
  stream,
  chunk,
  toolName = '',
) {
  if (!chunk) return;

  const panel = resolvePanel(ref);
  if (!panel) return;

  let badge = findToolBadge(panel, toolId, {
    allowPendingEmptyId: true,
    toolName,
  });
  if (!badge) {
    addSubagentTool(ref, toolName || 'tool', toolId);
    badge = findToolBadge(panel, toolId, {
      allowPendingEmptyId: true,
      toolName,
    });
  }
  if (!badge || badge.dataset.toolHasResult === 'true') return;

  badge.dataset.toolLiveOutput = mergeToolLiveOutput(
    badge.dataset.toolLiveOutput || '',
    stream,
    chunk,
  );

  if (state.activeToolPanel === badge) {
    syncToolDrawer(badge);
  }
}

export function startSubagentReasoning(ref: SubagentPanelRef) {
  const panel = resolvePanel(ref);
  if (!panel) return;

  const card = ensureReasoningCard(panel);
  const body = getReasoningBody(panel);
  const meta = getReasoningMeta(panel);
  if (!card || !body) return;
  card.dataset.reasoningActive = 'true';

  if (!body._textNode) {
    body.textContent = '';
    body._textNode = document.createTextNode('');
    body.appendChild(body._textNode);
  }

  const cycleLabel = panel.querySelector('[data-subagent-chip="cycle"]')?.textContent?.trim() || '';
  if ((body._textNode.nodeValue || '').trim()) body._textNode.nodeValue += '\n\n';
  if (cycleLabel) body._textNode.nodeValue += `[${cycleLabel}]\n`;

  card.hidden = false;
  if (meta) {
    meta.textContent = cycleLabel ? `${cycleLabel} / ${LABELS.thinking}` : LABELS.thinking;
    meta.title = meta.textContent;
  }

  scrollDown();
}

export function appendSubagentReasoning(ref: SubagentPanelRef, content) {
  if (!content) return;

  const panel = resolvePanel(ref);
  if (!panel) return;

  const card = ensureReasoningCard(panel);
  const body = getReasoningBody(panel);
  if (!card || !body) return;

  if (!body._textNode) {
    body.textContent = '';
    body._textNode = document.createTextNode('');
    body.appendChild(body._textNode);
  }

  card.hidden = false;
  body._textNode.nodeValue += content;
  scrollDown();
}

export function finishSubagentReasoning(ref: SubagentPanelRef) {
  const panel = resolvePanel(ref);
  if (!panel) return;

  const meta = getReasoningMeta(panel);
  const body = getReasoningBody(panel);
  if (!meta || !body) return;
  const card = getReasoningCard(panel);
  if (card) card.dataset.reasoningActive = 'false';

  const rawText = body._textNode?.nodeValue || body.textContent || '';
  const preview = reasoningPreview(rawText);
  meta.textContent = preview;
  meta.title = rawText.trim() || LABELS.completed;
}

export function restoreSubagentHistorySnapshot(
  ref: SubagentPanelRef,
  snapshot: SubagentHistorySnapshot,
) {
  const panel = resolvePanel(ref);
  if (!panel || !snapshot) return;

  const reasoning = typeof snapshot.reasoning === 'string' ? snapshot.reasoning.trim() : '';
  if (reasoning) {
    const card = ensureReasoningCard(panel);
    const body = getReasoningBody(panel);
    const meta = getReasoningMeta(panel);
    if (card && body) {
      body.textContent = '';
      body._textNode = document.createTextNode(reasoning);
      body.appendChild(body._textNode);
      card.hidden = false;
      if (meta) {
        const preview = reasoningPreview(reasoning);
        meta.textContent = preview;
        meta.title = reasoning;
      }
    }
  }

  const tools = Array.isArray(snapshot.tools) ? snapshot.tools : [];
  for (const [index, tool] of tools.entries()) {
    const toolId = tool?.id || `${tool?.name || 'tool'}-${index}`;
    addSubagentTool(ref, tool?.name || 'tool', toolId, tool?.arguments || '');
    updateSubagentToolResult(
      ref,
      toolId,
      tool?.duration_ms,
      tool?.result,
      tool?.is_error === true,
      tool?.name || 'tool',
      tool?.images || [],
    );
  }

  finishSubagentPanel(
    ref,
    snapshot.success !== false,
    {
      cycles: snapshot.cycles,
      tool_calls: snapshot.tool_calls,
      duration_ms: snapshot.duration_ms,
      input_tokens: snapshot.input_tokens,
      output_tokens: snapshot.output_tokens,
      result_excerpt: snapshot.result_excerpt,
      error: snapshot.error,
    },
    { immediate: true },
  );
}

export function updateSubagentToolResult(
  ref: SubagentPanelRef,
  toolId,
  durationMs,
  result,
  isError = false,
  toolName = '',
  images: ImageAttachment[] = [],
) {
  const panel = resolvePanel(ref);
  if (!panel) return;

  let badge = findToolBadge(panel, toolId, {
    allowPendingEmptyId: true,
    toolName,
  });
  if (!badge) {
    addSubagentTool(ref, toolName || 'tool', toolId);
    badge = findToolBadge(panel, toolId, {
      allowPendingEmptyId: true,
      toolName,
    });
  }
  if (!badge) return;

  const durationLabel = formatToolDuration(durationMs);
  const stateLabel = `${isError ? LABELS.failed : LABELS.completed}${durationLabel ? ` / ${durationLabel}` : ''}`;
  const hasResult = typeof result === 'string' && result.trim().length > 0;
  const displayResult = hasResult ? result : isError ? LABELS.toolFailedNoOutput : '';
  const showResult = hasResult || isError;
  badge.dataset.toolState = isError ? 'failed' : 'completed';
  badge.dataset.toolDurationMs = durationMs == null ? '' : String(durationMs);
  syncToolBadgeDataset(
    badge,
    toolName || badge.dataset.toolName || 'tool',
    badge.dataset.toolArgs || '',
    displayResult,
    stateLabel,
    showResult,
    images,
  );
  badge.dataset.toolLiveOutput = '';
  updateToolBadgeState(badge, stateLabel, isError ? 'is-failed' : 'is-done');

  if (state.activeToolPanel === badge) {
    syncToolDrawer(badge);
  }

  syncToolCount(panel);
  syncPanelActions(panel);
  refreshExecutionStackForPanel(panel);

  if (isError) {
    pulseFocus(badge);
  }

  scrollDown();
}

export function finishSubagentPanel(
  ref: SubagentPanelRef,
  success,
  stats: SubagentStats,
  { immediate = false } = {},
) {
  const panel = resolvePanel(ref);
  if (!panel) return;

  panel.classList.remove('subagent-active');
  panel.classList.remove('subagent-done', 'subagent-failed', 'subagent-skipped');
  const normalizedStatusLabel = stats.status_label?.trim().toLowerCase();
  if (normalizedStatusLabel === 'skipped') {
    panel.classList.add('subagent-skipped');
  } else {
    panel.classList.add(success ? 'subagent-done' : 'subagent-failed');
  }

  panel.dataset.finalStatusLabel = stats.status_label || '';
  panel.dataset.finalError = stats.error || '';
  panel.dataset.finalCycles = stats.cycles == null ? '' : String(stats.cycles);
  panel.dataset.finalToolCalls = stats.tool_calls == null ? '' : String(stats.tool_calls);
  panel.dataset.finalDurationMs = stats.duration_ms == null ? '' : String(stats.duration_ms);
  storeSummaryData(panel, success, stats);

  const status = panel.querySelector('.subagent-status');
  if (status) status.textContent = subagentCompletionStatus(panel);

  const chipLabel = subagentCompletionLabel(panel);
  const chipClass =
    normalizedStatusLabel === 'skipped' ? 'is-muted' : success ? 'is-success' : 'is-error';
  setChipText(panel, 'state', chipLabel, chipClass);
  if (stats.cycles != null) {
    setChipText(panel, 'cycle', tr('execution.cycle', { cycle: stats.cycles }));
  }

  renderSummary(panel);
  syncToolCount(panel, stats.tool_calls ?? null);
  syncPanelActions(panel);
  syncSubagentModalPlaceholder(panel);
  refreshExecutionStackForPanel(panel);

  const collapsePanel = () => {
    const body = panel.querySelector('.subagent-body');
    const chevron = panel.querySelector('.chevron');
    if (body) animateCollapsibleSection(body, false);
    if (chevron) chevron.classList.remove('open');
  };

  if (immediate) {
    collapsePanel();
  }

  state.activeSubagentPanels.delete(panelKey(ref));
  if (ref && ref.agent) state.activeSubagentPanels.delete(ref.agent);
}

export function copySubagentSummary(button) {
  const panel = button.closest('.subagent-panel');
  if (!panel) return;
  void copyButtonText(button, summaryCopyText(panel), LABELS.copySummary);
}

export function openSubagentToolDrawer(button) {
  if (!button) return;
  pulseFocus(button);
  openToolDrawer(button, button);
}
