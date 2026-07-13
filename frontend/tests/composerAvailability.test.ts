import { beforeEach, describe, expect, it, vi } from 'vitest';
import { setLanguage } from '../src/i18n.js';
import { dom, initDomRefs, state } from '../src/state.js';
import {
  acceptComposerConfigRevision,
  acceptComposerHttpModelPayloadRevision,
  applyComposerConfig,
  acceptComposerSocketModelPayloadRevision,
  beginComposerSessionTransition,
  beginComposerRevisionHandshake,
  composerAvailabilityResolution,
  composerSessionPayloadMatchesTransition,
  getComposerConnectionGeneration,
  groupModelRosterMatches,
  handleComposerConfigSaved,
  refreshComposerAvailability,
  resolveComposerModelAvailability,
  restoreComposerSessionTransition,
  setComposerExplicitPrimaryModelConfigured,
  setComposerSessionModelConfigured,
  setGroupModelConfiguredMembers,
  syncComposerAvailability,
  updateComposerSessionTransitionFallback,
} from '../src/composerAvailability.js';

describe('composer model availability', () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    document.body.innerHTML = `
      <textarea id="input"></textarea>
      <button id="send"></button>
      <button id="stop"></button>
      <p id="composer-availability-status">
        <span id="composer-availability-message"></span>
        <button id="composer-availability-action"></button>
        <button id="composer-availability-retry"></button>
      </p>
    `;
    initDomRefs();
    state.busy = false;
    state.composerModelAvailability = 'checking';
    state.composerExplicitPrimaryModelConfigured = false;
    state.composerSessionModelOverridePresent = false;
    state.composerEffectiveModelConfigured = null;
    state.composerSessionTransitionPending = false;
    state.composerSessionTransitionTarget = '';
    state.composerSessionIdentityPending = false;
    state.sessionSwitchInFlight = false;
    state.sessionIdentityMutationInFlight = false;
    state.imageUploadInFlight = false;
    state.composerConfigRevision = null;
    state.composerSessionModelRevision = null;
    state.composerGroupModelRevision = null;
    state.sessions = [];
    state.activeGroupId = '';
    state.activeGroupMembers = [];
    state.activeGroupMemberDetails = [];
    state.groupModelConfiguredMembers = new Set();
    state.groupTargetMode = 'all';
    state.groupSelectedTargets = [];
    setLanguage('en');
  });

  it('distinguishes missing models from a missing primary Agent model', () => {
    expect(resolveComposerModelAvailability({})).toBe('models-unconfigured');
    expect(
      resolveComposerModelAvailability({
        models: {
          providers: { openai: { models: [{ id: 'gpt-4.1-mini' }] } },
        },
      }),
    ).toBe('agent-model-unconfigured');
    expect(
      resolveComposerModelAvailability(
        {
          models: {
            providers: { openai: { models: [{ id: 'gpt-4.1-mini' }] } },
          },
          agents: { defaults: { model: { primary: 'openai/gpt-4.1-mini' } } },
        },
        true,
      ),
    ).toBe('ready');
    expect(
      resolveComposerModelAvailability(
        {
          agents: { defaults: { model: { primary: 'openai/gpt-4.1-mini' } } },
        },
        true,
      ),
    ).toBe('ready');
    expect(resolveComposerModelAvailability({}, true)).toBe('ready');
    expect(resolveComposerModelAvailability({}, false, true)).toBe('ready');
    expect(
      resolveComposerModelAvailability(
        { models: { providers: { stale: { models: [{ id: 'removed' }] } } } },
        false,
        false,
        false,
      ),
    ).toBe('models-unconfigured');
  });

  it('waits for the Session status after accepting a validated primary model', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({
        ok: true,
        json: async () => ({ config: {}, explicitPrimaryModelConfigured: true }),
      }),
    );

    await refreshComposerAvailability();

    expect(state.composerModelAvailability).toBe('checking');
    expect(dom.sendBtn?.disabled).toBe(true);

    setComposerSessionModelConfigured(false, false, true);
    expect(state.composerModelAvailability).toBe('ready');
    expect(dom.sendBtn?.disabled).toBe(false);
  });

  it('keeps a validated global model ready when the config document is unavailable', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({
        ok: true,
        json: async () => ({
          config: null,
          parse_error: 'invalid json',
          explicitPrimaryModelConfigured: true,
        }),
      }),
    );

    await refreshComposerAvailability();

    expect(state.composerModelAvailability).toBe('checking');
    setComposerSessionModelConfigured(false, false, true);
    expect(state.composerModelAvailability).toBe('ready');
    expect(dom.sendBtn?.disabled).toBe(false);
  });

  it('discards an older config document after a WebSocket model update', async () => {
    let resolveRequest!: (value: unknown) => void;
    const fetchMock = vi
      .fn()
      .mockReturnValueOnce(
        new Promise((resolve) => {
          resolveRequest = resolve;
        }),
      )
      .mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          config: {},
          explicitPrimaryModelConfigured: false,
          configuredModelsAvailable: false,
        }),
      });
    vi.stubGlobal('fetch', fetchMock);

    const refresh = refreshComposerAvailability();
    setComposerSessionModelConfigured(false, false, false);
    setComposerExplicitPrimaryModelConfigured(false);
    resolveRequest({
      ok: true,
      json: async () => ({
        config: { models: { providers: { stale: { models: [{ id: 'old-model' }] } } } },
        explicitPrimaryModelConfigured: false,
        configuredModelsAvailable: true,
      }),
    });
    await refresh;
    await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
    await vi.waitFor(() => expect(state.composerModelAvailability).toBe('models-unconfigured'));

    expect(dom.input?.placeholder).toContain('Configure a model');
  });

  it('uses the sanitized runtime catalog instead of stale raw providers', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({
        ok: true,
        json: async () => ({
          config: {
            models: { providers: { openai: { models: [{ id: 'gpt-4.1-mini' }] } } },
            agents: { defaults: { model: { primary: 'unknown/missing-model' } } },
          },
          explicitPrimaryModelConfigured: false,
          configuredModelsAvailable: false,
        }),
      }),
    );

    await refreshComposerAvailability();
    setComposerSessionModelConfigured(false, false, false);

    expect(state.composerModelAvailability).toBe('models-unconfigured');
    expect(dom.sendBtn?.disabled).toBe(true);
  });

  it('disables sending and localizes both unconfigured states', () => {
    setComposerSessionModelConfigured(false, false, false);
    applyComposerConfig({});
    expect(dom.sendBtn?.disabled).toBe(true);
    expect(dom.input?.placeholder).toBe('Configure a model in Settings before sending a message.');
    expect(document.getElementById('composer-availability-message')?.textContent).toBe(
      'Configure a model in Settings before sending a message.',
    );
    expect(dom.composerAvailabilityStatus?.hidden).toBe(false);
    expect(dom.composerAvailabilityAction?.hidden).toBe(false);
    expect(dom.composerAvailabilityAction?.textContent).toBe('Configure models');
    expect(composerAvailabilityResolution()).toBe('configure-models');

    setLanguage('zh-CN');
    applyComposerConfig({
      models: { providers: { openai: { models: [{ id: 'gpt-4.1-mini' }] } } },
    });
    expect(dom.sendBtn?.disabled).toBe(true);
    expect(dom.input?.placeholder).toBe('Agent 模型未配置，请先在设置中指定主模型。');
    expect(dom.composerAvailabilityAction?.textContent).toBe('配置代理模型');
    expect(composerAvailabilityResolution()).toBe('configure-agent');
  });

  it('enables sending only after both models and the primary Agent model are configured', () => {
    state.composerExplicitPrimaryModelConfigured = true;
    applyComposerConfig({
      models: { providers: { openai: { models: [{ id: 'gpt-4.1-mini' }] } } },
      agents: { defaults: { model: { primary: 'openai/gpt-4.1-mini' } } },
    });
    setComposerSessionModelConfigured(false, false, true);

    expect(state.composerModelAvailability).toBe('ready');
    expect(dom.sendBtn?.disabled).toBe(false);
    expect(dom.input?.placeholder).toContain('Message LingClaw');
    expect(dom.composerAvailabilityStatus?.hidden).toBe(true);
    expect(dom.composerAvailabilityAction?.hidden).toBe(true);
  });

  it('keeps sending disabled while config cannot be loaded', async () => {
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new Error('offline')));
    setComposerSessionModelConfigured(false, false, false);

    await refreshComposerAvailability();

    expect(state.composerModelAvailability).toBe('config-unavailable');
    expect(dom.sendBtn?.disabled).toBe(true);
    expect(dom.input?.placeholder).toContain('configuration is unavailable');
    expect(dom.composerAvailabilityRetry?.hidden).toBe(false);
  });

  it('recovers after retrying a transient config failure', async () => {
    const fetchMock = vi
      .fn()
      .mockRejectedValueOnce(new Error('offline'))
      .mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          config: { agents: { defaults: { model: { primary: 'openai/gpt-4.1-mini' } } } },
          explicitPrimaryModelConfigured: true,
        }),
      });
    vi.stubGlobal('fetch', fetchMock);
    setComposerSessionModelConfigured(false, false, false);

    await refreshComposerAvailability();
    await refreshComposerAvailability();
    setComposerSessionModelConfigured(false, false, true);

    expect(state.composerModelAvailability).toBe('ready');
    expect(dom.sendBtn?.disabled).toBe(false);
    expect(dom.composerAvailabilityRetry?.hidden).toBe(true);
  });

  it('applies the config-saved event without another request', () => {
    handleComposerConfigSaved(
      new CustomEvent('lingclaw:config-saved', {
        detail: {
          config: {
            models: { providers: { openai: { models: [{ id: 'gpt-4.1-mini' }] } } },
            agents: { defaults: { model: { primary: 'openai/gpt-4.1-mini' } } },
          },
          explicitPrimaryModelConfigured: true,
        },
      }),
    );

    expect(state.composerModelAvailability).toBe('checking');
    setComposerSessionModelConfigured(false, false, true);
    expect(state.composerModelAvailability).toBe('ready');
    expect(dom.sendBtn?.disabled).toBe(false);
  });

  it('ignores an older refresh after Settings saves newer configuration', async () => {
    let resolveRequest!: (value: unknown) => void;
    vi.stubGlobal(
      'fetch',
      vi.fn().mockReturnValue(
        new Promise((resolve) => {
          resolveRequest = resolve;
        }),
      ),
    );

    const refresh = refreshComposerAvailability();
    handleComposerConfigSaved(
      new CustomEvent('lingclaw:config-saved', {
        detail: {
          config: {
            models: { providers: { openai: { models: [{ id: 'gpt-4.1-mini' }] } } },
            agents: { defaults: { model: { primary: 'openai/gpt-4.1-mini' } } },
          },
          explicitPrimaryModelConfigured: true,
        },
      }),
    );
    setComposerSessionModelConfigured(false, false, true);
    resolveRequest({
      ok: true,
      json: async () => ({ config: {} }),
    });
    await refresh;

    expect(state.composerModelAvailability).toBe('ready');
    expect(dom.sendBtn?.disabled).toBe(false);
  });

  it('uses the validated primary flag supplied by a Settings save event', () => {
    handleComposerConfigSaved(
      new CustomEvent('lingclaw:config-saved', {
        detail: { config: {}, explicitPrimaryModelConfigured: true },
      }),
    );

    expect(state.composerExplicitPrimaryModelConfigured).toBe(true);
    expect(state.composerModelAvailability).toBe('checking');
    setComposerSessionModelConfigured(false, false, true);
    expect(state.composerModelAvailability).toBe('ready');
  });

  it('enables the current session after an explicit model override', () => {
    applyComposerConfig({});
    setComposerSessionModelConfigured(true, true);

    expect(state.composerModelAvailability).toBe('ready');
    expect(dom.sendBtn?.disabled).toBe(false);
  });

  it('does not let a valid global model hide an invalid Session override', () => {
    state.composerExplicitPrimaryModelConfigured = true;
    applyComposerConfig(
      { models: { providers: { openai: { models: [{ id: 'gpt-4.1-mini' }] } } } },
      true,
    );

    setComposerSessionModelConfigured(true, false, false);

    expect(state.composerModelAvailability).toBe('session-model-unconfigured');
    expect(dom.sendBtn?.disabled).toBe(true);
  });

  it('localizes an invalid Session override independently of the global model', () => {
    applyComposerConfig({}, false);
    setComposerSessionModelConfigured(true, false, false);

    expect(state.composerModelAvailability).toBe('session-model-unconfigured');
    expect(dom.input?.placeholder).toContain('current Session model is no longer available');
    expect(dom.composerAvailabilityAction?.textContent).toBe('Choose model');
    expect(composerAvailabilityResolution()).toBe('choose-session-model');

    setComposerExplicitPrimaryModelConfigured(true);
    expect(state.composerModelAvailability).toBe('session-model-unconfigured');

    setLanguage('zh-CN');
    syncComposerAvailability();
    expect(dom.input?.placeholder).toBe(
      '当前 Session 的模型已不可用，请使用 /model 选择其他模型。',
    );
    expect(dom.composerAvailabilityAction?.textContent).toBe('选择模型');
  });

  it('makes a config-only newer revision stale until the matching Session status arrives', () => {
    applyComposerConfig({}, true, 10);
    setComposerExplicitPrimaryModelConfigured(true, 10);
    setComposerSessionModelConfigured(false, false, true, 10);
    expect(state.composerModelAvailability).toBe('ready');

    applyComposerConfig({}, true, 11);
    expect(state.composerConfigRevision).toBe(11);
    expect(state.composerModelAvailability).toBe('checking');
    expect(dom.sendBtn?.disabled).toBe(true);

    setComposerSessionModelConfigured(false, false, true, 11);
    expect(state.composerModelAvailability).toBe('ready');
  });

  it('rejects model and config payloads from an older revision', () => {
    applyComposerConfig({}, true, 20);
    setComposerExplicitPrimaryModelConfigured(true, 20);
    setComposerSessionModelConfigured(false, false, true, 20);

    expect(applyComposerConfig({}, false, 19)).toBe(false);
    expect(setComposerSessionModelConfigured(true, false, false, 19)).toBe(false);
    expect(state.composerConfigRevision).toBe(20);
    expect(state.composerSessionModelOverridePresent).toBe(false);
    expect(state.composerModelAvailability).toBe('ready');
  });

  it('limits retries when config responses remain behind the accepted revision', async () => {
    applyComposerConfig({}, false, 50);
    setComposerSessionModelConfigured(false, false, false, 50);
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({ config: {}, configRevision: 49 }),
    });
    vi.stubGlobal('fetch', fetchMock);

    await refreshComposerAvailability();
    await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(state.composerModelAvailability).toBe('config-unavailable');
  });

  it('does not remain checking when versioned config refreshes omit the revision', async () => {
    applyComposerConfig({}, false, 50);
    setComposerSessionModelConfigured(false, false, false, 50);
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({ config: {} }),
    });
    vi.stubGlobal('fetch', fetchMock);

    await refreshComposerAvailability();
    await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
    await vi.waitFor(() => expect(state.composerModelAvailability).toBe('config-unavailable'));
  });

  it('refreshes the sanitized catalog after a newer socket model revision', async () => {
    applyComposerConfig(
      { models: { providers: { openai: { models: [{ id: 'old-model' }] } } } },
      true,
      70,
    );
    setComposerSessionModelConfigured(false, false, false, 70);
    expect(state.composerModelAvailability).toBe('agent-model-unconfigured');

    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({
        config: {},
        configuredModelsAvailable: false,
        explicitPrimaryModelConfigured: false,
        configRevision: 71,
      }),
    });
    vi.stubGlobal('fetch', fetchMock);

    expect(acceptComposerSocketModelPayloadRevision(71)).toBe(true);
    setComposerExplicitPrimaryModelConfigured(false, 71);
    setComposerSessionModelConfigured(false, false, false, 71);
    expect(state.composerModelAvailability).toBe('checking');

    await vi.waitFor(() => expect(state.composerModelAvailability).toBe('models-unconfigured'));
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it('refreshes the catalog when HTTP Group status advances before equal WS status', async () => {
    state.activeGroupId = 'review-group';
    state.activeGroupMembers = ['worker-a'];
    state.groupTargetMode = 'all';
    applyComposerConfig(
      { models: { providers: { openai: { models: [{ id: 'old-model' }] } } } },
      true,
      80,
    );
    setGroupModelConfiguredMembers([], 80);
    expect(state.composerModelAvailability).toBe('agent-model-unconfigured');

    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({
        config: {},
        configuredModelsAvailable: false,
        explicitPrimaryModelConfigured: false,
        configRevision: 81,
      }),
    });
    vi.stubGlobal('fetch', fetchMock);

    expect(acceptComposerHttpModelPayloadRevision(81)).toBe(true);
    expect(acceptComposerSocketModelPayloadRevision(81)).toBe(true);
    setGroupModelConfiguredMembers([], 81);
    expect(state.composerModelAvailability).toBe('checking');

    await vi.waitFor(() => expect(state.composerModelAvailability).toBe('models-unconfigured'));
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it('clears the previous Session override before switching', () => {
    applyComposerConfig({});
    setComposerSessionModelConfigured(true, true);

    beginComposerSessionTransition();

    expect(state.composerEffectiveModelConfigured).toBeNull();
    expect(state.composerSessionTransitionPending).toBe(true);
    expect(state.composerModelAvailability).toBe('checking');
    expect(dom.sendBtn?.disabled).toBe(true);
  });

  it('restores the previous model state when a slash Session switch fails', () => {
    applyComposerConfig({});
    setComposerSessionModelConfigured(true, true);

    beginComposerSessionTransition(true);
    expect(state.composerModelAvailability).toBe('checking');

    restoreComposerSessionTransition();
    expect(state.composerSessionTransitionPending).toBe(false);
    expect(state.composerModelAvailability).toBe('ready');
  });

  it('accepts an unambiguous full-id case alias returned on Windows', () => {
    state.activeSessionId = 'source';
    state.sessions = [
      { id: 'source', name: 'Source' },
      { id: 'research-notes', name: 'Research Notes' },
    ];
    setComposerSessionModelConfigured(false, false, true);
    beginComposerSessionTransition(true, '  RESEARCH-NOTES  ');

    expect(composerSessionPayloadMatchesTransition('research-notes')).toBe(true);
    expect(composerSessionPayloadMatchesTransition(' Research-Notes ')).toBe(false);
    expect(composerSessionPayloadMatchesTransition('source')).toBe(false);
  });

  it('accepts the server fallback from a UI-driven new-socket Session switch', () => {
    state.activeSessionId = 'source';
    setComposerSessionModelConfigured(false, false, true);
    beginComposerSessionTransition(false, 'missing-or-corrupt');

    expect(state.composerSessionTransitionTarget).toBe('');
    expect(composerSessionPayloadMatchesTransition('main')).toBe(true);
  });

  it('accepts the resolved full id for a unique slash switch prefix', () => {
    state.activeSessionId = 'source';
    state.sessions = [
      { id: 'source', name: 'Source' },
      { id: 'research-notes', name: 'Research Notes' },
    ];
    setComposerSessionModelConfigured(false, false, true);
    beginComposerSessionTransition(true, 'research');

    expect(composerSessionPayloadMatchesTransition('research-notes')).toBe(true);
    expect(composerSessionPayloadMatchesTransition('source')).toBe(false);
  });

  it('does not fold distinct case-sensitive Session ids together', () => {
    state.activeSessionId = 'source';
    state.sessions = [
      { id: 'source', name: 'Source' },
      { id: 'Foo', name: 'Upper' },
      { id: 'foo', name: 'Lower' },
    ];
    setComposerSessionModelConfigured(false, false, true);
    beginComposerSessionTransition(true, 'foo');

    expect(composerSessionPayloadMatchesTransition('foo')).toBe(true);
    expect(composerSessionPayloadMatchesTransition('Foo')).toBe(false);
  });

  it('restores the newest source Session status when a slash switch fails', () => {
    state.activeSessionId = 'source';
    applyComposerConfig({}, true, 30);
    setComposerExplicitPrimaryModelConfigured(true, 30);
    setComposerSessionModelConfigured(false, false, true, 30);
    beginComposerSessionTransition(true, 'target');

    expect(updateComposerSessionTransitionFallback('source', true, false, false, true, 31)).toBe(
      true,
    );
    expect(state.composerSessionTransitionPending).toBe(true);

    restoreComposerSessionTransition();
    expect(state.composerConfigRevision).toBe(31);
    expect(state.composerSessionModelRevision).toBe(31);
    expect(state.composerSessionModelOverridePresent).toBe(true);
    expect(state.composerModelAvailability).toBe('session-model-unconfigured');
  });

  it('preserves a config load failure when a Session has no override', async () => {
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new Error('offline')));
    await refreshComposerAvailability();

    setComposerSessionModelConfigured(false, false);

    expect(state.composerModelAvailability).toBe('config-unavailable');
    expect(dom.composerAvailabilityRetry?.hidden).toBe(false);
  });

  it('enables model-independent slash commands but keeps /new disabled', () => {
    applyComposerConfig({});

    dom.input!.value = '/status';
    syncComposerAvailability();
    expect(dom.sendBtn?.disabled).toBe(false);
    expect(dom.sendBtn?.title).toBe('');
    expect(dom.composerAvailabilityStatus?.hidden).toBe(true);
    expect(dom.composerAvailabilityRetry?.hidden).toBe(true);

    // Unknown slash commands are still handled locally by the backend as
    // command feedback and never enter an Agent run.
    dom.input!.value = '/bogus';
    syncComposerAvailability();
    expect(dom.sendBtn?.disabled).toBe(false);

    dom.input!.value = '/new';
    syncComposerAvailability();
    expect(dom.sendBtn?.disabled).toBe(true);
    expect(dom.composerAvailabilityStatus?.hidden).toBe(false);

    beginComposerSessionTransition(true, 'target');
    dom.input!.value = '/switch another-target';
    syncComposerAvailability();
    expect(dom.sendBtn?.disabled).toBe(true);

    dom.input!.value = '/status';
    syncComposerAvailability();
    expect(dom.sendBtn?.disabled).toBe(false);
    expect(dom.composerAvailabilityStatus?.hidden).toBe(true);
  });

  it('blocks ordinary messages during image upload but keeps model-free commands available', () => {
    state.composerModelAvailability = 'ready';
    state.imageUploadInFlight = true;

    dom.input!.value = 'send this with the image';
    syncComposerAvailability();
    expect(dom.sendBtn?.disabled).toBe(true);
    expect(dom.input?.placeholder).toContain('image upload');

    dom.input!.value = '/status';
    syncComposerAvailability();
    expect(dom.sendBtn?.disabled).toBe(false);

    state.imageUploadInFlight = false;
    dom.input!.value = 'send this with the image';
    syncComposerAvailability();
    expect(dom.sendBtn?.disabled).toBe(false);
  });

  it('moves focus back to the composer before hiding the Retry button', async () => {
    const fetchMock = vi
      .fn()
      .mockRejectedValueOnce(new Error('offline'))
      .mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          config: {},
          explicitPrimaryModelConfigured: false,
          configuredModelsAvailable: false,
        }),
      });
    vi.stubGlobal('fetch', fetchMock);
    await refreshComposerAvailability();
    dom.composerAvailabilityRetry?.focus();

    const retry = refreshComposerAvailability();

    expect(document.activeElement).toBe(dom.input);
    await retry;
  });

  it('matches backend mention punctuation and reflects Group target readiness', () => {
    applyComposerConfig({});
    state.activeGroupId = 'review-group';
    state.activeGroupMembers = ['worker-a', 'worker-b'];
    state.activeGroupMemberDetails = [
      { id: 'worker-a', name: 'Worker A', role: 'member' },
      { id: 'worker-b', name: 'Worker B', role: 'member' },
    ];
    state.groupModelConfiguredMembers = new Set(['worker-a']);
    state.groupTargetMode = 'mentions';

    dom.input!.value = '(@worker-a) review this';
    syncComposerAvailability();
    expect(dom.sendBtn?.disabled).toBe(false);
    expect(dom.composerAvailabilityStatus?.hidden).toBe(true);

    dom.input!.value = '@worker-a... review this';
    syncComposerAvailability();
    expect(dom.sendBtn?.disabled).toBe(false);

    dom.input!.value = '@all. review this';
    syncComposerAvailability();
    expect(dom.sendBtn?.disabled).toBe(true);
    expect(dom.input?.placeholder).toContain('Worker B');
    expect(dom.composerAvailabilityStatus?.textContent).toContain('Worker B');
  });

  it('clears transition pending state when a Group model payload arrives', () => {
    state.activeGroupId = 'review-group';
    state.activeGroupMembers = ['worker-a'];
    state.groupTargetMode = 'all';
    beginComposerSessionTransition();

    setGroupModelConfiguredMembers(['worker-a'], 40);

    expect(state.composerSessionTransitionPending).toBe(false);
    expect(state.composerGroupModelRevision).toBe(40);
    expect(dom.sendBtn?.disabled).toBe(false);
  });

  it('matches Group model payloads only to the active member roster', () => {
    state.activeGroupMembers = ['worker-a', 'worker-b'];

    expect(groupModelRosterMatches(['worker-b', 'worker-a'])).toBe(true);
    expect(groupModelRosterMatches(['worker-a'])).toBe(false);
    expect(groupModelRosterMatches(['worker-a', 'worker-c'])).toBe(false);
  });

  it('never enables slash commands in an active Group', () => {
    state.activeGroupId = 'review-group';
    state.activeGroupMembers = ['worker-a'];
    state.groupTargetMode = 'all';
    setGroupModelConfiguredMembers(['worker-a']);
    dom.input!.value = '/status';

    syncComposerAvailability();
    expect(dom.sendBtn?.disabled).toBe(true);
    expect(dom.input?.placeholder).toBe('Slash commands are not supported in group chat.');
    expect(dom.sendBtn?.title).toBe('Slash commands are not supported in group chat.');

    setGroupModelConfiguredMembers([]);
    syncComposerAvailability();
    expect(dom.sendBtn?.disabled).toBe(true);
    expect(dom.input?.placeholder).toBe('Slash commands are not supported in group chat.');
  });

  it('prompts for a target when selected or mentioned Group targets are empty', () => {
    state.activeGroupId = 'review-group';
    state.activeGroupMembers = ['worker-a', 'worker-b'];
    setGroupModelConfiguredMembers(['worker-a', 'worker-b']);

    state.groupTargetMode = 'selected';
    state.groupSelectedTargets = [];
    dom.input!.value = 'Please review this';
    syncComposerAvailability();
    expect(dom.sendBtn?.disabled).toBe(true);
    expect(dom.input?.placeholder).toBe('Select at least one group member before sending.');

    state.groupTargetMode = 'mentions';
    dom.input!.value = 'Please review this without a mention';
    syncComposerAvailability();
    expect(dom.sendBtn?.disabled).toBe(true);
    expect(dom.input?.placeholder).toBe('Select at least one group member before sending.');
  });

  it('does not reuse Group member readiness during a socket handshake', () => {
    vi.stubGlobal('fetch', vi.fn().mockReturnValue(new Promise(() => {})));
    state.activeGroupId = 'review-group';
    state.activeGroupMembers = ['worker-a'];
    state.groupTargetMode = 'all';
    setGroupModelConfiguredMembers(['worker-a'], 45);
    expect(dom.sendBtn?.disabled).toBe(false);

    beginComposerRevisionHandshake();
    expect(state.composerModelAvailability).toBe('checking');
    expect(dom.sendBtn?.disabled).toBe(true);
    expect(dom.input?.placeholder).toBe('Checking model configuration...');

    expect(acceptComposerSocketModelPayloadRevision(45)).toBe(true);
    setGroupModelConfiguredMembers(['worker-a'], 45);
    expect(dom.sendBtn?.disabled).toBe(false);
  });

  it('keeps the revision on same-process reconnect and accepts a lower restart baseline', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({
        ok: true,
        json: async () => ({ config: {}, configRevision: 50 }),
      }),
    );
    applyComposerConfig({}, true, 50);
    setComposerSessionModelConfigured(false, false, true, 50);

    const generationBeforeReconnect = getComposerConnectionGeneration();
    beginComposerRevisionHandshake();
    expect(getComposerConnectionGeneration()).toBe(generationBeforeReconnect + 1);
    expect(state.composerModelAvailability).toBe('checking');
    expect(dom.sendBtn?.disabled).toBe(true);
    expect(acceptComposerSocketModelPayloadRevision(50)).toBe(true);
    expect(state.composerConfigRevision).toBe(50);

    beginComposerRevisionHandshake();
    expect(acceptComposerConfigRevision(49)).toBe(false);
    expect(acceptComposerSocketModelPayloadRevision(5)).toBe(true);
    expect(state.composerConfigRevision).toBe(5);
    expect(state.composerSessionModelRevision).toBeNull();

    await vi.waitFor(() => expect(fetch).toHaveBeenCalled());
  });

  it('does not let an unversioned payload consume a versioned socket handshake', () => {
    vi.stubGlobal('fetch', vi.fn().mockReturnValue(new Promise(() => {})));
    applyComposerConfig({}, true, 60);
    setComposerSessionModelConfigured(false, false, true, 60);

    beginComposerRevisionHandshake();
    expect(acceptComposerSocketModelPayloadRevision(undefined)).toBe(false);
    expect(state.composerModelAvailability).toBe('checking');

    expect(acceptComposerSocketModelPayloadRevision(60)).toBe(true);
    setComposerSessionModelConfigured(false, false, true, 60);
    expect(state.composerModelAvailability).toBe('ready');
  });

  it('does not treat a malformed revision as a legacy socket payload', () => {
    vi.stubGlobal('fetch', vi.fn().mockReturnValue(new Promise(() => {})));
    beginComposerRevisionHandshake();

    expect(acceptComposerSocketModelPayloadRevision('invalid')).toBe(false);
    expect(state.composerModelAvailability).toBe('checking');

    expect(acceptComposerSocketModelPayloadRevision(undefined)).toBe(true);
  });

  it('refreshes the active placeholder when the language changes', () => {
    setComposerSessionModelConfigured(false, false, false);
    applyComposerConfig({});
    setLanguage('zh-CN');
    syncComposerAvailability();

    expect(dom.input?.placeholder).toBe('模型未配置，请先前往设置添加模型。');
  });
});
