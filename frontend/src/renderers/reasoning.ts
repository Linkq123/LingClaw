/**
 * Builds a static history reasoning panel for a completed thinking block.
 * This is the history-replay path — not the live streaming path
 * (which is handled incrementally in handlers/stream.ts via flushReasoningText).
 */
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

  const statusEl = header.querySelector<HTMLElement>('.reasoning-status');
  if (statusEl) {
    const summaryText = thinking.trim().replace(/\n+/g, ' ');
    const preview = summaryText.substring(0, 60);
    statusEl.textContent = preview
      ? preview + (summaryText.length > 60 ? '\u2026' : '')
      : '\u5b8c\u6210';
    statusEl.title = summaryText || '\u5b8c\u6210';
  }

  const body = document.createElement('div');
  body.className = 'reasoning-body';
  body.textContent = thinking;

  panel.appendChild(header);
  panel.appendChild(body);

  return panel;
}
