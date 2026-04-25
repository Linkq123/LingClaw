import { describe, expect, it } from 'vitest';
import {
  buildHistoryReasoningPanel,
  finalizeOrDiscardLiveReasoningPanel,
  finalizeLiveReasoningPanel,
  summarizeReasoningText,
} from '../src/renderers/reasoning.js';
import { wrapInTimeline } from '../src/renderers/timeline.js';

describe('buildHistoryReasoningPanel', () => {
  it('sets body.textContent to the full thinking text', () => {
    const panel = buildHistoryReasoningPanel('deep reasoning step');
    const body = panel.querySelector<HTMLElement>('.reasoning-body');
    expect(body).not.toBeNull();
    expect(body!.textContent).toBe('deep reasoning step');
  });

  it('does not set a _textNode property on body (dead code guard)', () => {
    const panel = buildHistoryReasoningPanel('some thinking');
    const body = panel.querySelector('.reasoning-body') as HTMLElement & { _textNode?: unknown };
    expect(body._textNode).toBeUndefined();
  });

  it('sets statusEl.title to trimmed single-line summary text', () => {
    const panel = buildHistoryReasoningPanel('hello world');
    const statusEl = panel.querySelector<HTMLElement>('.reasoning-status');
    expect(statusEl?.title).toBe('hello world');
  });

  it('collapses runs of newlines into a single space for statusEl.title', () => {
    const panel = buildHistoryReasoningPanel('line one\n\nline two');
    const statusEl = panel.querySelector<HTMLElement>('.reasoning-status');
    // /\n+/ replaces one-or-more consecutive newlines with a single space
    expect(statusEl?.title).toBe('line one line two');
  });

  it('shows full preview text (≤60 chars) without ellipsis', () => {
    const panel = buildHistoryReasoningPanel('short text');
    const statusEl = panel.querySelector<HTMLElement>('.reasoning-status');
    expect(statusEl?.textContent).toBe('short text');
    expect(statusEl?.title).toBe('short text');
  });

  it('truncates statusEl.textContent with … and preserves full title for long thinking', () => {
    const long = 'a'.repeat(80);
    const panel = buildHistoryReasoningPanel(long);
    const statusEl = panel.querySelector<HTMLElement>('.reasoning-status');
    expect(statusEl?.textContent).toBe('a'.repeat(60) + '\u2026');
    expect(statusEl?.title).toBe(long);
  });

  it('does not add … when thinking is exactly 60 characters', () => {
    const exact = 'b'.repeat(60);
    const panel = buildHistoryReasoningPanel(exact);
    const statusEl = panel.querySelector<HTMLElement>('.reasoning-status');
    expect(statusEl?.textContent).toBe(exact);
    expect(statusEl?.title).toBe(exact);
  });

  it('falls back statusEl.title to 完成 when thinking trims to empty', () => {
    const panel = buildHistoryReasoningPanel('   \n  ');
    const statusEl = panel.querySelector<HTMLElement>('.reasoning-status');
    expect(statusEl?.title).toBe('\u5b8c\u6210');
    expect(statusEl?.textContent).toBe('\u5b8c\u6210');
  });

  it('marks whitespace-only reasoning as empty summary content', () => {
    const summary = summarizeReasoningText('   \n  ');
    expect(summary.hasContent).toBe(false);
    expect(summary.previewText).toBe('\u5b8c\u6210');
    expect(summary.titleText).toBe('\u5b8c\u6210');
  });

  it('drops live reasoning panels that only contain whitespace', () => {
    const panel = document.createElement('div');
    panel.innerHTML = `
      <div class="reasoning-header">
        <span class="reasoning-status">推理中</span>
      </div>
      <div class="reasoning-body"></div>
    `;

    const body = panel.querySelector('.reasoning-body') as HTMLElement & { _textNode?: Text };
    body._textNode = document.createTextNode('   \n  ');
    body.appendChild(body._textNode);

    expect(finalizeLiveReasoningPanel(panel)).toBe(false);
  });

  it('removes the timeline wrapper when discarding an empty live reasoning panel', () => {
    const panel = document.createElement('div');
    panel.innerHTML = `
      <div class="reasoning-header">
        <span class="reasoning-status">推理中</span>
      </div>
      <div class="reasoning-body"></div>
    `;

    const body = panel.querySelector('.reasoning-body') as HTMLElement & { _textNode?: Text };
    body._textNode = document.createTextNode('   \n  ');
    body.appendChild(body._textNode);

    const wrapper = wrapInTimeline(panel, 'reasoning');
    document.body.appendChild(wrapper);

    expect(finalizeOrDiscardLiveReasoningPanel(panel)).toBe(false);
    expect(wrapper.isConnected).toBe(false);
  });

  it('keeps live reasoning panels with real content and updates their summary', () => {
    const panel = document.createElement('div');
    panel.innerHTML = `
      <div class="reasoning-header">
        <span class="reasoning-status">推理中</span>
      </div>
      <div class="reasoning-body"></div>
    `;

    const body = panel.querySelector('.reasoning-body') as HTMLElement & { _textNode?: Text };
    const statusEl = panel.querySelector('.reasoning-status') as HTMLElement;
    body._textNode = document.createTextNode('first line\n\nsecond line');
    body.appendChild(body._textNode);

    expect(finalizeLiveReasoningPanel(panel)).toBe(true);
    expect(statusEl.textContent).toBe('first line second line');
    expect(statusEl.title).toBe('first line second line');
  });

  it('keeps the timeline wrapper when finalizing a non-empty live reasoning panel', () => {
    const panel = document.createElement('div');
    panel.innerHTML = `
      <div class="reasoning-header">
        <span class="reasoning-status">推理中</span>
      </div>
      <div class="reasoning-body"></div>
    `;

    const body = panel.querySelector('.reasoning-body') as HTMLElement & { _textNode?: Text };
    body._textNode = document.createTextNode('first line\n\nsecond line');
    body.appendChild(body._textNode);

    const wrapper = wrapInTimeline(panel, 'reasoning');
    document.body.appendChild(wrapper);

    expect(finalizeOrDiscardLiveReasoningPanel(panel)).toBe(true);
    expect(wrapper.isConnected).toBe(true);
  });

  it('panel has class reasoning-panel and header has class reasoning-header', () => {
    const panel = buildHistoryReasoningPanel('some text');
    expect(panel.classList.contains('reasoning-panel')).toBe(true);
    const header = panel.querySelector('.reasoning-header');
    expect(header).not.toBeNull();
    expect((header as HTMLElement).dataset.action).toBe('toggle-tool');
  });
});
