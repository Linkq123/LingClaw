import { dom, state } from '../state.js';
import type { SubagentHistorySnapshot } from '../types.js';
import {
  escHtml,
  formatToolDuration,
  formatTokenCount,
  formatDetailText,
  inlinePreview,
  stripDelegatedPromptRuntimeContext,
  pulseFocus,
  copyButtonText,
} from '../utils.js';
import { scrollDown } from '../scroll.js';
import { wrapInTimeline, animatePanelIn, animateCollapsibleSection } from './timeline.js';
import { pinReactStatusToBottom } from './react-status.js';

interface SubagentModalHost extends HTMLElement {
  _subagentModalParent?: HTMLElement | null;
  _subagentModalNextSibling?: ChildNode | null;
  _subagentModalPlaceholder?: HTMLElement | null;
}

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
  expandAll: 'Expand all',
  collapseAll: 'Collapse all',
  focusActive: 'Focus active',
  copySummary: 'Copy summary',
  taskPrompt: 'Task prompt',
  toolChain: 'Tool chain',
  toolDetails: 'Tool details',
  toolDetailsHint: 'Expand to inspect arguments and output',
  noToolCallsYet: 'No tool calls yet',
  noToolCallsInHistory: 'Tool details were not saved for this history replay.',
  noArguments: 'No arguments',
  noOutput: 'No output',
  toolFailedNoOutput: 'Tool failed without returning displayable output.',
  arguments: 'Arguments',
  output: 'Output',
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

function getToolRows(panel): HTMLElement[] {
  return Array.from((panel as Element).querySelectorAll('.subagent-tool-row')) as HTMLElement[];
}

function findToolRowById(panel, toolId) {
  if (!panel || !toolId) return null;
  for (const row of getToolRows(panel)) {
    if (row.dataset.toolId === toolId) return row;
  }
  return null;
}

function getToolBadges(panel): HTMLButtonElement[] {
  const trail = getToolTrail(panel);
  if (!trail) return [];
  return Array.from(trail.querySelectorAll<HTMLButtonElement>('.subagent-tool-pill'));
}

function findToolBadge(panel, toolId): HTMLButtonElement | null {
  if (!panel || !toolId) return null;
  return getToolBadges(panel).find((badge) => badge.dataset.toolId === toolId) || null;
}

function setToolRowExpanded(row, expand) {
  const details = row?.querySelector('.subagent-tool-details');
  const chevron = row?.querySelector('.subagent-tool-summary .chevron');
  if (!details) return;
  animateCollapsibleSection(details, expand);
  if (chevron) chevron.classList.toggle('open', expand);
}

function updateToolBadgeState(badge, stateLabel, tone) {
  if (!badge) return;
  badge.classList.remove('is-running', 'is-done', 'is-failed');
  if (tone) badge.classList.add(tone);
  const status = badge.querySelector('.subagent-tool-pill-state');
  if (status) status.textContent = stateLabel;
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
  badge.dataset.action = 'subagent-focus-tool';
  badge.dataset.toolId = toolId || '';
  badge.innerHTML = `
    <span class="subagent-tool-pill-index">${trail.childElementCount + 1}</span>
    <span class="subagent-tool-pill-name">${escHtml(toolName)}</span>
    <span class="subagent-tool-pill-state">${LABELS.running}</span>
  `;
  trail.appendChild(badge);
  return badge;
}

function syncToolOverview(panel, fallbackTotal: number | null = null, counts: ToolCounts | null = null) {
  if (!panel) return;

  const rows = counts ? null : getToolRows(panel);
  const total = counts ? counts.total : rows.length;
  const settled = counts
    ? counts.settled
    : rows.filter((row) => row.classList.contains('subagent-tool-done')).length;
  const failed = counts
    ? counts.failed
    : rows.filter((row) => row.classList.contains('subagent-tool-failed')).length;
  const running = counts
    ? counts.running
    : rows.filter((row) => row.classList.contains('subagent-tool-running')).length;
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

function findPriorityToolRow(panel) {
  const rows = getToolRows(panel);
  return (
    rows.find((row) => row.classList.contains('subagent-tool-running')) ||
    rows.find((row) => row.classList.contains('subagent-tool-failed')) ||
    rows[rows.length - 1] ||
    null
  );
}

function focusToolRow(row) {
  if (!row) return;
  setToolRowExpanded(row, true);
  pulseFocus(row);
  row.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
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
      '.subagent-summary:not(.hidden) .subagent-preview, .subagent-summary:not(.hidden) .subagent-error',
    )
    ?.textContent?.trim();
  const latestOutput = (
    Array.from((panel as Element).querySelectorAll('.subagent-tool-output-code')) as HTMLElement[]
  )
    .map((node) => node.textContent?.trim() || '')
    .filter(Boolean)
    .slice(-1)[0];
  const toolsUsed = getToolBadges(panel)
    .map((badge) => {
      const index = badge.querySelector('.subagent-tool-pill-index')?.textContent?.trim();
      const name = badge.querySelector('.subagent-tool-pill-name')?.textContent?.trim();
      return [index, name].filter(Boolean).join('. ');
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

function movePanelHostToBody(panel) {
  const wrapper = panel?.closest('.timeline-node') as SubagentModalHost | null;
  if (!wrapper || wrapper.classList.contains('subagent-modal-host')) return;
  const parent = wrapper.parentElement;
  if (!parent) return;

  const placeholder = document.createElement('div');
  placeholder.className = 'subagent-modal-placeholder';
  placeholder.style.height = `${Math.max(wrapper.getBoundingClientRect().height, 1)}px`;

  wrapper._subagentModalParent = parent;
  wrapper._subagentModalNextSibling = wrapper.nextSibling;
  wrapper._subagentModalPlaceholder = placeholder;
  parent.replaceChild(placeholder, wrapper);
  wrapper.classList.add('subagent-modal-host');
  document.body.appendChild(wrapper);
}

function restorePanelHost(panel) {
  const wrapper = panel?.closest('.timeline-node') as SubagentModalHost | null;
  if (!wrapper || !wrapper.classList.contains('subagent-modal-host')) return;

  const parent = wrapper._subagentModalParent;
  const nextSibling = wrapper._subagentModalNextSibling;
  const placeholder = wrapper._subagentModalPlaceholder;
  if (placeholder?.parentNode) {
    placeholder.parentNode.replaceChild(wrapper, placeholder);
  } else if (parent) {
    if (nextSibling && nextSibling.parentNode === parent) {
      parent.insertBefore(wrapper, nextSibling);
    } else {
      parent.appendChild(wrapper);
    }
  }

  wrapper.classList.remove('subagent-modal-host');
  wrapper._subagentModalParent = null;
  wrapper._subagentModalNextSibling = null;
  wrapper._subagentModalPlaceholder = null;
}

function syncPanelActions(panel) {
  if (!panel) return;

  const rows = getToolRows(panel);
  const toggleAllBtn = panel.querySelector('[data-action="subagent-toggle-all"]');
  const focusBtn = panel.querySelector('[data-action="subagent-focus-current"]');
  const copyBtn = panel.querySelector('[data-action="subagent-copy-summary"]');
  const allExpanded =
    rows.length > 0 &&
    rows.every((row) => row.querySelector('.subagent-tool-details')?.classList.contains('show'));

  if (toggleAllBtn) {
    toggleAllBtn.textContent = rows.length > 0 && allExpanded ? LABELS.collapseAll : LABELS.expandAll;
    toggleAllBtn.disabled = rows.length === 0;
  }
  if (focusBtn) {
    focusBtn.disabled = rows.length === 0;
  }
  if (copyBtn) {
    copyBtn.disabled = !summaryCopyText(panel);
  }
}

function syncToolCount(panel, fallbackTotal: number | null = null) {
  const rows = getToolRows(panel);
  const total = rows.length;
  const settled = rows.filter((row) => row.classList.contains('subagent-tool-done')).length;
  const failed = rows.filter((row) => row.classList.contains('subagent-tool-failed')).length;
  const running = rows.filter((row) => row.classList.contains('subagent-tool-running')).length;
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

  const bodyText = success
    ? (stats.result_excerpt || stats.result_preview || '').trim()
    : (stats.error || '').trim();

  const metricHtml = metrics
    .map((metric) => `<span class="subagent-summary-chip">${escHtml(metric)}</span>`)
    .join('');
  const contentHtml = bodyText
    ? `<pre class="${success ? 'subagent-preview' : 'subagent-error'}">${escHtml(bodyText)}</pre>`
    : '';

  if (!metricHtml && !contentHtml) {
    summary.classList.add('hidden');
    summary.innerHTML = '';
    return;
  }

  summary.innerHTML = `
    <div class="subagent-summary-head">
      <div class="subagent-summary-title">${success ? LABELS.executionSummary : LABELS.failureDetails}</div>
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
  let backdrop = document.getElementById('subagent-modal-backdrop');
  if (backdrop) return backdrop;
  backdrop = document.createElement('div');
  backdrop.id = 'subagent-modal-backdrop';
  backdrop.className = 'subagent-modal-backdrop';
  backdrop.dataset.action = 'close-subagent-modal';
  backdrop.hidden = true;
  document.body.appendChild(backdrop);
  return backdrop;
}

export function closeSubagentModal() {
  const panel = document.querySelector('.subagent-panel.subagent-modal-open');
  if (panel) {
    panel.classList.remove('subagent-modal-open');
    panel.querySelector('.subagent-header')?.setAttribute('aria-expanded', 'false');
    panel.querySelector('.subagent-modal-close')?.setAttribute('tabindex', '-1');
    panel.querySelector('.subagent-body')?.classList.remove('show');
    const body = panel.querySelector('.subagent-body') as HTMLElement | null;
    if (body) {
      body.style.height = '';
      body.setAttribute('inert', '');
    }
    restorePanelHost(panel);
  }
  const backdrop = document.getElementById('subagent-modal-backdrop');
  if (backdrop) backdrop.hidden = true;
}

export function openSubagentModal(trigger) {
  const panel = trigger?.closest?.('.subagent-panel');
  if (!panel) return;
  closeSubagentModal();
  const backdrop = ensureSubagentBackdrop();
  backdrop.hidden = false;
  movePanelHostToBody(panel);
  panel.classList.add('subagent-modal-open');
  panel.querySelector('.subagent-header')?.setAttribute('aria-expanded', 'true');
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

function panelKey(ref: SubagentPanelRef) {
  if (ref && ref.task_id) return ref.task_id;
  return (ref && ref.agent) || '';
}

export function createSubagentPanel(agentName, prompt, taskId) {
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
    <button type="button" class="panel-action-btn" data-action="subagent-toggle-all">${LABELS.expandAll}</button>
    <button type="button" class="panel-action-btn" data-action="subagent-focus-current">${LABELS.focusActive}</button>
    <button type="button" class="panel-action-btn" data-action="subagent-copy-summary" disabled>${LABELS.copySummary}</button>
  `;
  body.appendChild(actions);

  if (prompt) {
    const promptCard = document.createElement('div');
    promptCard.className = 'subagent-section-card';
    promptCard.innerHTML = `
      <div class="subagent-section-title">${LABELS.taskPrompt}</div>
      <div class="subagent-prompt">${escHtml(displayPrompt)}</div>
    `;
    body.appendChild(promptCard);
  }

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

  const toolListSection = document.createElement('div');
  toolListSection.className = 'subagent-section-card subagent-tool-list-section';
  toolListSection.innerHTML = `
    <div class="subagent-section-head">
      <div class="subagent-section-title">${LABELS.toolDetails}</div>
      <div class="subagent-section-meta">${LABELS.toolDetailsHint}</div>
    </div>
  `;
  body.appendChild(toolListSection);

  const toolList = document.createElement('div');
  toolList.className = 'subagent-tool-list';
  toolListSection.appendChild(toolList);

  const summary = document.createElement('div');
  summary.className = 'subagent-summary hidden';
  body.appendChild(summary);

  panel.appendChild(header);
  panel.appendChild(body);

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

  state.activeSubagentPanels.set(panelKey({ task_id: taskId, agent: agentName }), panel);
  syncToolOverview(panel);
  syncPanelActions(panel);
}

export function addSubagentTool(ref: SubagentPanelRef, toolName, toolId, toolArgs = '') {
  const panel = resolvePanel(ref);
  if (!panel) return;

  const toolList = panel.querySelector('.subagent-tool-list');
  if (!toolList) return;

  const formattedArgs = formatDetailText(toolArgs);
  const row = document.createElement('div');
  row.className = 'subagent-tool-row subagent-tool-running';
  if (toolId) row.dataset.toolId = toolId;
  row.dataset.toolName = toolName;
  row.innerHTML = `
    <div class="subagent-tool-summary" data-action="toggle-tool">
      <span class="subagent-tool-icon">&#9881;</span>
      <span class="subagent-tool-main">
        <span class="subagent-tool-name">${escHtml(toolName)}</span>
        <span class="subagent-tool-preview">${escHtml(inlinePreview(formattedArgs || LABELS.noArguments))}</span>
      </span>
      <span class="subagent-tool-status">${LABELS.running}</span>
      <span class="chevron">&#9656;</span>
    </div>
    <div class="subagent-tool-details">
      <div class="subagent-tool-section">
        <div class="subagent-tool-section-title">${LABELS.arguments}</div>
        <pre class="subagent-tool-code">${escHtml(formattedArgs || LABELS.noArguments)}</pre>
      </div>
      <div class="subagent-tool-section subagent-tool-output" hidden>
        <div class="subagent-tool-section-title">${LABELS.output}</div>
        <pre class="subagent-tool-code subagent-tool-output-code"></pre>
      </div>
    </div>
  `;
  toolList.appendChild(row);
  const badge = ensureToolBadge(panel, toolId, toolName);
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

  let row = null;
  for (const candidate of panel.querySelectorAll('.subagent-tool-row')) {
    if ((candidate as HTMLElement).dataset.toolId === toolId) {
      row = candidate as HTMLElement;
      break;
    }
  }

  if (!row) {
    addSubagentTool(ref, toolName || 'tool', toolId);
    row =
      Array.from(panel.querySelectorAll('.subagent-tool-row')).find(
        (candidate) => (candidate as HTMLElement).dataset.toolId === toolId,
      ) || null;
  }
  if (!row) return;

  row.classList.remove('subagent-tool-running');
  row.classList.add('subagent-tool-done');
  row.classList.toggle('subagent-tool-failed', isError);

  const statusEl = row.querySelector('.subagent-tool-status');
  if (statusEl) {
    const label = formatToolDuration(durationMs);
    statusEl.textContent = `${isError ? LABELS.failed : LABELS.completed}${label ? ` / ${label}` : ''}`;
  }

  const hasResult = typeof result === 'string';
  const formattedResult = hasResult ? formatDetailText(result) : '';
  const previewEl = row.querySelector('.subagent-tool-preview');
  if (previewEl && formattedResult) {
    previewEl.textContent = inlinePreview(formattedResult, 120);
  }

  const outputSection = row.querySelector('.subagent-tool-output');
  const outputCode = row.querySelector('.subagent-tool-output-code');
  if (outputSection && outputCode && (hasResult || isError)) {
    outputCode.textContent =
      formattedResult || (isError ? LABELS.toolFailedNoOutput : LABELS.noOutput);
    (outputSection as HTMLElement).hidden = false;
  }

  const badge = findToolBadge(panel, toolId);
  updateToolBadgeState(
    badge,
    isError ? LABELS.failed : LABELS.completed,
    isError ? 'is-failed' : 'is-done',
  );

  syncToolCount(panel);
  syncPanelActions(panel);

  if (isError) {
    setToolRowExpanded(row, true);
    pulseFocus(row);
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
  panel.classList.add(success ? 'subagent-done' : 'subagent-failed');

  const status = panel.querySelector('.subagent-status');
  if (status) {
    if (success) {
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

  setChipText(panel, 'state', success ? LABELS.completed : LABELS.failed, success ? 'is-success' : 'is-error');
  if (stats.cycles != null) setChipText(panel, 'cycle', `Cycle ${stats.cycles}`);

  renderSummary(panel, success, stats);
  syncToolCount(panel, stats.tool_calls ?? null);
  syncPanelActions(panel);

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

export function toggleSubagentTools(button) {
  const panel = button.closest('.subagent-panel');
  if (!panel) return;

  const rows = getToolRows(panel);
  if (rows.length === 0) return;
  const shouldExpand = rows.some(
    (row) => !row.querySelector('.subagent-tool-details')?.classList.contains('show'),
  );
  rows.forEach((row) => setToolRowExpanded(row, shouldExpand));
  syncPanelActions(panel);
}

export function focusSubagentCurrent(button) {
  const panel = button.closest('.subagent-panel');
  if (!panel) return;

  const target = findPriorityToolRow(panel);
  if (!target) return;
  focusToolRow(target);
}

export function copySubagentSummary(button) {
  const panel = button.closest('.subagent-panel');
  if (!panel) return;
  void copyButtonText(button, summaryCopyText(panel), LABELS.copySummary);
}

export function focusSubagentTool(button) {
  const panel = button.closest('.subagent-panel');
  if (!panel) return;

  const row = findToolRowById(panel, button.dataset.toolId || '');
  if (!row) return;
  focusToolRow(row);
}
