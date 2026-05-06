import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import {
  applyAutoTrace,
  applyTopLevelAutoTrace,
  clearActiveAutoTrace,
  setAutoDebugEnabled,
  updateAutoDebugToggleButton,
} from '../src/renderers/auto-trace.js';
import { dom, state } from '../src/state.js';
import type { AutoTraceEvent } from '../src/types.js';

function sampleTrace(overrides: Partial<AutoTraceEvent> = {}): AutoTraceEvent {
  return {
    type: 'auto_trace',
    round: 2,
    cycle: 4,
    phase: 'analyze',
    model: 'openai/gpt-4o-reasoner',
    provider: 'openai',
    selected_think: 'high',
    baseline_level: 'medium',
    baseline_reason: 'mid_loop_investigate',
    escalators: ['stagnation_streak'],
    dampeners: [],
    clamps: [],
    signals: {
      intent: 'investigate',
      user_msg_chars: 96,
      observation_strength: 'medium',
      tool_results_count: 2,
      tool_error_count: 1,
      summary_count: 1,
      summary_bytes: 4096,
      stagnation_streak: 3,
      error_streak: 1,
      task_pressure: 2,
      ready_to_finish: false,
      action_oriented: true,
      has_blocking_uncertainty: true,
      finish_deferral_count: 1,
      progress_made: false,
      retry_pattern: 'same_tool',
      error_kind: 'timeout',
      evidence_delta_quality: 'no_meaningful_progress',
    },
    ...overrides,
  };
}

describe('auto trace debug panel', () => {
  beforeEach(() => {
    document.body.innerHTML = `
      <div id="chat"></div>
      <button id="toggle-auto-debug-btn"></button>
    `;
    dom.chat = document.getElementById('chat') as HTMLElement;
    dom.toggleAutoDebugBtn = document.getElementById('toggle-auto-debug-btn') as HTMLButtonElement;
    state.autoFollowChat = true;
    state.bulkRenderingChat = false;
    state.autoDebugEnabled = false;
    state.latestAutoTrace = null;
    state.autoDebugRow = null;
    updateAutoDebugToggleButton();
  });

  afterEach(() => {
    clearActiveAutoTrace();
    state.autoDebugEnabled = false;
    state.latestAutoTrace = null;
    state.autoDebugRow = null;
    document.body.innerHTML = '';
    dom.chat = null;
    dom.toggleAutoDebugBtn = null;
  });

  it('stays hidden by default while caching the latest trace', () => {
    applyAutoTrace(sampleTrace());

    expect(state.latestAutoTrace?.selected_think).toBe('high');
    expect(state.autoDebugRow).toBeNull();
    expect(document.querySelector('[data-auto-trace-panel="true"]')).toBeNull();
    expect(dom.toggleAutoDebugBtn?.textContent).toBe('Auto Debug: Off');
  });

  it('renders the cached trace when the local debug toggle is enabled', () => {
    applyAutoTrace(sampleTrace());
    setAutoDebugEnabled(true);

    const panel = document.querySelector('[data-auto-trace-panel="true"]');
    expect(dom.toggleAutoDebugBtn?.textContent).toBe('Auto Debug: On');
    expect(panel?.textContent).toContain('selected=high');
    expect(panel?.textContent).toContain('baseline=medium');
    expect(panel?.textContent).toContain('retry=same_tool');
  });

  it('updates the existing panel when a newer trace arrives', () => {
    setAutoDebugEnabled(true);
    applyAutoTrace(sampleTrace());
    applyAutoTrace(
      sampleTrace({
        selected_think: 'xhigh',
        escalators: ['retry_same_args', 'repeated_finish_deferrals'],
      }),
    );

    const panels = document.querySelectorAll('[data-auto-trace-panel="true"]');
    expect(panels).toHaveLength(1);
    expect(panels[0].textContent).toContain('selected=xhigh');
    expect(panels[0].textContent).toContain('retry_same_args, repeated_finish_deferrals');
  });

  it('ignores sub-agent traces for the top-level debug panel', () => {
    setAutoDebugEnabled(true);
    applyAutoTrace(sampleTrace({ selected_think: 'high' }));
    applyTopLevelAutoTrace({
      ...sampleTrace({ selected_think: 'low' }),
      subagent: 'coder',
    });

    const panel = document.querySelector('[data-auto-trace-panel="true"]');
    expect(state.latestAutoTrace?.selected_think).toBe('high');
    expect(panel?.textContent).toContain('selected=high');
    expect(panel?.textContent).not.toContain('selected=low');
  });
});
