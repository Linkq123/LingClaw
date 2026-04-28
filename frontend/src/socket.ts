import { state, dom } from './state.js';
import { MAX_RECONNECT_ATTEMPTS } from './constants.js';
import { addSystem, setBusy } from './renderers/chat.js';
import { clearReactStatus } from './renderers/react-status.js';
import { closeToolDrawer } from './renderers/tools.js';
import { finishAssistantStream, finishReasoningStream } from './handlers/stream.js';

// Connection indicator has three visual states: connecting (amber, pulsing),
// connected (green), disconnected/failed (red). We used to flip straight from
// connected → disconnected on socket close which hid the in-flight retry from
// the user; the intermediate state makes the retry loop legible.
function setConnStatus(status: 'connecting' | 'connected' | 'disconnected', label: string): void {
  if (dom.connDot) dom.connDot.className = `conn-dot ${status}`;
  if (dom.connLabel) dom.connLabel.textContent = label;
}

let reconnectTimer: ReturnType<typeof setTimeout> | null = null;

export function cancelReconnect(): void {
  if (reconnectTimer !== null) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
}

export function connect(onMessage) {
  const proto = location.protocol === 'https:' ? 'wss' : 'ws';
  setConnStatus('connecting', 'Connecting…');
  state.ws = new WebSocket(`${proto}://${location.host}/ws`);

  state.ws.onopen = () => {
    state.reconnectDelay = 1000;
    state.reconnectAttempts = 0;
    setConnStatus('connected', 'Online');
    addSystem('Connected.');
  };

  state.ws.onclose = () => {
    finishAssistantStream({ discardIfEmpty: true });
    finishReasoningStream();
    closeToolDrawer();
    clearReactStatus();
    state.reasoningPanel = null;
    setBusy(false);
    if (state.reconnectAttempts < MAX_RECONNECT_ATTEMPTS) {
      const delaySecs = Math.ceil(state.reconnectDelay / 1000);
      setConnStatus(
        'connecting',
        `Reconnecting in ${delaySecs}s (#${state.reconnectAttempts + 1})`,
      );
      if (state.reconnectAttempts === 0) {
        addSystem('Disconnected. Reconnecting...');
      }
      reconnectTimer = setTimeout(() => {
        reconnectTimer = null;
        connect(onMessage);
      }, state.reconnectDelay);
      state.reconnectDelay = Math.min(state.reconnectDelay * 2, 30000);
      state.reconnectAttempts++;
    } else {
      setConnStatus('disconnected', 'Offline');
      addSystem('Connection lost. Please refresh the page.', 'error');
    }
  };

  state.ws.onerror = () => state.ws.close();

  state.ws.onmessage = (e) => {
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
