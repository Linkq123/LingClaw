import { beforeEach, describe, expect, it } from 'vitest';
import { dom, state } from '../src/state.js';
import {
  appendLiveReasoningText,
  buildHistoryReasoningPanel,
  createLiveReasoningPanel,
  finalizeOrDiscardLiveReasoningPanel,
  finalizeLiveReasoningPanel,
  reasoningTextForTest,
  summarizeReasoningText,
  syncReasoningPanelsDensity,
} from '../src/renderers/reasoning.js';
import { mountExecutionPanel } from '../src/renderers/execution-stack.js';
import { wrapInTimeline } from '../src/renderers/timeline.js';

describe('density-aware reasoning panels', () => {
  beforeEach(() => {
    document.body.innerHTML = '<div id="chat"></div>';
    dom.chat = document.getElementById('chat') as HTMLElement;
    state.activeExecutionStack = null;
    state.reasoningDensity = 'summary';
  });

  it('defaults history replay to a bounded Summary without inserting the raw tail', () => {
    const raw = `${'analysis '.repeat(50)}PRIVATE_RAW_TAIL`;
    const panel = buildHistoryReasoningPanel(raw);

    expect(panel.dataset.reasoningDensity).toBe('summary');
    expect(panel.textContent).not.toContain('PRIVATE_RAW_TAIL');
    expect(panel.querySelector('.reasoning-body')?.textContent?.length).toBeLessThanOrEqual(221);
    expect(reasoningTextForTest(panel)).toBe(raw);
    expect(panel.querySelector('.reasoning-status')?.getAttribute('title')).not.toContain(
      'PRIVATE_RAW_TAIL',
    );
  });

  it('renders Normal as derived trace metadata and Verbose as the full trace', () => {
    const raw = `${'head '.repeat(240)}TAIL_SENTINEL`;
    const panel = buildHistoryReasoningPanel(raw);
    document.body.appendChild(panel);

    state.reasoningDensity = 'normal';
    syncReasoningPanelsDensity();
    const normal = panel.querySelector('.reasoning-body')?.textContent || '';
    expect(normal).not.toContain('TAIL_SENTINEL');
    expect(normal).toContain('Reasoning trace retained');
    expect(normal.length).toBeLessThan(raw.length);

    state.reasoningDensity = 'verbose';
    syncReasoningPanelsDensity();
    expect(panel.querySelector('.reasoning-body')?.textContent).toBe(raw);
  });

  it('keeps live raw reasoning off-DOM in Summary while updating its accessible preview', () => {
    const panel = createLiveReasoningPanel();
    document.body.appendChild(panel);
    appendLiveReasoningText(panel, `${'working '.repeat(40)}LIVE_PRIVATE_TAIL`);

    expect(panel.textContent).not.toContain('LIVE_PRIVATE_TAIL');
    expect(reasoningTextForTest(panel)).toContain('LIVE_PRIVATE_TAIL');
    expect(panel.querySelector('.reasoning-body')?.getAttribute('aria-label')).toBe(
      'Concise reasoning summary',
    );
  });

  it('preserves the in-memory raw trace across density changes and finalization', () => {
    const panel = createLiveReasoningPanel();
    appendLiveReasoningText(panel, 'first line\n\nsecond line');
    expect(finalizeLiveReasoningPanel(panel)).toBe(true);
    expect(reasoningTextForTest(panel)).toBe('first line\n\nsecond line');
    expect(panel.querySelector('.reasoning-status')?.textContent).toContain('Reasoning trace');
    expect(panel.dataset.executionState).toBe('completed');
  });

  it('drops live reasoning panels that contain only whitespace', () => {
    const panel = createLiveReasoningPanel();
    appendLiveReasoningText(panel, '   \n  ');
    expect(finalizeLiveReasoningPanel(panel)).toBe(false);
  });

  it('removes the owning timeline wrapper when empty reasoning is discarded', () => {
    const panel = createLiveReasoningPanel();
    appendLiveReasoningText(panel, '   ');
    const wrapper = wrapInTimeline(panel, 'reasoning');
    document.body.appendChild(wrapper);

    expect(finalizeOrDiscardLiveReasoningPanel(panel)).toBe(false);
    expect(wrapper.isConnected).toBe(false);
  });

  it('keeps a non-empty panel when finalizing a replay-compatible legacy body', () => {
    const panel = document.createElement('div');
    panel.innerHTML = `
      <div class="reasoning-header"><span class="reasoning-status"></span></div>
      <div class="reasoning-body">legacy reasoning</div>
    `;
    expect(finalizeLiveReasoningPanel(panel)).toBe(true);
    expect(panel.querySelector('.reasoning-status')?.textContent).toContain('Reasoning trace');
  });

  it('normalizes whitespace and marks empty summaries', () => {
    expect(summarizeReasoningText('line one\n\nline two').previewText).toContain('Reasoning trace');
    expect(summarizeReasoningText('   \n  ').hasContent).toBe(false);
    expect(summarizeReasoningText('   \n  ').previewText).toBe('Completed');
  });

  it('keeps keyboard disclosure semantics for history and live panels', () => {
    const history = buildHistoryReasoningPanel('some text');
    const live = createLiveReasoningPanel();
    expect(history.querySelector('.reasoning-header')?.getAttribute('aria-expanded')).toBe('false');
    expect(live.querySelector('.reasoning-header')?.getAttribute('aria-expanded')).toBe('true');
    expect(history.querySelector('.reasoning-header')?.getAttribute('aria-controls')).toBeTruthy();
  });

  it.each(['summary', 'normal'] as const)(
    'never inserts a short raw sentinel into the DOM at %s density',
    (density) => {
      const raw = 'SHORT_UNIQUE_REASONING_SENTINEL';
      state.reasoningDensity = density;
      const panel = buildHistoryReasoningPanel(raw);
      mountExecutionPanel(panel, 'reasoning');
      const stack = panel.closest<HTMLElement>('.execution-stack')!;

      expect(reasoningTextForTest(panel)).toBe(raw);
      expect(panel.querySelector('.reasoning-body')?.textContent).not.toContain(raw);
      expect(panel.textContent).not.toContain(raw);
      expect(panel.outerHTML).not.toContain(raw);
      expect(panel.querySelector('.reasoning-status')?.getAttribute('title')).not.toContain(raw);
      expect(Object.values(panel.dataset).join(' ')).not.toContain(raw);
      expect(stack.textContent).not.toContain(raw);
      expect(
        stack.querySelector('.execution-stack-header')?.getAttribute('aria-label'),
      ).not.toContain(raw);
    },
  );

  it('allows the short raw trace only after Verbose is explicitly selected', () => {
    const raw = 'SHORT_VERBOSE_REASONING_SENTINEL';
    state.reasoningDensity = 'verbose';
    const panel = buildHistoryReasoningPanel(raw);

    expect(panel.querySelector('.reasoning-body')?.textContent).toBe(raw);
    expect(panel.textContent).toContain(raw);
  });
});
