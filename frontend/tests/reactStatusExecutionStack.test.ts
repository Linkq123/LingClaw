import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { dom, state } from '../src/state.js';
import { completeExecutionStack, mountExecutionPanel } from '../src/renderers/execution-stack.js';
import {
  clearReactStatus,
  requestClearReactStatus,
  setReactActTool,
  showReactStatus,
} from '../src/renderers/react-status.js';

describe('ReAct status in the execution stack', () => {
  beforeEach(() => {
    document.body.innerHTML = '<div id="chat"></div>';
    dom.chat = document.getElementById('chat') as HTMLElement;
    state.activeExecutionStack = null;
    state.executionRunSequence = 1;
    state.activeExecutionRunId = 1;
    state.reactStatusRow = null;
    state.reactStatusPhase = '';
    state.reactStatusCycle = null;
    state.reactStatusToolName = '';
    state.reactStatusElapsedMs = 0;
    state.reactPhaseQueue = [];
    state.reactPhaseTimer = 0;
    state.reactPendingClear = false;
    state.showReasoning = true;
    state.showTools = true;
    state.autoFollowChat = false;
    mountExecutionPanel(document.createElement('div'), 'tool');
  });

  afterEach(() => {
    vi.useRealTimers();
    clearReactStatus();
    state.activeExecutionStack = null;
    dom.chat = null;
    document.body.replaceChildren();
  });

  it('mounts phase progress as a unified run step, never as a chat message', () => {
    showReactStatus('act', 2);
    setReactActTool('read_file', 1400);

    const status = state.reactStatusRow;
    expect(status?.closest('.execution-stack')).toBe(state.activeExecutionStack);
    expect(status?.closest('.execution-step--react')).not.toBeNull();
    expect(document.querySelector('.msg-row.react-status-row')).toBeNull();
    expect(status?.textContent).toContain('read_file');
    expect(status?.dataset.executionObject).toContain('read_file');
  });

  it('removes only the ReAct step when a phase clears', () => {
    showReactStatus('analyze', 1);
    clearReactStatus();

    expect(document.querySelector('.execution-step--react')).toBeNull();
    expect(document.querySelector('.execution-stack')).not.toBeNull();
  });

  it('does not reuse or later clear the ReAct node of a completed run', () => {
    vi.useFakeTimers();
    showReactStatus('act', 1);
    setReactActTool('read_file', 900);
    const oldRow = state.reactStatusRow;
    const oldStack = state.activeExecutionStack;
    requestClearReactStatus();
    completeExecutionStack({ status: 'completed' });

    // Mirrors a new `start` arriving before the old minimum-visible timer.
    clearReactStatus();
    state.executionRunSequence += 1;
    state.activeExecutionRunId = state.executionRunSequence;
    mountExecutionPanel(document.createElement('div'), 'tool');
    showReactStatus('analyze', 1);
    setReactActTool('write_file', 0);
    const newRow = state.reactStatusRow;
    const newStack = state.activeExecutionStack;

    expect(newRow).not.toBe(oldRow);
    expect(newStack).not.toBe(oldStack);
    expect(oldRow?.isConnected).toBe(false);
    expect(newRow?.closest('.execution-stack')).toBe(newStack);
    expect(document.querySelectorAll('.execution-stack')).toHaveLength(2);

    vi.advanceTimersByTime(2_000);
    expect(state.reactStatusRow).toBe(newRow);
    expect(newRow?.isConnected).toBe(true);
    expect(newRow?.closest('.execution-stack')).toBe(newStack);
  });

  it('does not create another stack while draining queued phases after a fast run completes', () => {
    vi.useFakeTimers();
    showReactStatus('analyze', 0);
    showReactStatus('act', 0);
    showReactStatus('observe', 0);
    const completedStack = state.activeExecutionStack;
    requestClearReactStatus();
    completeExecutionStack({ status: 'completed' });
    state.activeExecutionRunId = 0;

    vi.advanceTimersByTime(650);

    expect(document.querySelectorAll('.execution-stack')).toHaveLength(1);
    expect(completedStack?.dataset.executionStatus).toBe('completed');
    expect(state.reactStatusRow).toBeNull();
    expect(document.querySelector('.execution-stack[data-execution-status="running"]')).toBeNull();
  });
});
