/** Density-aware rendering for live and replayed reasoning. */
import { iconMarkup } from '../icons.js';
import { tr } from '../i18n.js';
import type { ReasoningDensity } from '../reasoningDensity.js';
import { state } from '../state.js';
import { refreshExecutionStackForPanel, removeExecutionPanel } from './execution-stack.js';
import { linkCollapsibleControl } from './timeline.js';

const reasoningText = new WeakMap<HTMLElement, string>();

function reasoningMetrics(raw: string) {
  const trimmed = String(raw ?? '').trim();
  const nonEmptyLines = trimmed ? trimmed.split(/\r?\n/).filter((line) => line.trim()).length : 0;
  const sections = trimmed
    ? trimmed.split(/\r?\n\s*\r?\n/).filter((part) => part.trim()).length
    : 0;
  return {
    hasContent: trimmed.length > 0,
    characters: Array.from(trimmed).length,
    lines: nonEmptyLines,
    sections,
    trimmed,
  };
}

function ensureDerivedRepresentation(raw: string, derived: string): string {
  return derived === raw.trim() ? `${derived} · ${tr('reasoning.derivedMarker')}` : derived;
}

export function displayReasoningText(raw: string, density: ReasoningDensity): string {
  if (density === 'verbose') return raw.trim();
  const metrics = reasoningMetrics(raw);
  if (!metrics.hasContent) return '';
  const derived =
    density === 'normal'
      ? tr('reasoning.normalBody', {
          sections: metrics.sections,
          lines: metrics.lines,
          characters: metrics.characters,
        })
      : tr('reasoning.summaryBody', { characters: metrics.characters });
  return ensureDerivedRepresentation(raw, derived);
}

function rawReasoningText(panel: HTMLElement): string {
  const stored = reasoningText.get(panel);
  if (stored !== undefined) return stored;
  const legacyBody = panel.querySelector('.reasoning-body') as
    | (HTMLElement & { _textNode?: Text | null })
    | null;
  return legacyBody?._textNode?.nodeValue || legacyBody?.textContent || '';
}

export function summarizeReasoningText(thinking: string) {
  const metrics = reasoningMetrics(thinking);
  const preview = metrics.hasContent
    ? tr('reasoning.traceSummary', { characters: metrics.characters })
    : tr('execution.completed');

  return {
    hasContent: metrics.hasContent,
    previewText: preview,
    titleText: preview,
    summaryText: metrics.hasContent
      ? displayReasoningText(thinking, 'summary')
      : tr('execution.completed'),
  };
}

function syncReasoningPresentation(panel: HTMLElement): void {
  const raw = rawReasoningText(panel);
  const summary = summarizeReasoningText(raw);
  const status = panel.querySelector<HTMLElement>('.reasoning-status');
  const body = panel.querySelector<HTMLElement>('.reasoning-body');
  const density = state.reasoningDensity;
  const live = panel.classList.contains('reasoning-active');

  panel.dataset.reasoningDensity = density;
  panel.dataset.executionAction = tr('execution.reasoningAction');
  panel.dataset.executionObject = summary.previewText;
  panel.dataset.executionResult = live
    ? tr('execution.reasoningActive')
    : tr('execution.completed');
  panel.dataset.executionState = live ? 'running' : 'completed';

  const densityBadge = panel.querySelector<HTMLElement>('.reasoning-density-badge');
  if (densityBadge) densityBadge.textContent = tr(`reasoning.${density}`);
  if (status) {
    status.textContent = live ? tr('execution.reasoningActive') : summary.previewText;
    status.title = summary.previewText;
  }
  if (body) {
    // Summary and Normal derive bounded text directly from the in-memory raw
    // value. The full trace only enters the DOM in the explicit Verbose mode.
    body.textContent = summary.hasContent ? displayReasoningText(raw, density) : '';
    body.setAttribute('aria-label', tr(`reasoning.${density}Aria`));
  }
  refreshExecutionStackForPanel(panel);
}

function buildReasoningPanel(thinking: string, live: boolean): HTMLElement {
  const panel = document.createElement('div');
  panel.className = `reasoning-panel${live ? ' reasoning-active' : ''}`;

  const header = document.createElement('button');
  header.type = 'button';
  header.className = 'reasoning-header';
  header.dataset.action = 'toggle-tool';
  header.setAttribute('aria-expanded', String(live));
  header.innerHTML = `
    <span class="reasoning-icon">${iconMarkup('reasoning')}</span>
    <span class="reasoning-label" data-i18n="common.reasoning">${tr('common.reasoning')}</span>
    <span class="reasoning-density-badge">${tr(`reasoning.${state.reasoningDensity}`)}</span>
    <span class="reasoning-status"></span>
    <span class="chevron${live ? ' open' : ''}">${iconMarkup('chevron-right')}</span>
  `;

  const body = document.createElement('div');
  body.className = `reasoning-body${live ? ' show' : ''}`;
  linkCollapsibleControl(header, body, 'reasoning-body');
  panel.append(header, body);
  reasoningText.set(panel, thinking);
  syncReasoningPresentation(panel);
  return panel;
}

export function createLiveReasoningPanel(): HTMLElement {
  return buildReasoningPanel('', true);
}

export function appendLiveReasoningText(panel: HTMLElement, chunk: string): void {
  reasoningText.set(panel, `${rawReasoningText(panel)}${chunk}`);
  syncReasoningPresentation(panel);
}

export function syncReasoningPanelsDensity(root: ParentNode = document): void {
  root
    .querySelectorAll<HTMLElement>('.reasoning-panel')
    .forEach((panel) => syncReasoningPresentation(panel));
}

export function finalizeLiveReasoningPanel(panel: HTMLElement): boolean {
  const summary = summarizeReasoningText(rawReasoningText(panel));
  if (!summary.hasContent) return false;
  panel.classList.remove('reasoning-active');
  syncReasoningPresentation(panel);
  return true;
}

export function finalizeOrDiscardLiveReasoningPanel(panel: HTMLElement): boolean {
  if (!finalizeLiveReasoningPanel(panel)) {
    removeExecutionPanel(panel);
    return false;
  }
  return true;
}

export function buildHistoryReasoningPanel(thinking: string): HTMLElement {
  return buildReasoningPanel(thinking, false);
}

export function reasoningTextForTest(panel: HTMLElement): string {
  return rawReasoningText(panel);
}
