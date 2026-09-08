import { vi } from 'vitest';
import {
  beginComposerTransport,
  bindComposerTransportSocket,
  confirmComposerTransportIdentity,
  confirmComposerTransportHistory,
  openComposerTransport,
} from '../src/composerTransport.js';
import { state } from '../src/state.js';

/** Component fixtures explicitly represent an opened, negotiated, identified connection. */
export function prepareComposerTransportFixture(): void {
  const socket =
    state.ws || ({ readyState: WebSocket.OPEN, send: vi.fn() } as unknown as WebSocket);
  state.ws = socket;
  state.executionIdentityProtocol = 'strict';
  const generation = ++state.socketGeneration;
  beginComposerTransport(generation);
  bindComposerTransportSocket(generation, socket, generation, 'strict');
  openComposerTransport(generation, socket, generation);
  confirmComposerTransportIdentity();
  confirmComposerTransportHistory();
}
