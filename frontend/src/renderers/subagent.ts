import { dom, state } from '../state.js';
import type { SubagentHistorySnapshot } from '../types.js';
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
import { wrapInTimeline, animatePanelIn, animateCollapsibleSection } from './timeline.js';
import { pinReactStatusToBottom } from './react-status.js';
import { closeToolDrawer, openToolDrawer, syncToolDrawer } from './tools.js';
import {
  ensureModalBackdrop,
  moveModalHostToBody,
  restoreModalHost,
  syncModalHostPlaceholder,
} from './modalHost.js';

type SubagentPanelRef = {
  task_id?: string;
  agent?: string;
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
  summary_tone?: 'success' | 'error' | 'muted';
  summary_body?: string;
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
  subagent: 'Sub-agent',
  running: 'Running',
  thinking: 'Thinking...',
  completed: 'Completed',
  failed: 'Failed',
  waiting: 'Waiting',
  reasoning: 'Reasoning',
  copySummary: 'Copy summary',
  taskPrompt: 'Task prompt',
  toolChain: 'Tool chain',
  noToolCallsYet: 'No tool calls yet',
  noToolCallsInHistory: 'Tool details were not saved for this history replay.',
  noArguments: 'No arguments',
  toolFailedNoOutput: 'Tool failed without returning displayable output.',
  executionSummary: 'Execution summary',
  failureDetails: 'Failure details',
  duration: 'Duration',
} as const;

function pluralize(count: number, singular: string, plural = `${singular}s`) {
  return count === 1 ? singular : plural;
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

function ensurePromptCard(panel) {
  if (!panel) return null;

  const body = panel.querySelector('.subagent-body');
  if (!body) return null;

  let card = body.querySelector('[data-subagent-prompt-card]');
  if (card) return card;

  card = document.createElement('div');
  card.className = 'subagent-section-card';
  card.dataset.subagentPromptCard = 'true';
  card.innerHTML = `
    <div class="subagent-section-title">${LABELS.taskPrompt}</div>
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
      <div class="subagent-section-title">${LABELS.reasoning}</div>
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
  const matchesToolName = (badge) =>
    !toolName || (badge.dataset.toolName || '') === toolName;

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

function findToolBadge(panel, toolId, { allowPendingEmptyId = false, toolName = '' } = {}): HTMLButtonElement | null {
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

function syncToolBadgeDataset(
  badge,
  toolName,
  toolArgs = '',
  toolResult = '',
  toolStatus: string = LABELS.running,
  hasResult: boolean = false,
) {
  if (!badge) return;
  const formattedArgs = formatDetailText(toolArgs || '');
  const formattedResult = formatDetailText(toolResult || '');
  badge.dataset.toolName = toolName || 'tool';
  badge.dataset.toolArgs = formattedArgs || LABELS.noArguments;
  badge.dataset.toolResult = formattedResult;
  badge.dataset.toolHasResult = hasResult ? 'true' : 'false';
  badge.dataset.toolStatus = toolStatus;
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
  badge.innerHTML = `
    <span class="subagent-tool-pill-index">${trail.childElementCount + 1}</span>
    <span class="subagent-tool-pill-name">${escHtml(toolName)}</span>
    <span class="subagent-tool-pill-state">${LABELS.running}</span>
  `;
  syncToolBadgeDataset(badge, toolName, '', '', LABELS.running, false);
  trail.appendChild(badge);
  return badge;
}

function syncToolOverview(panel, fallbackTotal: number | null = null, counts: ToolCounts | null = null) {
  if (!panel) return;

  const badges = counts ? null : getToolBadges(panel);
  const total = counts ? counts.total : badges.length;
  const settled = counts
    ? counts.settled
    : badges.filter(
        (badge) =>
          badge.classList.contains('is-done') || badge.classList.contains('is-failed'),
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
          ? `History replay preserved ${fallbackTotal} ${pluralize(fallbackTotal, 'tool call')}.`
          : LABELS.noToolCallsYet;
    } else {
      const parts = [`${total} ${pluralize(total, 'call')}`];
      if (running) parts.push(`${running} running`);
      if (succeeded) parts.push(`${succeeded} completed`);
      if (failed) parts.push(`${failed} failed`);
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
    ? `${settled}/${total} tools`
    : fallbackTotal != null
      ? `${fallbackTotal} tools`
      : '0 tools';
  setChipText(panel, 'tools', displayText);
  syncToolOverview(panel, fallbackTotal, { total, settled, failed, running });
}

function renderSummary(panel, success, stats: SubagentStats = {}) {
  const summary = panel.querySelector('.subagent-summary');
  if (!summary) return;

  const metrics: string[] = [];
  if (stats.cycles != null) metrics.push(`Cycles ${stats.cycles}`);
  if (stats.tool_calls != null) metrics.push(`Tools ${stats.tool_calls}`);
  if (stats.duration_ms != null) {
    const duration = formatToolDuration(stats.duration_ms);
    if (duration) metrics.push(`${LABELS.duration} ${duration}`);
  }
  if (stats.input_tokens != null || stats.output_tokens != null) {
    const tokens: string[] = [];
    if (stats.input_tokens != null) tokens.push(`In ${formatTokenCount(stats.input_tokens)}`);
    if (stats.output_tokens != null) tokens.push(`Out ${formatTokenCount(stats.output_tokens)}`);
    if (tokens.length) metrics.push(tokens.join(' / '));
  }

  const bodyText = String(
    stats.summary_body ??
      (success ? stats.result_excerpt || stats.result_preview || '' : stats.error || ''),
  ).trim();
  const titleText =
    stats.summary_title || (success ? LABELS.executionSummary : LABELS.failureDetails);
  const tone = stats.summary_tone || (success ? 'success' : 'error');
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
  if (ref && ref.agent && state.activeSubagentPanels.has(ref.agent)) {
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
  return panel.closest('.timeline-node, .subagent-modal-anchor') as HTMLElement | null;
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
  if (panel) {
    if (state.activeToolPanel && panel.contains(state.activeToolPanel)) {
      closeToolDrawer();
    }
    panel.classList.remove('subagent-modal-open');
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
  }
  const backdrop = document.getElementById('subagent-modal-backdrop');
  if (backdrop) backdrop.hidden = true;
}

export function openSubagentPanelModal(panel) {
  if (!panel) return;
  closeSubagentModal();
  closeToolDrawer();
  const backdrop = ensureSubagentBackdrop();
  backdrop.hidden = false;
  const host = resolveSubagentModalHost(panel);
  moveModalHostToBody(host, {
    hostClass: 'subagent-modal-host',
    placeholderClass: 'subagent-modal-placeholder',
  });
  panel.classList.add('subagent-modal-open');
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
  openSubagentPanelModal(panel);
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
  if (taskId) panel.dataset.taskId = taskId;

  const header = document.createElement('div');
  header.className = 'subagent-header';
  header.dataset.action = 'open-subagent-modal';
  header.setAttribute('role', 'button');
  header.setAttribute('tabindex', '0');
  header.setAttribute('aria-expanded', 'false');
  header.innerHTML = `
    <span class="subagent-icon">&#10022;</span>
    <span class="subagent-head-copy">
      <span class="subagent-kicker">${LABELS.subagent}</span>
      <span class="subagent-label">${escHtml(agentName)}</span>
    </span>
    <span class="subagent-status">${LABELS.running}</span>
    <span class="chevron">&#9656;</span>
    <button type="button" class="subagent-modal-close" data-action="close-subagent-modal" aria-label="Close sub-agent details" tabindex="-1">&times;</button>
  `;

  const body = document.createElement('div');
  body.className = 'subagent-body';
  body.setAttribute('inert', '');

  const meta = document.createElement('div');
  meta.className = 'subagent-meta';
  meta.innerHTML = `
    <span class="subagent-chip is-live" data-subagent-chip="state">${LABELS.running}</span>
    <span class="subagent-chip" data-subagent-chip="cycle">Cycle 1</span>
    <span class="subagent-chip" data-subagent-chip="tools">0 tools</span>
  `;
  body.appendChild(meta);

  const actions = document.createElement('div');
  actions.className = 'panel-actions subagent-actions';
  actions.innerHTML = `
    <button type="button" class="panel-action-btn" data-action="subagent-copy-summary" disabled>${LABELS.copySummary}</button>
  `;
  body.appendChild(actions);

  const toolOverview = document.createElement('div');
  toolOverview.className = 'subagent-section-card subagent-tools-overview';
  toolOverview.innerHTML = `
    <div class="subagent-section-head">
      <div class="subagent-section-title">${LABELS.toolChain}</div>
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
  const panel = buildSubagentPanel(agentName, prompt, taskId);

  const currentRow = state.currentMsg ? state.currentMsg.closest('.msg-row') : null;
  const wrapper = wrapInTimeline(panel, 'subagent');
  if (currentRow) {
    dom.chat.insertBefore(wrapper, currentRow);
  } else {
    dom.chat.appendChild(wrapper);
  }
  pinReactStatusToBottom();
  animatePanelIn(panel);
  scrollDown();

  registerSubagentPanel(panel, taskId, agentName);
}

export function updateSubagentPrompt(ref: SubagentPanelRef, prompt) {
  const panel = resolvePanel(ref);
  if (!panel) return;

  const displayPrompt = stripDelegatedPromptRuntimeContext(prompt || '');
  if (!displayPrompt) return;

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
  scrollDown();
}

export function updateSubagentProgress(ref: SubagentPanelRef, cycle) {
  const panel = resolvePanel(ref);
  if (!panel) return;

  const status = panel.querySelector('.subagent-status');
  if (status) {
    status.textContent = `${LABELS.running} (cycle ${cycle})`;
  }
  setChipText(panel, 'cycle', `Cycle ${cycle}`);
}

export function startSubagentReasoning(ref: SubagentPanelRef) {
  const panel = resolvePanel(ref);
  if (!panel) return;

  const card = ensureReasoningCard(panel);
  const body = getReasoningBody(panel);
  const meta = getReasoningMeta(panel);
  if (!card || !body) return;

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

  const rawText = body._textNode?.nodeValue || body.textContent || '';
  const preview = reasoningPreview(rawText);
  meta.textContent = preview;
  meta.title = rawText.trim() || LABELS.completed;
}

export function restoreSubagentHistorySnapshot(ref: SubagentPanelRef, snapshot: SubagentHistorySnapshot) {
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
  syncToolBadgeDataset(
    badge,
    toolName || badge.dataset.toolName || 'tool',
    badge.dataset.toolArgs || '',
    displayResult,
    stateLabel,
    showResult,
  );
  updateToolBadgeState(
    badge,
    stateLabel,
    isError ? 'is-failed' : 'is-done',
  );

  if (state.activeToolPanel === badge) {
    syncToolDrawer(badge);
  }

  syncToolCount(panel);
  syncPanelActions(panel);

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

  const status = panel.querySelector('.subagent-status');
  if (status) {
    if (stats.status_label) {
      const parts: string[] = [];
      if (stats.cycles != null) parts.push(`${stats.cycles} cycles`);
      if (stats.tool_calls != null) parts.push(`${stats.tool_calls} tools`);
      if (stats.duration_ms != null) {
        const dur = formatToolDuration(stats.duration_ms);
        if (dur) parts.push(dur);
      }
      status.textContent = parts.length
        ? `${stats.status_label} (${parts.join(', ')})`
        : stats.status_label;
    } else if (success) {
      const parts: string[] = [];
      if (stats.cycles != null) parts.push(`${stats.cycles} cycles`);
      if (stats.tool_calls != null) parts.push(`${stats.tool_calls} tools`);
      if (stats.duration_ms != null) {
        const dur = formatToolDuration(stats.duration_ms);
        if (dur) parts.push(dur);
      }
      status.textContent = parts.length
        ? `${LABELS.completed} (${parts.join(', ')})`
        : LABELS.completed;
    } else {
      status.textContent = stats.error
        ? `${LABELS.failed}: ${stats.error.slice(0, 60)}`
        : LABELS.failed;
    }
  }

  const chipLabel = stats.status_label || (success ? LABELS.completed : LABELS.failed);
  const chipClass =
    normalizedStatusLabel === 'skipped' ? 'is-muted' : success ? 'is-success' : 'is-error';
  setChipText(panel, 'state', chipLabel, chipClass);
  if (stats.cycles != null) setChipText(panel, 'cycle', `Cycle ${stats.cycles}`);

  renderSummary(panel, success, stats);
  syncToolCount(panel, stats.tool_calls ?? null);
  syncPanelActions(panel);
  syncSubagentModalPlaceholder(panel);

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
  openToolDrawer(button);
}
