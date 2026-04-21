import { dom, state } from '../state.js';
import {
  MIN_REACT_ANALYZE_VISIBLE_MS,
  MIN_REACT_ACT_VISIBLE_MS,
  MIN_REACT_OBSERVE_VISIBLE_MS,
  MAX_REACT_QUEUED_PHASES,
} from '../constants.js';
import { reactPhaseLabel, hideWelcome } from '../utils.js';
import { scrollDown } from '../scroll.js';

export function pinReactStatusToBottom() {
  if (!state.reactStatusRow?.isConnected) return;
  if (dom.chat.lastElementChild === state.reactStatusRow) return;
  dom.chat.appendChild(state.reactStatusRow);
}

export function renderReactStatus() {
  if (!state.reactStatusRow) return;
  const card = state.reactStatusRow.querySelector<HTMLElement>('.react-status-card');
  const phase = state.reactStatusRow.querySelector<HTMLElement>('.react-status-phase');
  const cycle = state.reactStatusRow.querySelector<HTMLElement>('.react-status-cycle');
  const detail = state.reactStatusRow.querySelector<HTMLElement>('.react-status-detail');
  const detailTool = state.reactStatusRow.querySelector<HTMLElement>('.react-status-tool');
  const detailTime = state.reactStatusRow.querySelector<HTMLElement>('.react-status-time');
  if (!card || !phase || !cycle || !detail || !detailTool || !detailTime) return;
  card.dataset.phase = state.reactStatusPhase || 'analyze';
  phase.textContent = reactPhaseLabel(state.reactStatusPhase);
  cycle.textContent = Number.isInteger(state.reactStatusCycle)
    ? `cycle ${state.reactStatusCycle}`
    : '';
  if (state.reactStatusPhase === 'act' && state.reactStatusToolName) {
    const seconds = Math.max(1, Math.floor((state.reactStatusElapsedMs || 0) / 1000));
    detailTool.textContent = state.reactStatusToolName;
    detailTime.textContent = `${seconds}s`;
    detail.hidden = false;
  } else {
    detailTool.textContent = '';
    detailTime.textContent = '';
    detail.hidden = true;
  }
}

export function clearReactStatus() {
  if (state.reactPhaseTimer) {
    clearTimeout(state.reactPhaseTimer);
    state.reactPhaseTimer = 0;
  }
  state.reactPhaseQueue = [];
  state.reactPendingClear = false;
  state.reactStatusPhase = '';
  state.reactStatusCycle = null;
  state.reactStatusToolName = '';
  state.reactStatusElapsedMs = 0;
  state.reactPhaseShownAt = 0;
  if (state.reactStatusRow) {
    state.reactStatusRow.remove();
    state.reactStatusRow = null;
  }
}

function reactPhaseMinVisibleMs(phase) {
  switch (phase) {
    case 'act':
      return MIN_REACT_ACT_VISIBLE_MS;
    case 'observe':
      return MIN_REACT_OBSERVE_VISIBLE_MS;
    case 'analyze':
    default:
      return MIN_REACT_ANALYZE_VISIBLE_MS;
  }
}

export function requestClearReactStatus() {
  if (!state.reactStatusPhase && state.reactPhaseQueue.length === 0) {
    clearReactStatus();
    return;
  }
  state.reactPendingClear = true;
  scheduleNextReactPhase();
}

function ensureReactStatusRow() {
  if (!state.reactStatusRow) {
    state.reactStatusRow = document.createElement('div');
    state.reactStatusRow.className = 'msg-row system react-status-row';
    state.reactStatusRow.innerHTML = `
      <div class="system-card system-inline react-status-card">
        <span class="react-status-tag">ReAct</span>
        <span class="react-status-phase"></span>
        <span class="react-status-cycle"></span>
        <span class="react-status-detail" hidden>
          <span class="react-status-tool"></span>
          <span class="react-status-separator">·</span>
          <span class="react-status-time"></span>
        </span>
        <span class="react-status-dots" aria-hidden="true">
          <span></span>
          <span></span>
          <span></span>
        </span>
      </div>
    `;
    dom.chat.appendChild(state.reactStatusRow);
    hideWelcome();
  }
}

function scheduleNextReactPhase() {
  if (state.reactPhaseTimer || !state.reactStatusPhase) {
    return;
  }

  const elapsed = performance.now() - state.reactPhaseShownAt;
  const delay = Math.max(0, reactPhaseMinVisibleMs(state.reactStatusPhase) - elapsed);
  state.reactPhaseTimer = setTimeout(() => {
    state.reactPhaseTimer = 0;
    const next = state.reactPhaseQueue.shift();
    if (next) {
      applyReactStatusNow(next.phase, next.cycle);
      return;
    }
    if (state.reactPendingClear) {
      clearReactStatus();
    }
  }, delay);
}

function applyReactStatusNow(phase, cycle = null) {
  ensureReactStatusRow();
  state.reactStatusPhase = phase;
  state.reactStatusCycle = Number.isInteger(cycle) ? cycle : null;
  if (phase !== 'act') {
    state.reactStatusToolName = '';
    state.reactStatusElapsedMs = 0;
  }
  state.reactPhaseShownAt = performance.now();
  renderReactStatus();
  scrollDown();
  scheduleNextReactPhase();
}

export function setReactActTool(name, elapsedMs = 0) {
  if (!name) return;
  state.reactStatusToolName = name;
  state.reactStatusElapsedMs = elapsedMs;
  if (state.reactStatusPhase === 'act') {
    renderReactStatus();
  }
}

export function showReactStatus(phase, cycle = null) {
  if (!phase) {
    requestClearReactStatus();
    return;
  }

  if (phase === 'finish') {
    requestClearReactStatus();
    return;
  }

  state.reactPendingClear = false;

  if (!state.reactStatusPhase && state.reactPhaseQueue.length === 0 && !state.reactPhaseTimer) {
    applyReactStatusNow(phase, cycle);
    return;
  }

  if (state.reactStatusPhase === phase && state.reactPhaseQueue.length === 0) {
    state.reactStatusCycle = Number.isInteger(cycle) ? cycle : null;
    renderReactStatus();
    return;
  }

  for (let index = state.reactPhaseQueue.length - 1; index >= 0; index -= 1) {
    if (state.reactPhaseQueue[index].phase === phase) {
      state.reactPhaseQueue[index].cycle = Number.isInteger(cycle) ? cycle : null;
      state.reactPhaseQueue.splice(index + 1);
      scheduleNextReactPhase();
      return;
    }
  }

  state.reactPhaseQueue.push({
    phase,
    cycle: Number.isInteger(cycle) ? cycle : null,
  });
  if (state.reactPhaseQueue.length > MAX_REACT_QUEUED_PHASES) {
    state.reactPhaseQueue.splice(0, state.reactPhaseQueue.length - MAX_REACT_QUEUED_PHASES);
  }
  scheduleNextReactPhase();
}
