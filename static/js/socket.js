import { state, dom } from './state.js';
import { MAX_RECONNECT_ATTEMPTS } from './constants.js';
import { addSystem, setBusy } from './renderers/chat.js';
import { clearReactStatus } from './renderers/react-status.js';
import { closeToolDrawer } from './renderers/tools.js';
import { finishAssistantStream, finishReasoningStream } from './handlers/stream.js';

export function connect(onMessage) {
  const proto = location.protocol === 'https:' ? 'wss' : 'ws';
  state.ws = new WebSocket(`${proto}://${location.host}/ws`);

  state.ws.onopen = () => {
    state.reconnectDelay = 1000;
    state.reconnectAttempts = 0;
    dom.connDot.className = 'conn-dot connected';
    dom.connLabel.textContent = 'Online';
    addSystem('Connected.');
  };

  state.ws.onclose = () => {
    dom.connDot.className = 'conn-dot disconnected';
    dom.connLabel.textContent = 'Offline';
    finishAssistantStream({ discardIfEmpty: true });
    finishReasoningStream();
    closeToolDrawer();
    clearReactStatus();
    state.reasoningPanel = null;
    setBusy(false);
    if (state.reconnectAttempts < MAX_RECONNECT_ATTEMPTS) {
      addSystem('Disconnected. Reconnecting...');
      setTimeout(() => connect(onMessage), state.reconnectDelay);
      state.reconnectDelay = Math.min(state.reconnectDelay * 2, 30000);
      state.reconnectAttempts++;
    } else {
      addSystem('Connection lost. Please refresh the page.', 'error');
    }
  };

  state.ws.onerror = () => state.ws.close();

  state.ws.onmessage = (e) => {
    let data;
    try { data = JSON.parse(e.data); } catch { console.warn('Invalid JSON from server:', e.data); return; }
    onMessage(data);
  };
}
