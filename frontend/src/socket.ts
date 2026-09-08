import { state, dom } from './state.js';
import { MAX_RECONNECT_ATTEMPTS } from './constants.js';
import { addSystem, setBusy } from './renderers/chat.js';
import { clearActiveAutoTrace, clearCompressionOutcome } from './renderers/auto-trace.js';
import { clearReactStatus } from './renderers/react-status.js';
import { completeExecutionStackForClientRun } from './renderers/execution-stack.js';
import { restorePendingPlanAction } from './renderers/pending-plan.js';
import { renderSessionDrawer } from './renderers/sessions.js';
import { finishTaskPlanPanel } from './renderers/task-plan.js';
import { closeToolDrawer } from './renderers/tools.js';
import { resetTodosUiState } from './renderers/todos.js';
import { finishAssistantStream, finishReasoningStream } from './handlers/stream.js';
import { tr } from './i18n.js';
import {
  beginComposerTransport,
  bindComposerTransportSocket,
  clearComposerTransport,
  isComposerTransportSocketCurrent,
  openComposerTransport,
} from './composerTransport.js';
import { syncRestoredSessionCapabilities, updateAttachButton } from './images.js';
import {
  beginComposerRevisionHandshake,
  completeComposerSessionTransition,
  invalidateComposerSessionModelRecovery,
  restoreComposerSessionTransition,
  syncComposerAvailability,
} from './composerAvailability.js';

type TranslationVars = Record<string, string | number | boolean | null | undefined>;
type ConnectionStatus = 'connecting' | 'connected' | 'disconnected';
export type ExecutionIdentityProtocol = 'legacy' | 'strict';

export function executionIdentityProtocolFromClientConfig(
  payload: unknown,
): ExecutionIdentityProtocol | null {
  const version = (payload as { protocols?: { execution_identity?: unknown } } | null)?.protocols
    ?.execution_identity;
  if (version == null) return 'legacy';
  return version === 1 ? 'strict' : null;
}

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

interface SocketTarget {
  key: string;
  url: string;
}

interface NegotiatedClientCapabilities {
  executionIdentityProtocol: ExecutionIdentityProtocol;
  groupsEnabled: boolean;
}

interface ConnectOptions {
  initialFeatureDiscovery?: boolean;
}

export const CLIENT_CONFIG_TIMEOUT_MS = 5_000;

interface ClientConfigRequestOptions {
  signal?: AbortSignal;
  timeoutMs?: number;
}

export interface ClientConfigPayload {
  features?: { groups?: unknown };
  protocols?: { execution_identity?: unknown };
}

function clientConfigAbortError(timedOut: boolean): Error {
  const error = new Error(
    timedOut ? 'Client configuration request timed out' : 'Client configuration request aborted',
  );
  error.name = timedOut ? 'TimeoutError' : 'AbortError';
  return error;
}

/**
 * Fetches and fully decodes client-config within one deadline. The explicit
 * abort race also bounds mocked/non-conforming Response bodies whose json()
 * implementation does not observe the fetch signal.
 */
export async function fetchClientConfigBounded(
  options: ClientConfigRequestOptions = {},
): Promise<ClientConfigPayload> {
  const controller = new AbortController();
  const externalSignal = options.signal;
  let timedOut = false;
  const abortFromOwner = () => controller.abort();
  if (externalSignal?.aborted) controller.abort();
  else externalSignal?.addEventListener('abort', abortFromOwner, { once: true });

  const timeout = setTimeout(() => {
    timedOut = true;
    controller.abort();
  }, options.timeoutMs ?? CLIENT_CONFIG_TIMEOUT_MS);
  const aborted = new Promise<never>((_resolve, reject) => {
    const rejectAbort = () => reject(clientConfigAbortError(timedOut));
    if (controller.signal.aborted) rejectAbort();
    else controller.signal.addEventListener('abort', rejectAbort, { once: true });
  });
  const request = (async () => {
    const response = await fetch('/api/client-config', {
      cache: 'no-store',
      signal: controller.signal,
    });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    return (await response.json()) as ClientConfigPayload;
  })();

  try {
    return await Promise.race([request, aborted]);
  } finally {
    clearTimeout(timeout);
    externalSignal?.removeEventListener('abort', abortFromOwner);
  }
}

function sessionWebSocketTarget(): SocketTarget {
  const proto = location.protocol === 'https:' ? 'wss' : 'ws';
  const url = new URL(`${proto}://${location.host}/ws`);
  if (state.activeGroupId) {
    url.searchParams.set('group', state.activeGroupId);
    url.searchParams.set('session', 'main');
    return { key: `group:${state.activeGroupId}`, url: url.toString() };
  } else if (state.activeSessionId) {
    url.searchParams.set('session', state.activeSessionId);
  }
  return { key: `session:${state.activeSessionId || 'main'}`, url: url.toString() };
}

let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
let lastProtocolFailureKey = '';
let connectionIntentSequence = 0;
let connectionNegotiationController: AbortController | null = null;
let groupRecoveryController: AbortController | null = null;

function abortConnectionNegotiation(): void {
  connectionNegotiationController?.abort();
  connectionNegotiationController = null;
}

function abortGroupRecoveryProbe(): void {
  groupRecoveryController?.abort();
  groupRecoveryController = null;
}

function isCurrentConnectionIntent(intent: number, targetKey: string): boolean {
  return intent === connectionIntentSequence && sessionWebSocketTarget().key === targetKey;
}

async function negotiateClientCapabilities(
  signal: AbortSignal,
): Promise<NegotiatedClientCapabilities> {
  const payload = await fetchClientConfigBounded({ signal });
  const executionIdentityProtocol = executionIdentityProtocolFromClientConfig(payload);
  if (!executionIdentityProtocol) throw new Error('Unsupported execution identity protocol');
  return {
    executionIdentityProtocol,
    groupsEnabled: payload?.features?.groups === true,
  };
}

async function recoverDisabledGroupAfterClose(socket: WebSocket, onMessage): Promise<boolean> {
  const closedGroupId = state.activeGroupId;
  if (!closedGroupId) return false;

  abortGroupRecoveryProbe();
  const controller = new AbortController();
  groupRecoveryController = controller;

  try {
    const payload = await fetchClientConfigBounded({ signal: controller.signal });
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
    // A newer connection intent owns recovery after an explicit owner abort.
    // A request deadline, by contrast, falls back to the normal reconnect.
    if (controller.signal.aborted) return true;
    // A failed feature probe is indistinguishable from an ordinary daemon or
    // network outage, so preserve the existing reconnect behavior.
    return false;
  } finally {
    if (groupRecoveryController === controller) groupRecoveryController = null;
  }
}

function scheduleReconnect(socket: WebSocket, onMessage, intent: number, targetKey: string): void {
  if (state.ws !== socket || !isCurrentConnectionIntent(intent, targetKey)) return;
  if (state.reconnectAttempts < MAX_RECONNECT_ATTEMPTS) {
    beginComposerTransport(intent);
    syncComposerAvailability();
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
      void connect(onMessage);
    }, state.reconnectDelay);
    state.reconnectDelay = Math.min(state.reconnectDelay * 2, 30000);
    state.reconnectAttempts++;
  } else {
    clearComposerTransport();
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
  state.activeExecutionRunId = 0;
  state.activeExecutionServerRunId = '';
  state.activeExecutionPlanId = '';
  state.terminalExecutionStack = null;
  state.currentRoundStartedAt = 0;
  state.currentRoundFirstTokenAt = 0;
  setBusy(false);
}

function rejectExecutionProtocolConnection(key: string): void {
  const socket = state.ws;
  state.ws = null;
  closeSocketWithoutCallbacks(socket);
  clearComposerTransport(key);
  resetSessionScopedUiState();
  completeComposerSessionTransition();
  state.sessionSwitchInFlight = false;
  state.composerSessionIdentityPending = false;
  syncComposerAvailability();
  updateAttachButton();
  renderSessionDrawer();
  setConnStatus('disconnected', key);
  if (lastProtocolFailureKey !== key) {
    addSystem(tr(key), 'error');
    lastProtocolFailureKey = key;
  }
}

export function cancelReconnect(): void {
  connectionIntentSequence += 1;
  clearComposerTransport();
  abortConnectionNegotiation();
  abortGroupRecoveryProbe();
  if (reconnectTimer !== null) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
  syncComposerAvailability();
}

function closeSocketWithoutCallbacks(socket: WebSocket | null): void {
  if (!socket) return;
  socket.onopen = null;
  socket.onclose = null;
  socket.onerror = null;
  socket.onmessage = null;
  socket.close();
}

export function failCloseCurrentExecutionProtocol(key = 'socket.executionIdentityMissing'): void {
  const activeClientRunId = state.activeExecutionRunId;
  const activeStack = state.activeExecutionStack;
  if (
    activeClientRunId > 0 &&
    activeStack?.isConnected === true &&
    activeStack.dataset.executionStatus === 'running' &&
    activeStack.dataset.executionClientRunId === String(activeClientRunId) &&
    activeStack.dataset.executionServerRunId === state.activeExecutionServerRunId
  ) {
    completeExecutionStackForClientRun(activeClientRunId, {
      status: 'incomplete',
      summary: tr('execution.protocolFailureSummary'),
      summaryKey: 'execution.protocolFailureSummary',
      recoveryLabel: tr('execution.reviewIncomplete'),
      recoveryLabelKey: 'execution.reviewIncomplete',
      terminalSource: 'error',
      immediate: true,
    });
  }
  cancelReconnect();
  const socket = state.ws;
  state.ws = null;
  closeSocketWithoutCallbacks(socket);
  state.executionIdentityProtocol = 'unavailable';
  finishTaskPlanPanel();
  restorePendingPlanAction();
  rejectExecutionProtocolConnection(key);
}

export async function connect(onMessage, options: ConnectOptions = {}): Promise<void> {
  invalidateComposerSessionModelRecovery();
  abortConnectionNegotiation();
  abortGroupRecoveryProbe();
  const intent = ++connectionIntentSequence;
  if (reconnectTimer !== null) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
  beginComposerTransport(intent);
  syncComposerAvailability();
  let target = sessionWebSocketTarget();
  const negotiationController = new AbortController();
  connectionNegotiationController = negotiationController;
  setConnStatus('connecting', 'common.connecting');

  let capabilities: NegotiatedClientCapabilities;
  try {
    capabilities = await negotiateClientCapabilities(negotiationController.signal);
  } catch {
    if (!isCurrentConnectionIntent(intent, target.key)) return;
    if (options.initialFeatureDiscovery) {
      const featureUpdate = Promise.resolve(
        onMessage({
          type: 'feature_status',
          features: { groups: false },
          initial_client_config: true,
        }),
      );
      void featureUpdate.catch((error) => {
        console.warn('Could not apply initial client features:', error);
      });
      await Promise.resolve();
      if (intent !== connectionIntentSequence) return;
      target = sessionWebSocketTarget();
    }
    state.executionIdentityProtocol = 'unavailable';
    rejectExecutionProtocolConnection('socket.executionProtocolUnavailable');
    return;
  } finally {
    if (connectionNegotiationController === negotiationController) {
      connectionNegotiationController = null;
    }
  }

  if (!isCurrentConnectionIntent(intent, target.key)) return;
  if (options.initialFeatureDiscovery || state.groupsEnabled !== capabilities.groupsEnabled) {
    const featureUpdate = Promise.resolve(
      onMessage({
        type: 'feature_status',
        features: { groups: capabilities.groupsEnabled },
        ...(options.initialFeatureDiscovery ? { initial_client_config: true } : {}),
      }),
    );
    void featureUpdate.catch((error) => {
      console.warn('Could not apply negotiated client features:', error);
    });
    // The production feature handler applies its identity/target state before
    // any optional Group-list refresh. Yield once for synchronous callbacks,
    // but never let an unrelated discovery request hold the socket preflight.
    await Promise.resolve();
    if (intent !== connectionIntentSequence) return;
    if (options.initialFeatureDiscovery) target = sessionWebSocketTarget();
    else if (sessionWebSocketTarget().key !== target.key) return;
  }

  state.executionIdentityProtocol = capabilities.executionIdentityProtocol;
  if (capabilities.executionIdentityProtocol === 'legacy' && state.socketGeneration > 0) {
    rejectExecutionProtocolConnection('socket.legacyExecutionReconnectUnsupported');
    return;
  }

  // A direct connect call may supersede a still-open older generation. Once
  // this intent and target have won negotiation, retire that exact socket
  // before constructing its replacement so no orphan connection survives.
  const previousSocket = state.ws;
  if (previousSocket) {
    state.ws = null;
    closeSocketWithoutCallbacks(previousSocket);
  }

  let socket: WebSocket;
  try {
    socket = new WebSocket(target.url);
  } catch {
    if (isCurrentConnectionIntent(intent, target.key)) {
      state.executionIdentityProtocol = 'unavailable';
      rejectExecutionProtocolConnection('socket.executionProtocolUnavailable');
    }
    return;
  }
  if (!isCurrentConnectionIntent(intent, target.key)) {
    closeSocketWithoutCallbacks(socket);
    return;
  }

  state.socketGeneration += 1;
  const socketGeneration = state.socketGeneration;
  const socketProtocol = capabilities.executionIdentityProtocol;
  if (socketProtocol === 'legacy') state.legacyExecutionSocketGeneration = socketGeneration;
  state.ws = socket;
  bindComposerTransportSocket(intent, socket, socketGeneration, socketProtocol);

  socket.onopen = () => {
    if (!isComposerTransportSocketCurrent(intent, socket, socketGeneration)) return;
    openComposerTransport(intent, socket, socketGeneration);
    beginComposerRevisionHandshake();
    renderSessionDrawer();
    state.reconnectDelay = 1000;
    state.reconnectAttempts = 0;
    lastProtocolFailureKey = '';
    setConnStatus('connected', 'common.online');
    addSystem(tr('common.connected'));
  };

  socket.onclose = () => {
    if (!isComposerTransportSocketCurrent(intent, socket, socketGeneration)) return;
    clearComposerTransport();
    invalidateComposerSessionModelRecovery();
    syncRestoredSessionCapabilities(restoreComposerSessionTransition());
    resetSessionScopedUiState();
    syncComposerAvailability();
    if (socketProtocol === 'legacy') {
      rejectExecutionProtocolConnection('socket.legacyExecutionReconnectUnsupported');
      return;
    }
    const closedTargetKey = sessionWebSocketTarget().key;
    if (state.activeGroupId) {
      beginComposerTransport(connectionIntentSequence);
      syncComposerAvailability();
      setConnStatus('connecting', 'common.connecting');
      void recoverDisabledGroupAfterClose(socket, onMessage).then((recovered) => {
        if (!recovered) scheduleReconnect(socket, onMessage, intent, closedTargetKey);
      });
      return;
    }
    scheduleReconnect(socket, onMessage, intent, closedTargetKey);
  };

  socket.onerror = () => {
    if (isComposerTransportSocketCurrent(intent, socket, socketGeneration)) {
      // Keep the close handler as the single reconnect owner.
      socket.close();
      syncComposerAvailability();
    }
  };

  socket.onmessage = (e) => {
    if (!isComposerTransportSocketCurrent(intent, socket, socketGeneration)) return;
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

export function reconnectToActiveSession(onMessage): Promise<void> {
  cancelReconnect();
  state.reconnectAttempts = 0;
  state.reconnectDelay = 1000;
  if (state.ws) {
    const ws = state.ws;
    state.ws = null;
    closeSocketWithoutCallbacks(ws);
  }
  resetSessionScopedUiState();
  return connect(onMessage);
}
