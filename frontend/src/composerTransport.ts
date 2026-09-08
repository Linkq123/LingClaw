import { state } from './state.js';

export type ComposerTransportAvailability = 'connecting' | 'preparing' | 'ready' | 'offline';
type Protocol = 'legacy' | 'strict';

interface TransportBinding {
  intent: number;
  phase: ComposerTransportAvailability;
  socket: WebSocket | null;
  generation: number;
  protocol: Protocol | null;
  opened: boolean;
  identityConfirmed: boolean;
  historyConfirmed: boolean;
  target: string;
}

let binding: TransportBinding | null = null;
let protocolFailure = false;
let lastPreparedTarget = '';

function activeTarget(): string {
  return state.activeGroupId
    ? `group:${state.activeGroupId}`
    : `session:${state.activeSessionId || 'main'}`;
}

export function beginComposerTransport(intent: number): void {
  binding = {
    intent,
    phase: 'connecting',
    socket: null,
    generation: 0,
    protocol: null,
    opened: false,
    identityConfirmed: false,
    historyConfirmed: false,
    target: activeTarget(),
  };
  protocolFailure = false;
}

export function bindComposerTransportSocket(
  intent: number,
  socket: WebSocket,
  generation: number,
  protocol: Protocol,
): void {
  if (binding?.intent !== intent) return;
  Object.assign(binding, { socket, generation, protocol, target: activeTarget() });
}

export function isComposerTransportSocketCurrent(
  intent: number,
  socket: WebSocket,
  generation: number,
): boolean {
  return Boolean(
    binding?.intent === intent &&
    binding.socket === socket &&
    binding.generation === generation &&
    state.ws === socket &&
    state.socketGeneration === generation,
  );
}

export function openComposerTransport(intent: number, socket: WebSocket, generation: number): void {
  if (!isComposerTransportSocketCurrent(intent, socket, generation) || !binding) return;
  binding.opened = true;
  binding.phase = 'preparing';
}

/** Called only after the full socket Session/Group payload passes its existing identity validator. */
export function confirmComposerTransportIdentity(): void {
  if (
    !binding?.opened ||
    !binding.socket ||
    state.ws !== binding.socket ||
    state.socketGeneration !== binding.generation
  )
    return;
  if (binding.identityConfirmed && binding.target !== activeTarget())
    binding.historyConfirmed = false;
  binding.identityConfirmed = true;
  binding.target = activeTarget();
  binding.phase = 'ready';
  if (binding.historyConfirmed) lastPreparedTarget = binding.target;
}

/** A same-target reconnect replays history without discarding the unsent attachment draft. */
export function preserveComposerAttachmentsOnHistory(): boolean {
  return Boolean(
    binding?.opened &&
    binding.socket === state.ws &&
    binding.generation === state.socketGeneration &&
    !binding.historyConfirmed &&
    binding.target === activeTarget() &&
    binding.target === lastPreparedTarget,
  );
}

export function confirmComposerTransportHistory(): void {
  if (
    !binding?.opened ||
    !binding.socket ||
    binding.socket !== state.ws ||
    binding.generation !== state.socketGeneration
  )
    return;
  binding.historyConfirmed = true;
  if (binding.identityConfirmed) lastPreparedTarget = binding.target;
}

export function clearComposerTransport(failureKey = ''): void {
  binding = null;
  protocolFailure =
    failureKey === 'socket.legacyExecutionReconnectUnsupported' ||
    failureKey === 'socket.executionIdentityMissing';
}

export function composerTransportAvailability(): ComposerTransportAvailability {
  if (!binding) return 'offline';
  if (binding.phase === 'connecting') return 'connecting';
  if (
    !binding.socket ||
    state.ws !== binding.socket ||
    state.socketGeneration !== binding.generation ||
    state.executionIdentityProtocol !== binding.protocol ||
    binding.socket.readyState !== WebSocket.OPEN
  )
    return 'offline';
  if (
    !binding.opened ||
    !binding.identityConfirmed ||
    !binding.historyConfirmed ||
    binding.target !== activeTarget() ||
    state.composerSessionIdentityPending ||
    state.composerSessionTransitionPending ||
    state.sessionSwitchInFlight ||
    state.sessionIdentityMutationInFlight
  )
    return 'preparing';
  return 'ready';
}

export function composerTransportReasonKey(): string {
  const availability = composerTransportAvailability();
  return availability === 'connecting'
    ? 'composer.connectionConnecting'
    : availability === 'preparing'
      ? 'composer.connectionPreparing'
      : protocolFailure
        ? 'composer.connectionProtocolUnavailable'
        : 'composer.connectionUnavailable';
}

export function getComposerTransportSocket(): WebSocket | null {
  return composerTransportAvailability() === 'ready' ? binding?.socket || null : null;
}

/** No queue or automatic resend: callers may clear a draft only after this exact socket accepts it. */
export function sendComposerTransportMessage(payload: string): boolean {
  const socket = getComposerTransportSocket();
  if (!socket) return false;
  try {
    socket.send(payload);
    return true;
  } catch {
    if (state.ws === socket) clearComposerTransport();
    return false;
  }
}
