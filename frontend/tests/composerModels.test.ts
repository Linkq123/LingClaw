import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  applyComposerModelPayload,
  closeComposerModelPicker,
  initComposerModelPicker,
  invalidateComposerModelCatalog,
  openComposerModelPicker,
  refreshActiveComposerSessionModelState,
  syncComposerModelControls,
} from '../src/composerModels.js';
import {
  acceptComposerSocketModelPayloadRevision,
  beginComposerRevisionHandshake,
  beginComposerSessionTransition,
  completeComposerSessionTransition,
  invalidateComposerSessionModelRecovery,
  setComposerSessionModelConfigured,
} from '../src/composerAvailability.js';
import { openAttachPopup } from '../src/images.js';
import { setLanguage } from '../src/i18n.js';
import { dom, initDomRefs, state } from '../src/state.js';
import { prepareComposerTransportFixture } from './composerTransportFixture.js';

function response(payload: unknown, status = 200): Response {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

function deferredResponse(): {
  promise: Promise<Response>;
  resolve: (value: Response) => void;
  reject: (reason?: unknown) => void;
} {
  let resolve!: (value: Response) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<Response>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function installOpenSocket(): WebSocket {
  const socket = { readyState: WebSocket.OPEN } as WebSocket;
  state.ws = socket;
  prepareComposerTransportFixture();
  return socket;
}

function recoveryRequestSignal(fetchMock: ReturnType<typeof vi.fn>, call = 0): AbortSignal {
  const init = fetchMock.mock.calls[call]?.[1] as RequestInit | undefined;
  const signal = init?.signal;
  if (!signal) throw new Error('Session model recovery request must carry an AbortSignal');
  return signal;
}

const catalogPayload = {
  session: {
    id: 'main',
    model: 'gateway/current',
    effort: 'medium',
    modelOverridePresent: true,
    modelOverrideConfigured: true,
    effectiveModelConfigured: true,
  },
  explicitPrimaryModelConfigured: true,
  capabilities: { image: false },
  configRevision: 7,
  models: [
    {
      ref: 'gateway/current',
      provider: 'gateway',
      id: 'current',
      name: 'Current Reasoner',
      input: ['text'],
      reasoning: true,
      efforts: ['low', 'medium', 'high'],
      defaultEffort: 'medium',
    },
    {
      ref: 'gateway/vision',
      provider: 'gateway',
      id: 'vision',
      name: 'Vision Reasoner',
      input: ['text', 'image'],
      reasoning: true,
      efforts: ['low', 'high'],
      defaultEffort: 'high',
    },
    {
      ref: 'local/plain',
      provider: 'local',
      id: 'plain',
      name: 'Plain Text',
      input: ['text'],
      reasoning: false,
      efforts: ['off'],
      defaultEffort: 'off',
    },
  ],
};

describe('Composer model picker', () => {
  beforeEach(() => {
    setLanguage('en');
    document.body.innerHTML = `
      <div class="composer-model-wrapper">
        <button id="composer-model-btn" aria-expanded="false">
          <span id="composer-model-label"></span>
        </button>
        <div id="composer-model-popup" role="dialog" hidden></div>
      </div>
      <div class="attach-wrapper">
        <button id="attach-btn" aria-expanded="false"></button>
        <div id="attach-popup" style="display: none">
          <div id="attach-menu">
            <button id="attach-local-btn" class="attach-menu-item"></button>
            <small id="attach-local-reason" hidden></small>
          </div>
          <div id="attach-upload-status" style="display: none"></div>
        </div>
      </div>
      <div id="input-area">
        <textarea id="input"></textarea>
        <button id="send"></button>
      </div>
      <p id="composer-availability-status" hidden>
        <span id="composer-availability-message"></span>
        <button id="composer-availability-action" hidden></button>
        <button id="composer-availability-retry" hidden></button>
      </p>
      <span id="composer-availability-detail"></span>
    `;
    initDomRefs();
    invalidateComposerModelCatalog();
    state.activeSessionId = 'main';
    state.activeGroupId = '';
    state.storageMode = 'healthy';
    state.busy = false;
    state.imageUploadInFlight = false;
    state.sessionSwitchInFlight = false;
    state.sessionIdentityMutationInFlight = false;
    state.composerSessionTransitionPending = false;
    state.composerSessionIdentityPending = false;
    state.composerModelSwitchInFlight = false;
    state.composerCurrentModel = 'gateway/current';
    state.composerCurrentEffort = 'medium';
    state.composerConfigRevision = 7;
    state.composerSessionModelRevision = 7;
    state.composerSessionModelOverridePresent = true;
    state.composerEffectiveModelConfigured = true;
    state.composerExplicitPrimaryModelConfigured = true;
    state.composerModelAvailability = 'ready';
    state.imageCapable = false;
    installOpenSocket();
    vi.stubGlobal('fetch', vi.fn<typeof fetch>().mockResolvedValue(response(catalogPayload)));
    initComposerModelPicker();
  });

  afterEach(() => {
    invalidateComposerSessionModelRecovery();
    closeComposerModelPicker(false);
    invalidateComposerModelCatalog();
    vi.useRealTimers();
    vi.unstubAllGlobals();
    document.body.innerHTML = '';
    dom.composerModelBtn = null;
    dom.composerModelLabel = null;
    dom.composerModelPopup = null;
    state.ws = null;
  });

  it('groups searchable models by provider and shows configured capabilities', async () => {
    vi.stubGlobal('fetch', vi.fn<typeof fetch>().mockResolvedValue(response(catalogPayload)));

    await openComposerModelPicker();

    const providers = Array.from(
      document.querySelectorAll<HTMLElement>('.composer-model-provider-name'),
    ).map((node) => node.textContent);
    expect(providers).toEqual(['gateway', 'local']);
    expect(dom.composerModelLabel?.textContent).toBe('Current Reasoner · Medium');
    expect(dom.composerModelPopup?.textContent).toContain('Image');
    expect(dom.composerModelPopup?.textContent).toContain('Reasoning');

    const search = document.querySelector<HTMLInputElement>('.composer-model-search')!;
    search.value = 'plain';
    search.dispatchEvent(new Event('input', { bubbles: true }));
    expect(document.querySelectorAll('.composer-model-option')).toHaveLength(1);
    expect(dom.composerModelPopup?.textContent).toContain('Plain Text');
  });

  it('keeps the attachment and model popovers mutually exclusive', async () => {
    openAttachPopup();

    expect(dom.attachPopup?.style.display).toBe('block');
    expect(dom.attachBtn?.getAttribute('aria-expanded')).toBe('true');

    await openComposerModelPicker();

    expect(dom.attachPopup?.style.display).toBe('none');
    expect(dom.attachBtn?.getAttribute('aria-expanded')).toBe('false');
    expect(dom.composerModelPopup?.hidden).toBe(false);
    expect(dom.composerModelBtn?.getAttribute('aria-expanded')).toBe('true');

    openAttachPopup();

    expect(dom.composerModelPopup?.hidden).toBe(true);
    expect(dom.composerModelBtn?.getAttribute('aria-expanded')).toBe('false');
    expect(dom.attachPopup?.style.display).toBe('block');
  });

  it('keeps the search input mounted while filtering so IME composition is not interrupted', async () => {
    await openComposerModelPicker();

    const search = document.querySelector<HTMLInputElement>('.composer-model-search')!;
    search.focus();
    search.value = 'vision';
    search.dispatchEvent(new InputEvent('input', { bubbles: true, isComposing: true }));

    expect(document.querySelector('.composer-model-search')).toBe(search);
    expect(document.activeElement).toBe(search);
    expect(document.querySelectorAll('.composer-model-option')).toHaveLength(1);
  });

  it('uses localized capability labels and sprite icons for model navigation', async () => {
    setLanguage('zh-CN');
    await openComposerModelPicker();

    expect(dom.composerModelPopup?.textContent).toContain('文本');
    expect(dom.composerModelPopup?.textContent).toContain('图片');
    expect(document.querySelector('.composer-model-option-tail use')?.getAttribute('href')).toBe(
      '#icon-chevron-right',
    );

    const model = Array.from(
      document.querySelectorAll<HTMLButtonElement>('.composer-model-option'),
    ).find((button) => button.textContent?.includes('Vision Reasoner'))!;
    model.click();
    expect(document.querySelector('.composer-model-back use')?.getAttribute('href')).toBe(
      '#icon-chevron-left',
    );
  });

  it('submits a model and effort as one atomic request and applies the response', async () => {
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(response(catalogPayload))
      .mockResolvedValueOnce(
        response({
          ok: true,
          session: { id: 'main', model: 'gateway/vision', effort: 'high' },
          capabilities: { image: true },
          configRevision: 8,
        }),
      );
    vi.stubGlobal('fetch', fetchMock);
    await openComposerModelPicker();

    const model = Array.from(
      document.querySelectorAll<HTMLButtonElement>('.composer-model-option'),
    ).find((button) => button.textContent?.includes('Vision Reasoner'))!;
    model.click();
    expect(dom.composerModelPopup?.textContent).toContain('Choose thinking effort');

    const high = Array.from(
      document.querySelectorAll<HTMLButtonElement>('.composer-effort-option'),
    ).find((button) => button.textContent?.includes('High'))!;
    high.click();
    await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));

    const [, request] = fetchMock.mock.calls[1];
    expect(request).toMatchObject({ method: 'PUT' });
    expect(JSON.parse(String((request as RequestInit).body))).toEqual({
      model: 'gateway/vision',
      effort: 'high',
    });
    await vi.waitFor(() => expect(state.composerModelSwitchInFlight).toBe(false));
    expect(state.composerCurrentModel).toBe('gateway/vision');
    expect(state.composerCurrentEffort).toBe('high');
    expect(state.composerSessionModelRevision).toBe(8);
    expect(state.composerModelAvailability).toBe('ready');
    expect(state.imageCapable).toBe(true);
    expect(dom.composerModelLabel?.textContent).toBe('Vision Reasoner · High');
    expect(dom.composerModelPopup?.hidden).toBe(true);
  });

  it('applies a non-reasoning model directly with off effort', async () => {
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(response(catalogPayload))
      .mockResolvedValueOnce(
        response({
          ok: true,
          session: { id: 'main', model: 'local/plain', effort: 'off' },
          capabilities: { image: false },
          configRevision: 8,
        }),
      );
    vi.stubGlobal('fetch', fetchMock);
    await openComposerModelPicker();

    const model = Array.from(
      document.querySelectorAll<HTMLButtonElement>('.composer-model-option'),
    ).find((button) => button.textContent?.includes('Plain Text'))!;
    model.click();

    await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
    expect(JSON.parse(String((fetchMock.mock.calls[1][1] as RequestInit).body))).toEqual({
      model: 'local/plain',
      effort: 'off',
    });
    await vi.waitFor(() => expect(state.composerModelSwitchInFlight).toBe(false));
    expect(state.imageCapable).toBe(false);
    expect(dom.composerModelLabel?.textContent).toBe('Plain Text');
  });

  it('announces a failed atomic update and moves focus to the retry action', async () => {
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(response(catalogPayload))
      .mockResolvedValueOnce(
        response({ code: 'effort_not_supported', error: 'stale option' }, 400),
      );
    vi.stubGlobal('fetch', fetchMock);
    await openComposerModelPicker();

    const model = Array.from(
      document.querySelectorAll<HTMLButtonElement>('.composer-model-option'),
    ).find((button) => button.textContent?.includes('Plain Text'))!;
    model.click();

    await vi.waitFor(() => expect(state.composerModelSwitchInFlight).toBe(false));
    const alert = document.querySelector<HTMLElement>('[role="alert"]');
    const retry = document.querySelector<HTMLButtonElement>('.composer-model-retry');
    expect(alert?.textContent).toContain('Could not change the model');
    await vi.waitFor(() => expect(retry).toBe(document.activeElement));
    expect(dom.composerModelPopup?.hidden).toBe(false);
  });

  it('closes an in-flight picker when the active Session starts changing', async () => {
    let rejectPut: ((reason?: unknown) => void) | undefined;
    const pendingPut = new Promise<Response>((_resolve, reject) => {
      rejectPut = reject;
    });
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(response(catalogPayload))
      .mockReturnValueOnce(pendingPut);
    vi.stubGlobal('fetch', fetchMock);
    await openComposerModelPicker();

    const model = Array.from(
      document.querySelectorAll<HTMLButtonElement>('.composer-model-option'),
    ).find((button) => button.textContent?.includes('Plain Text'))!;
    model.click();
    await vi.waitFor(() => expect(state.composerModelSwitchInFlight).toBe(true));

    state.sessionSwitchInFlight = true;
    syncComposerModelControls();
    expect(dom.composerModelPopup?.hidden).toBe(true);

    rejectPut?.(new Error('network failed'));
    await vi.waitFor(() => expect(state.composerModelSwitchInFlight).toBe(false));
    expect(dom.composerModelPopup?.hidden).toBe(true);
  });

  it('hides the single-model control in Group chat and explains why switching is blocked', async () => {
    applyComposerModelPayload({ model: 'gateway/current', effort: 'high' });
    state.activeGroupId = 'group-a';
    syncComposerModelControls();
    expect(dom.composerModelBtn?.hidden).toBe(true);
    expect(dom.composerModelBtn?.closest<HTMLElement>('.composer-model-wrapper')?.hidden).toBe(
      true,
    );

    state.activeGroupId = '';
    state.busy = true;
    syncComposerModelControls();
    expect(dom.composerModelBtn?.hidden).toBe(false);
    expect(dom.composerModelBtn?.closest<HTMLElement>('.composer-model-wrapper')?.hidden).toBe(
      false,
    );
    expect(dom.composerModelBtn?.disabled).toBe(false);
    expect(dom.composerModelBtn?.getAttribute('aria-disabled')).toBe('true');
    expect(dom.composerModelBtn?.title).toContain('Agent run');

    vi.mocked(fetch).mockClear();
    await openComposerModelPicker();
    expect(dom.composerModelPopup?.hidden).toBe(false);
    expect(dom.composerModelPopup?.getAttribute('role')).toBe('dialog');
    expect(dom.composerModelPopup?.textContent).toContain('Agent run');
    expect(fetch).not.toHaveBeenCalled();
  });

  it('keeps the Effort submenu open when its model row is replaced during click bubbling', async () => {
    vi.stubGlobal('fetch', vi.fn<typeof fetch>().mockResolvedValue(response(catalogPayload)));
    await openComposerModelPicker();

    const model = Array.from(
      document.querySelectorAll<HTMLButtonElement>('.composer-model-option'),
    ).find((button) => button.textContent?.includes('Vision Reasoner'))!;
    model.click();

    expect(dom.composerModelPopup?.hidden).toBe(false);
    expect(document.querySelectorAll('.composer-effort-option')).toHaveLength(2);
  });

  it('preloads display metadata when the active Session model payload arrives', async () => {
    const startupPayload = {
      ...catalogPayload,
      session: { ...catalogPayload.session, effort: 'high' },
      models: catalogPayload.models.map((model) =>
        model.ref === 'gateway/current' ? { ...model, name: 'Startup Reasoner' } : model,
      ),
    };
    const fetchMock = vi.fn<typeof fetch>().mockResolvedValue(response(startupPayload));
    vi.stubGlobal('fetch', fetchMock);

    applyComposerModelPayload({
      model: 'gateway/current',
      effort: 'high',
      configRevision: 7,
    });

    await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    await vi.waitFor(() =>
      expect(dom.composerModelLabel?.textContent).toBe('Startup Reasoner · High'),
    );
    expect(dom.composerModelPopup?.hidden).toBe(true);
  });

  it('does not let an older model response overwrite a newer socket revision', () => {
    state.composerCurrentModel = 'gateway/vision';
    state.composerCurrentEffort = 'high';
    state.composerConfigRevision = 9;

    applyComposerModelPayload({
      model: 'local/plain',
      effort: 'off',
      configRevision: 8,
    });

    expect(state.composerCurrentModel).toBe('gateway/vision');
    expect(state.composerCurrentEffort).toBe('high');
  });

  it('retries a stale catalog response before rendering it', async () => {
    const stalePayload = {
      ...catalogPayload,
      configRevision: 6,
      models: catalogPayload.models.map((model) => ({ ...model, name: `Stale ${model.name}` })),
    };
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(response(stalePayload))
      .mockResolvedValueOnce(response(catalogPayload));
    vi.stubGlobal('fetch', fetchMock);

    await openComposerModelPicker();

    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(dom.composerModelPopup?.textContent).toContain('Current Reasoner');
    expect(dom.composerModelPopup?.textContent).not.toContain('Stale Current Reasoner');
  });

  it('adopts a newer HTTP catalog revision so reopening uses the cache', async () => {
    state.composerConfigRevision = 7;
    let catalogRequests = 0;
    const fetchMock = vi.fn<typeof fetch>().mockImplementation((input) => {
      const url = String(input);
      if (url.startsWith('/api/session-models')) {
        catalogRequests += 1;
        return Promise.resolve(response({ ...catalogPayload, configRevision: 8 }));
      }
      if (url === '/api/config') {
        return Promise.resolve(
          response({
            config: {},
            etag: 'revision-8',
            configRevision: 8,
            explicitPrimaryModelConfigured: true,
            configuredModelsAvailable: true,
          }),
        );
      }
      return Promise.reject(new Error(`Unexpected request: ${url}`));
    });
    vi.stubGlobal('fetch', fetchMock);

    await openComposerModelPicker();
    closeComposerModelPicker(false);
    await openComposerModelPicker();

    expect(state.composerConfigRevision).toBe(8);
    expect(state.composerSessionModelRevision).toBe(8);
    expect(state.composerModelAvailability).toBe('ready');
    expect(catalogRequests).toBe(1);
  });

  it('bounds permanently hanging recovery GETs and recovers through the existing Retry action', async () => {
    vi.useFakeTimers();
    const signals: AbortSignal[] = [];
    const unhandledRejections: unknown[] = [];
    const onUnhandledRejection = (event: PromiseRejectionEvent): void => {
      unhandledRejections.push(event.reason);
      event.preventDefault();
    };
    window.addEventListener('unhandledrejection', onUnhandledRejection);
    try {
      const hangingFetch = vi.fn<typeof fetch>((_input, init) => {
        if (init?.signal) signals.push(init.signal);
        if (signals.length === 1) return new Promise<Response>(() => {});
        return Promise.resolve({
          ok: true,
          status: 200,
          json: () => new Promise<unknown>(() => {}),
        } as Response);
      });
      vi.stubGlobal('fetch', hangingFetch);
      state.composerSessionModelRevision = 6;
      state.composerConfigRevision = 7;
      state.composerEffectiveModelConfigured = null;

      const recovery = refreshActiveComposerSessionModelState('main');
      expect(hangingFetch).toHaveBeenCalledTimes(1);
      await vi.advanceTimersByTimeAsync(10_000);

      await expect(recovery).resolves.toBe('failed');
      expect(hangingFetch).toHaveBeenCalledTimes(2);
      expect(signals).toHaveLength(2);
      expect(signals.every((signal) => signal.aborted)).toBe(true);
      expect(state.composerModelAvailability).toBe('config-unavailable');
      expect(dom.composerAvailabilityRetry?.hidden).toBe(false);
      expect(vi.getTimerCount()).toBe(0);

      const retryFetch = vi.fn<typeof fetch>().mockResolvedValue(response(catalogPayload));
      vi.stubGlobal('fetch', retryFetch);
      let retry: ReturnType<typeof refreshActiveComposerSessionModelState> | null = null;
      const retryHandler = (): void => {
        retry = refreshActiveComposerSessionModelState('main');
      };
      dom.composerAvailabilityRetry?.addEventListener('click', retryHandler, { once: true });
      dom.composerAvailabilityRetry?.click();
      expect(retry).not.toBeNull();
      await expect(retry!).resolves.toBe('applied');

      expect(retryFetch).toHaveBeenCalledTimes(1);
      expect(retryFetch.mock.calls[0]?.[1]?.method).toBeUndefined();
      expect(recoveryRequestSignal(retryFetch).aborted).toBe(false);
      expect(state.composerModelAvailability).toBe('ready');
      expect(dom.composerAvailabilityRetry?.hidden).toBe(true);
      expect(vi.getTimerCount()).toBe(0);
      await Promise.resolve();
      expect(unhandledRejections).toEqual([]);
    } finally {
      window.removeEventListener('unhandledrejection', onUnhandledRejection);
    }
  });

  it('cancels a Session recovery that enters Group chat before the response applies', async () => {
    const delayed = deferredResponse();
    const fetchMock = vi.fn<typeof fetch>().mockReturnValue(delayed.promise);
    vi.stubGlobal('fetch', fetchMock);
    state.composerSessionModelRevision = 6;
    state.composerConfigRevision = 7;
    state.composerEffectiveModelConfigured = null;
    state.composerCurrentModel = 'gateway/current';

    const recovery = refreshActiveComposerSessionModelState('main');
    await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    const signal = recoveryRequestSignal(fetchMock);
    beginComposerSessionTransition();
    state.activeGroupId = 'review-group';
    state.composerCurrentModel = 'group/model';
    state.imageCapable = true;
    expect(signal.aborted).toBe(true);
    await expect(recovery).resolves.toBe('cancelled');
    delayed.resolve(
      response({
        ...catalogPayload,
        session: { ...catalogPayload.session, model: 'stale/session-model' },
        capabilities: { image: false },
        configRevision: 99,
        models: catalogPayload.models.map((model) => ({ ...model, name: `Stale ${model.name}` })),
      }),
    );

    await Promise.resolve();
    expect(state.composerConfigRevision).toBe(7);
    expect(state.composerSessionModelRevision).toBeNull();
    expect(state.composerCurrentModel).toBe('group/model');
    expect(state.imageCapable).toBe(true);
  });

  it('cancels a recovery after the Session identity changes even if the id returns', async () => {
    const delayed = deferredResponse();
    const fetchMock = vi.fn<typeof fetch>().mockReturnValue(delayed.promise);
    vi.stubGlobal('fetch', fetchMock);
    state.composerSessionModelRevision = 6;
    state.composerConfigRevision = 7;
    state.composerEffectiveModelConfigured = null;

    const recovery = refreshActiveComposerSessionModelState('main');
    await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    const signal = recoveryRequestSignal(fetchMock);
    beginComposerSessionTransition(false, 'other-session');
    state.activeSessionId = 'other-session';
    completeComposerSessionTransition();
    beginComposerSessionTransition(false, 'main');
    state.activeSessionId = 'main';
    completeComposerSessionTransition();
    state.composerCurrentModel = 'new-identity/model';
    state.imageCapable = true;
    expect(signal.aborted).toBe(true);
    await expect(recovery).resolves.toBe('cancelled');
    delayed.resolve(
      response({
        ...catalogPayload,
        session: { ...catalogPayload.session, model: 'old-identity/model' },
        capabilities: { image: false },
        configRevision: 88,
      }),
    );

    await Promise.resolve();
    expect(state.composerConfigRevision).toBe(7);
    expect(state.composerCurrentModel).toBe('new-identity/model');
    expect(state.imageCapable).toBe(true);
  });

  it('cancels a recovery when its bound socket disconnects', async () => {
    const delayed = deferredResponse();
    const fetchMock = vi.fn<typeof fetch>().mockReturnValue(delayed.promise);
    vi.stubGlobal('fetch', fetchMock);
    state.composerSessionModelRevision = 6;
    state.composerConfigRevision = 7;
    state.composerEffectiveModelConfigured = null;
    const socket = state.ws as WebSocket & { readyState: number };

    const recovery = refreshActiveComposerSessionModelState('main');
    await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    const signal = recoveryRequestSignal(fetchMock);
    socket.readyState = WebSocket.CLOSED;
    invalidateComposerSessionModelRecovery();
    expect(signal.aborted).toBe(true);
    await expect(recovery).resolves.toBe('cancelled');
    delayed.resolve(
      response({
        ...catalogPayload,
        session: { ...catalogPayload.session, model: 'disconnected/model' },
        capabilities: { image: true },
        configRevision: 77,
      }),
    );

    await Promise.resolve();
    expect(state.composerConfigRevision).toBe(7);
    expect(state.composerSessionModelRevision).toBe(6);
    expect(state.composerCurrentModel).toBe('gateway/current');
    expect(state.imageCapable).toBe(false);
  });

  it('rejects an old daemon HTTP response after a lower socket revision establishes a new baseline', async () => {
    const oldDaemonResponse = deferredResponse();
    let sessionModelRequests = 0;
    const fetchMock = vi.fn<typeof fetch>().mockImplementation((input) => {
      const url = String(input);
      if (url.startsWith('/api/session-models')) {
        sessionModelRequests += 1;
        if (sessionModelRequests === 1) return oldDaemonResponse.promise;
        return Promise.resolve(
          response({
            ...catalogPayload,
            configRevision: 5,
            models: catalogPayload.models.map((model) => ({
              ...model,
              name: `New ${model.name}`,
            })),
          }),
        );
      }
      if (url === '/api/config') {
        return Promise.resolve(
          response({
            config: {},
            configuredModelsAvailable: true,
            explicitPrimaryModelConfigured: false,
            configRevision: 5,
          }),
        );
      }
      return Promise.reject(new Error(`Unexpected request: ${url}`));
    });
    vi.stubGlobal('fetch', fetchMock);
    state.composerConfigRevision = 100;
    state.composerSessionModelRevision = 99;
    state.composerEffectiveModelConfigured = null;
    const recovery = refreshActiveComposerSessionModelState('main');
    await vi.waitFor(() =>
      expect(
        fetchMock.mock.calls.filter(([input]) => String(input).startsWith('/api/session-models')),
      ).toHaveLength(1),
    );
    const oldDaemonSignal = recoveryRequestSignal(fetchMock);

    installOpenSocket();
    beginComposerRevisionHandshake();
    expect(oldDaemonSignal.aborted).toBe(true);
    expect(acceptComposerSocketModelPayloadRevision(5)).toBe(true);
    setComposerSessionModelConfigured(true, true, true, 5, false);
    state.composerSessionIdentityPending = false;
    state.composerCurrentModel = 'new-daemon/model';
    state.imageCapable = true;
    await expect(recovery).resolves.toBe('cancelled');
    oldDaemonResponse.resolve(
      response({
        ...catalogPayload,
        session: { ...catalogPayload.session, model: 'old-daemon/model' },
        capabilities: { image: false },
        configRevision: 101,
        models: catalogPayload.models.map((model) => ({ ...model, name: `Old ${model.name}` })),
      }),
    );

    await Promise.resolve();
    expect(state.composerConfigRevision).toBe(5);
    expect(state.composerSessionModelRevision).toBe(5);
    expect(state.composerCurrentModel).toBe('new-daemon/model');
    expect(state.imageCapable).toBe(true);

    state.composerCurrentModel = 'gateway/current';
    await openComposerModelPicker(true);
    expect(dom.composerModelPopup?.textContent).toContain('New Current Reasoner');
    expect(dom.composerModelPopup?.textContent).not.toContain('Old Current Reasoner');
  });

  it('retries when a superseding catalog request fails instead of reporting false convergence', async () => {
    const firstRecovery = deferredResponse();
    const recoveredPayload = {
      ...catalogPayload,
      session: { ...catalogPayload.session, model: 'gateway/vision', effort: 'high' },
      capabilities: { image: true },
      configRevision: 8,
    };
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockReturnValueOnce(firstRecovery.promise)
      .mockRejectedValueOnce(new Error('superseding picker request failed'))
      .mockResolvedValueOnce(response(recoveredPayload))
      .mockImplementation((input) => {
        if (String(input) === '/api/config') {
          return Promise.resolve(
            response({
              config: {},
              configuredModelsAvailable: true,
              explicitPrimaryModelConfigured: true,
              configRevision: 8,
            }),
          );
        }
        return Promise.reject(new Error(`Unexpected request: ${String(input)}`));
      });
    vi.stubGlobal('fetch', fetchMock);
    state.composerSessionModelRevision = 6;
    state.composerConfigRevision = 7;
    state.composerEffectiveModelConfigured = null;

    const recovery = refreshActiveComposerSessionModelState('main');
    await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    await openComposerModelPicker(true);
    firstRecovery.resolve(
      response({
        ...catalogPayload,
        session: {
          ...catalogPayload.session,
          model: 'superseded/model',
          effort: 'low',
        },
        capabilities: { image: false },
        configRevision: 88,
        models: catalogPayload.models.map((model) => ({
          ...model,
          name: `Superseded ${model.name}`,
        })),
      }),
    );

    await expect(recovery).resolves.toBe('applied');
    expect(
      fetchMock.mock.calls.filter(([input]) => String(input).startsWith('/api/session-models')),
    ).toHaveLength(3);
    expect(state.composerConfigRevision).toBe(8);
    expect(state.composerSessionModelRevision).toBe(8);
    expect(state.composerCurrentModel).toBe('gateway/vision');
    expect(state.composerCurrentEffort).toBe('high');
    expect(state.imageCapable).toBe(true);

    await openComposerModelPicker();
    expect(dom.composerModelPopup?.textContent).toContain('Current Reasoner');
    expect(dom.composerModelPopup?.textContent).not.toContain('Superseded Current Reasoner');
  });
});
