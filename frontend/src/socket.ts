import { state, dom } from './state.js';
import { MAX_RECONNECT_ATTEMPTS } from './constants.js';
import { addSystem, setBusy } from './renderers/chat.js';
import { clearActiveAutoTrace, clearCompressionOutcome } from './renderers/auto-trace.js';
import { clearReactStatus } from './renderers/react-status.js';
import { renderSessionDrawer } from './renderers/sessions.js';
import { closeToolDrawer } from './renderers/tools.js';
import { resetTodosUiState } from './renderers/todos.js';
import { finishAssistantStream, finishReasoningStream } from './handlers/stream.js';
import { tr } from './i18n.js';
import { syncRestoredSessionCapabilities, updateAttachButton } from './images.js';
import {
  beginComposerRevisionHandshake,
  completeComposerSessionTransition,
  restoreComposerSessionTransition,
  syncComposerAvailability,
} from './composerAvailability.js';

type TranslationVars = Record<string, string | number | boolean | null | undefined>;
type ConnectionStatus = 'connecting' | 'connected' | 'disconnected';

let currentConnStatus: {
  status: ConnectionStatus;
  key: string;
  vars?: TranslationVars;
} = {
  status: 'disconnected',
  key: 'common.offline',
};

// Connection indicator has three visual states: connecting (amber, pulsing),
// connected (green), disconnected/failed (red). We used to flip straight from
// connected → disconnected on socket close which hid the in-flight retry from
// the user; the intermediate state makes the retry loop legible.
function setConnStatus(status: ConnectionStatus, key: string, vars?: TranslationVars): void {
  currentConnStatus = { status, key, vars };
  refreshConnectionStatus();
}

export function refreshConnectionStatus(): void {
  const { status, key, vars } = currentConnStatus;
  if (dom.connDot) dom.connDot.className = `conn-dot ${status}`;
  if (dom.connLabel) dom.connLabel.textContent = tr(key, vars);
}

function sessionWebSocketUrl(): string {
  const proto = location.protocol === 'https:' ? 'wss' : 'ws';
  const url = new URL(`${proto}://${location.host}/ws`);
  if (state.activeGroupId) {
    url.searchParams.set('group', state.activeGroupId);
    url.searchParams.set('session', 'main');
  } else if (state.activeSessionId) {
    url.searchParams.set('session', state.activeSessionId);
  }
  return url.toString();
}

let reconnectTimer: ReturnType<typeof setTimeout> | null = null;

async function recoverDisabledGroupAfterClose(socket: WebSocket, onMessage): Promise<boolean> {
  const closedGroupId = state.activeGroupId;
  if (!closedGroupId) return false;

  try {
    const response = await fetch('/api/client-config', { cache: 'no-store' });
    if (!response.ok) return false;
    const payload = await response.json();
    if (payload?.features?.groups === true) return false;

    // The Group socket may have been replaced while feature discovery was in
    // flight. In that case its successor owns recovery and this stale close
    // callback must not schedule another connection.
    if (state.ws !== socket || state.activeGroupId !== closedGroupId) return true;

    await Promise.resolve(
      onMessage({
        type: 'feature_status',
        features: { groups: false },
      }),
    );
    return state.ws !== socket || !state.activeGroupId || !state.groupsEnabled;
  } catch {
    // A failed feature probe is indistinguishable from an ordinary daemon or
    // network outage, so preserve the existing reconnect behavior.
    return false;
  }
}

function scheduleReconnect(socket: WebSocket, onMessage): void {
  if (state.ws !== socket) return;
  if (state.reconnectAttempts < MAX_RECONNECT_ATTEMPTS) {
    const delaySecs = Math.ceil(state.reconnectDelay / 1000);
    setConnStatus('connecting', 'socket.reconnecting', {
      seconds: delaySecs,
      attempt: state.reconnectAttempts + 1,
    });
    if (state.reconnectAttempts === 0) {
      addSystem(tr('socket.disconnectedReconnecting'));
    }
    reconnectTimer = setTimeout(() => {
      reconnectTimer = null;
      connect(onMessage);
    }, state.reconnectDelay);
    state.reconnectDelay = Math.min(state.reconnectDelay * 2, 30000);
    state.reconnectAttempts++;
  } else {
    completeComposerSessionTransition();
    state.sessionSwitchInFlight = false;
    state.composerSessionIdentityPending = false;
    syncComposerAvailability();
    updateAttachButton();
    renderSessionDrawer();
    setConnStatus('disconnected', 'common.offline');
    addSystem(tr('socket.lostRefresh'), 'error');
  }
}

function resetSessionScopedUiState(): void {
  finishAssistantStream({ discardIfEmpty: true });
  finishReasoningStream();
  closeToolDrawer();
  clearReactStatus();
  clearCompressionOutcome();
  clearActiveAutoTrace();
  resetTodosUiState();
  state.reasoningPanel = null;
  setBusy(false);
}

export function cancelReconnect(): void {
  if (reconnectTimer !== null) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
}

export function connect(onMessage) {
  setConnStatus('connecting', 'common.connecting');
  const socket = new WebSocket(sessionWebSocketUrl());
  state.ws = socket;

  socket.onopen = () => {
    if (state.ws !== socket) return;
    beginComposerRevisionHandshake();
    renderSessionDrawer();
    state.reconnectDelay = 1000;
    state.reconnectAttempts = 0;
    setConnStatus('connected', 'common.online');
    addSystem(tr('common.connected'));
  };

  socket.onclose = () => {
    if (state.ws !== socket) return;
    syncRestoredSessionCapabilities(restoreComposerSessionTransition());
    resetSessionScopedUiState();
    if (state.activeGroupId) {
      void recoverDisabledGroupAfterClose(socket, onMessage).then((recovered) => {
        if (!recovered) scheduleReconnect(socket, onMessage);
      });
      return;
    }
    scheduleReconnect(socket, onMessage);
  };

  socket.onerror = () => {
    if (state.ws === socket) socket.close();
  };

  socket.onmessage = (e) => {
    if (state.ws !== socket) return;
    let data;
    try {
      data = JSON.parse(e.data);
    } catch {
      console.warn('Invalid JSON from server:', e.data);
      return;
    }
    onMessage(data);
  };
}

export function reconnectToActiveSession(onMessage): void {
  cancelReconnect();
  state.reconnectAttempts = 0;
  state.reconnectDelay = 1000;
  if (state.ws) {
    const ws = state.ws;
    state.ws = null;
    ws.onclose = null;
    ws.onerror = null;
    ws.onmessage = null;
    ws.close();
  }
  resetSessionScopedUiState();
  connect(onMessage);
}
