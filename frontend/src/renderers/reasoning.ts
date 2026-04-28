/**
 * Builds a static history reasoning panel for a completed thinking block.
 * This is the history-replay path — not the live streaming path
 * (which is handled incrementally in handlers/stream.ts via flushReasoningText).
 */
import { removeTimelinePanel } from './timeline.js';

export function summarizeReasoningText(thinking: string) {
  const summaryText = String(thinking ?? '')
    .trim()
    .replace(/\n+/g, ' ');
  const preview = summaryText.substring(0, 60);

  return {
    hasContent: summaryText.length > 0,
    previewText: preview ? preview + (summaryText.length > 60 ? '…' : '') : '完成',
    titleText: summaryText || '完成',
  };
}

export function finalizeLiveReasoningPanel(panel: HTMLElement): boolean {
  const statusEl = panel.querySelector('.reasoning-status') as Element | null;
  const body = panel.querySelector('.reasoning-body') as
    | (Element & {
        _textNode?: Text | null;
      })
    | null;
  const rawText = body?._textNode?.nodeValue || body?.textContent || '';
  const summary = summarizeReasoningText(rawText);

  if (!summary.hasContent) {
    return false;
  }

  if (statusEl) {
    statusEl.textContent = summary.previewText;
    statusEl.title = summary.titleText;
  }

  return true;
}

export function finalizeOrDiscardLiveReasoningPanel(panel: HTMLElement): boolean {
  if (!finalizeLiveReasoningPanel(panel)) {
    removeTimelinePanel(panel);
    return false;
  }

  return true;
}

export function buildHistoryReasoningPanel(thinking: string): HTMLElement {
  const panel = document.createElement('div');
  panel.className = 'reasoning-panel';

  const header = document.createElement('div');
  header.className = 'reasoning-header';
  header.dataset.action = 'toggle-tool';
  header.innerHTML = `
    <span class="reasoning-icon">\ud83d\udcad</span>
    <span class="reasoning-label">Reasoning</span>
    <span class="reasoning-status"></span>
    <span class="chevron">\u25b8</span>
  `;

  const statusEl = header.querySelector('.reasoning-status') as Element | null;
  const summary = summarizeReasoningText(thinking);
  if (statusEl) {
    statusEl.textContent = summary.previewText;
    statusEl.title = summary.titleText;
  }

  const body = document.createElement('div');
  body.className = 'reasoning-body';
  body.textContent = thinking;

  panel.appendChild(header);
  panel.appendChild(body);

  return panel;
}
