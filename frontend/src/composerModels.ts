import { tr } from './i18n.js';
import { createIcon } from './icons.js';
import { updateAttachButton } from './images.js';
import { renderSessionDrawer } from './renderers/sessions.js';
import {
  applyComposerHttpSessionModelState,
  syncComposerAvailability,
} from './composerAvailability.js';
import { closeComposerPopovers, registerComposerPopover } from './composerPopovers.js';
import { dom, state } from './state.js';

export interface ComposerModelCatalogEntry {
  ref: string;
  provider: string;
  id: string;
  name: string;
  input: string[];
  reasoning: boolean;
  efforts: string[];
  defaultEffort: string;
}

interface SessionModelsResponse {
  session?: {
    id?: string;
    model?: string;
    effort?: string;
    modelOverridePresent?: boolean;
    modelOverrideConfigured?: boolean;
    effectiveModelConfigured?: boolean;
  };
  explicitPrimaryModelConfigured?: boolean;
  capabilities?: { image?: boolean };
  models?: ComposerModelCatalogEntry[];
  configRevision?: number;
  code?: string;
  error?: string;
}

let catalogRevision: number | null = null;
let catalog: ComposerModelCatalogEntry[] = [];
let selectedModel: ComposerModelCatalogEntry | null = null;
let requestSequence = 0;
let initialized = false;

function normalizedRevision(value: unknown): number | null {
  const revision = Number(value);
  return Number.isSafeInteger(revision) && revision >= 0 ? revision : null;
}

function revisionIsStale(revision: number | null): boolean {
  return (
    revision !== null &&
    state.composerConfigRevision !== null &&
    revision < state.composerConfigRevision
  );
}

function modelEntry(modelRef: string): ComposerModelCatalogEntry | undefined {
  return catalog.find((model) => model.ref === modelRef);
}

function fallbackModelName(modelRef: string): string {
  return modelRef.split('/').pop() || modelRef || tr('composer.chooseModel');
}

function localizedEffort(effort: string): string {
  const key = `composer.effort.${effort}`;
  const value = tr(key);
  return value === key ? effort : value;
}

function localizedApiError(payload: SessionModelsResponse, status: number): string {
  switch (payload.code) {
    case 'model_unavailable':
      return tr('composer.modelSelectionUnavailable');
    case 'effort_not_supported':
      return tr('composer.effortSelectionUnsupported');
    case 'session_busy':
      return tr('composer.modelSwitchRunning');
    case 'storage_protected':
      return tr('storage.protectedLabel');
    case 'session_not_found':
      return tr('composer.modelSessionUnavailable');
    default:
      return payload.error || `HTTP ${status}`;
  }
}

function modelSwitchBlockReason(): string {
  if (state.activeGroupId) return tr('composer.groupModelPickerUnsupported');
  if (state.storageMode === 'protected') return tr('storage.protectedLabel');
  if (state.busy) return tr('composer.modelSwitchRunning');
  if (state.imageUploadInFlight) return tr('composer.modelSwitchUploading');
  if (
    state.sessionSwitchInFlight ||
    state.sessionIdentityMutationInFlight ||
    state.composerSessionTransitionPending ||
    state.composerSessionIdentityPending
  ) {
    return tr('composer.modelSwitchSessionChanging');
  }
  if (state.composerModelSwitchInFlight) return tr('composer.modelSwitchSaving');
  return '';
}

function pickerHasExternalBlock(): boolean {
  return Boolean(
    state.activeGroupId ||
    state.storageMode === 'protected' ||
    state.busy ||
    state.imageUploadInFlight ||
    state.sessionSwitchInFlight ||
    state.sessionIdentityMutationInFlight ||
    state.composerSessionTransitionPending ||
    state.composerSessionIdentityPending,
  );
}

function modelLabel(): string {
  const entry = modelEntry(state.composerCurrentModel);
  const name = entry?.name || fallbackModelName(state.composerCurrentModel);
  if (!entry?.reasoning) return name;
  return `${name} · ${localizedEffort(state.composerCurrentEffort)}`;
}

export function syncComposerModelControls(): void {
  if (!dom.composerModelBtn || !dom.composerModelLabel) return;
  const hidden = Boolean(state.activeGroupId);
  const wrapper = dom.composerModelBtn.closest<HTMLElement>('.composer-model-wrapper');
  const reason = modelSwitchBlockReason();
  if (wrapper) wrapper.hidden = hidden;
  dom.composerModelBtn.hidden = hidden;
  // Keep the control focusable when switching is blocked. Activating it opens
  // an explanatory status instead of silently doing nothing on touch devices.
  dom.composerModelBtn.disabled = false;
  dom.composerModelBtn.setAttribute('aria-disabled', String(!hidden && Boolean(reason)));
  dom.composerModelBtn.title = reason || modelLabel();
  dom.composerModelLabel.textContent = state.composerCurrentModel
    ? modelLabel()
    : tr('composer.chooseModel');
  // Keep the picker visible while its own atomic PUT is pending so the user
  // sees the saving state, but close it as soon as the surrounding Session or
  // runtime context changes. A late response must not leave an old Session's
  // error UI open over the newly selected Session.
  if (pickerHasExternalBlock() || (reason && !state.composerModelSwitchInFlight)) {
    closeComposerModelPicker(false);
  }
}

export function applyComposerModelPayload(data: Record<string, unknown>): void {
  const revision = normalizedRevision(data.configRevision);
  if (revisionIsStale(revision)) return;
  if (typeof data.model === 'string' && data.model.trim()) {
    state.composerCurrentModel = data.model.trim();
  }
  if (typeof data.effort === 'string' && data.effort.trim()) {
    state.composerCurrentEffort = data.effort.trim().toLowerCase();
  }
  if (revision !== null && catalogRevision !== null && revision !== catalogRevision) {
    invalidateComposerModelCatalog();
  }
  syncComposerModelControls();
  if (
    initialized &&
    !state.activeGroupId &&
    state.composerCurrentModel &&
    (!modelEntry(state.composerCurrentModel) || catalogRevision === null)
  ) {
    refreshComposerModelCatalog();
  }
}

export function invalidateComposerModelCatalog(): void {
  catalogRevision = null;
  // Keep the last safe names/capabilities for the compact toolbar while the
  // next picker open refreshes the stale directory. Session-level model
  // changes also advance configRevision even though the directory itself did
  // not change, so clearing it here would briefly regress the label to a raw
  // model id after every successful selection.
  selectedModel = null;
}

function safeCatalog(value: unknown): ComposerModelCatalogEntry[] {
  if (!Array.isArray(value)) return [];
  return value
    .map((raw) => {
      if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return null;
      const item = raw as Record<string, unknown>;
      const modelRef = String(item.ref || '').trim();
      const provider = String(item.provider || '').trim();
      const id = String(item.id || '').trim();
      if (!modelRef || !provider || !id) return null;
      const efforts = Array.isArray(item.efforts)
        ? item.efforts
            .map(String)
            .map((effort) => effort.trim())
            .filter(Boolean)
        : [];
      const defaultEffort = String(item.defaultEffort || efforts[0] || 'off').trim();
      return {
        ref: modelRef,
        provider,
        id,
        name: String(item.name || id).trim() || id,
        input: Array.isArray(item.input) ? item.input.map(String) : ['text'],
        reasoning: item.reasoning === true,
        efforts: efforts.length > 0 ? efforts : ['off'],
        defaultEffort,
      } satisfies ComposerModelCatalogEntry;
    })
    .filter((entry): entry is ComposerModelCatalogEntry => entry !== null);
}

async function loadCatalog(staleRetriesRemaining = 1): Promise<void> {
  const activeSessionId = state.activeSessionId || 'main';
  const revisionMatches =
    catalog.length > 0 &&
    catalogRevision !== null &&
    catalogRevision === state.composerConfigRevision;
  if (revisionMatches && state.composerCurrentModel) return;

  const sequence = ++requestSequence;
  const response = await fetch(
    `/api/session-models?session=${encodeURIComponent(activeSessionId)}`,
    { cache: 'no-store' },
  );
  const payload = (await response.json().catch(() => ({}))) as SessionModelsResponse;
  if (!response.ok) throw new Error(localizedApiError(payload, response.status));
  if (sequence !== requestSequence || (state.activeSessionId || 'main') !== activeSessionId) return;

  const responseRevision = normalizedRevision(payload.configRevision);
  if (revisionIsStale(responseRevision)) {
    if (staleRetriesRemaining > 0) {
      await loadCatalog(staleRetriesRemaining - 1);
      return;
    }
    throw new Error(tr('composer.modelCatalogChanged'));
  }
  if (
    !applyComposerHttpSessionModelState(
      {
        ...payload.session,
        explicitPrimaryModelConfigured: payload.explicitPrimaryModelConfigured,
      },
      payload.configRevision,
    )
  ) {
    throw new Error(tr('composer.modelCatalogChanged'));
  }

  catalog = safeCatalog(payload.models);
  catalogRevision = responseRevision;
  if (payload.session?.model) state.composerCurrentModel = payload.session.model;
  if (payload.session?.effort) state.composerCurrentEffort = payload.session.effort;
  if (typeof payload.capabilities?.image === 'boolean') {
    state.imageCapable = payload.capabilities.image;
    updateAttachButton();
  }
  syncComposerModelControls();
}

function capabilityBadge(label: string): HTMLElement {
  const span = document.createElement('span');
  span.className = 'composer-model-capability';
  span.textContent = label;
  return span;
}

function renderLoading(): void {
  if (!dom.composerModelPopup) return;
  dom.composerModelPopup.replaceChildren();
  const status = document.createElement('div');
  status.className = 'composer-model-status';
  status.textContent = tr('common.loading');
  dom.composerModelPopup.appendChild(status);
}

function renderBlocked(reason: string): void {
  if (!dom.composerModelPopup) return;
  dom.composerModelPopup.replaceChildren();
  const status = document.createElement('div');
  status.className = 'composer-model-status';
  status.setAttribute('role', 'status');
  status.textContent = reason;
  dom.composerModelPopup.appendChild(status);
}

function renderError(error: unknown, context: 'catalog' | 'selection' = 'catalog'): void {
  if (!dom.composerModelPopup) return;
  dom.composerModelPopup.replaceChildren();
  const status = document.createElement('div');
  status.className = 'composer-model-status is-error';
  status.setAttribute('role', 'alert');
  status.textContent = tr(
    context === 'selection' ? 'composer.modelSelectionError' : 'composer.modelCatalogError',
    {
      error: error instanceof Error ? error.message : String(error),
    },
  );
  const retry = document.createElement('button');
  retry.type = 'button';
  retry.className = 'composer-model-retry';
  retry.textContent = tr('composer.retryConfig');
  retry.addEventListener('click', () => void openComposerModelPicker(true));
  dom.composerModelPopup.append(status, retry);
  queueMicrotask(() => {
    if (!dom.composerModelPopup?.hidden && retry.isConnected) retry.focus();
  });
}

function renderModelResults(container: HTMLElement, query: string): void {
  container.replaceChildren();
  const normalizedQuery = query.trim().toLowerCase();
  const filtered = catalog.filter((model) =>
    [model.name, model.id, model.provider, model.ref].some((value) =>
      value.toLowerCase().includes(normalizedQuery),
    ),
  );
  if (filtered.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'composer-model-status';
    empty.textContent = tr('composer.noModelsFound');
    container.appendChild(empty);
  } else {
    const providers = new Map<string, ComposerModelCatalogEntry[]>();
    for (const model of filtered) {
      const group = providers.get(model.provider) || [];
      group.push(model);
      providers.set(model.provider, group);
    }
    for (const [provider, models] of providers) {
      const section = document.createElement('section');
      section.className = 'composer-model-provider';
      const heading = document.createElement('div');
      heading.className = 'composer-model-provider-name';
      heading.textContent = provider;
      section.appendChild(heading);
      for (const model of models) {
        const button = document.createElement('button');
        button.type = 'button';
        button.className = 'composer-model-option';
        if (model.ref === state.composerCurrentModel) button.classList.add('is-current');
        const copy = document.createElement('span');
        copy.className = 'composer-model-option-copy';
        const name = document.createElement('strong');
        name.textContent = model.name;
        const id = document.createElement('small');
        id.textContent = model.id;
        const capabilities = document.createElement('span');
        capabilities.className = 'composer-model-capabilities';
        if (model.input.includes('text')) {
          capabilities.appendChild(capabilityBadge(tr('settings.field.text')));
        }
        if (model.input.includes('image')) {
          capabilities.appendChild(capabilityBadge(tr('settings.field.image')));
        }
        if (model.reasoning) capabilities.appendChild(capabilityBadge(tr('common.reasoning')));
        copy.append(name, id, capabilities);
        button.appendChild(copy);
        const tail = document.createElement('span');
        tail.className = 'composer-model-option-tail';
        if (model.ref === state.composerCurrentModel) {
          tail.textContent = tr('common.current');
        } else if (model.reasoning) {
          tail.appendChild(createIcon('chevron-right'));
        }
        button.appendChild(tail);
        button.addEventListener('click', () => selectModel(model));
        section.appendChild(button);
      }
      container.appendChild(section);
    }
  }
}

function renderModelList(query = ''): void {
  if (!dom.composerModelPopup) return;
  selectedModel = null;
  dom.composerModelPopup.replaceChildren();

  const header = document.createElement('div');
  header.className = 'composer-model-popup-header';
  const title = document.createElement('strong');
  title.textContent = tr('composer.chooseModel');
  const search = document.createElement('input');
  search.type = 'search';
  search.className = 'composer-model-search';
  search.placeholder = tr('composer.searchModels');
  search.setAttribute('aria-label', tr('composer.searchModels'));
  search.value = query;
  const results = document.createElement('div');
  results.className = 'composer-model-results';
  search.addEventListener('input', () => renderModelResults(results, search.value));
  header.append(title, search);
  dom.composerModelPopup.append(header, results);
  renderModelResults(results, query);
  queueMicrotask(() => {
    if (!dom.composerModelPopup?.hidden && search.isConnected) search.focus();
  });
}

function selectModel(model: ComposerModelCatalogEntry): void {
  if (model.reasoning && model.efforts.length > 1) {
    renderEffortList(model);
    return;
  }
  const effort = model.efforts.includes(state.composerCurrentEffort)
    ? state.composerCurrentEffort
    : model.defaultEffort;
  void applyModelSelection(model, effort);
}

function renderEffortList(model: ComposerModelCatalogEntry): void {
  if (!dom.composerModelPopup) return;
  selectedModel = model;
  dom.composerModelPopup.replaceChildren();
  const header = document.createElement('div');
  header.className = 'composer-model-popup-header is-effort';
  const back = document.createElement('button');
  back.type = 'button';
  back.className = 'composer-model-back';
  back.appendChild(createIcon('chevron-left'));
  back.setAttribute('aria-label', tr('composer.backToModels'));
  back.addEventListener('click', () => renderModelList());
  const title = document.createElement('span');
  const strong = document.createElement('strong');
  strong.textContent = model.name;
  const subtitle = document.createElement('small');
  subtitle.textContent = tr('composer.chooseEffort');
  title.append(strong, subtitle);
  header.append(back, title);
  dom.composerModelPopup.appendChild(header);

  const selected = model.efforts.includes(state.composerCurrentEffort)
    ? state.composerCurrentEffort
    : model.defaultEffort;
  const list = document.createElement('div');
  list.className = 'composer-effort-list';
  for (const effort of model.efforts) {
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'composer-effort-option';
    button.classList.toggle('is-current', effort === selected);
    const label = document.createElement('span');
    label.textContent = localizedEffort(effort);
    const meta = document.createElement('small');
    meta.textContent = effort === model.defaultEffort ? tr('common.default') : '';
    button.append(label, meta);
    button.addEventListener('click', () => void applyModelSelection(model, effort));
    list.appendChild(button);
  }
  dom.composerModelPopup.appendChild(list);
  queueMicrotask(() => {
    if (dom.composerModelPopup?.hidden) return;
    list.querySelector<HTMLButtonElement>('.is-current')?.focus();
  });
}

async function applyModelSelection(
  model: ComposerModelCatalogEntry,
  effort: string,
): Promise<void> {
  const blockReason = modelSwitchBlockReason();
  if (blockReason) return;
  const sessionId = state.activeSessionId || 'main';
  state.composerModelSwitchInFlight = true;
  syncComposerModelControls();
  syncComposerAvailability();
  renderSessionDrawer();
  if (dom.composerModelPopup) {
    dom.composerModelPopup.querySelectorAll<HTMLButtonElement>('button').forEach((button) => {
      button.disabled = true;
    });
  }
  try {
    const response = await fetch(`/api/session-models?session=${encodeURIComponent(sessionId)}`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ model: model.ref, effort }),
    });
    const payload = (await response.json().catch(() => ({}))) as SessionModelsResponse;
    if (!response.ok) throw new Error(localizedApiError(payload, response.status));
    if (
      state.activeSessionId === sessionId &&
      payload.session &&
      applyComposerHttpSessionModelState(
        {
          ...payload.session,
          explicitPrimaryModelConfigured: payload.explicitPrimaryModelConfigured,
        },
        payload.configRevision,
      )
    ) {
      applyComposerModelPayload({ ...payload.session, configRevision: payload.configRevision });
      if (typeof payload.capabilities?.image === 'boolean') {
        state.imageCapable = payload.capabilities.image;
      }
    }
    closeComposerModelPicker(true);
    updateAttachButton();
  } catch (error) {
    renderError(error, 'selection');
  } finally {
    state.composerModelSwitchInFlight = false;
    syncComposerModelControls();
    syncComposerAvailability();
    renderSessionDrawer();
  }
}

export function closeComposerModelPicker(returnFocus = false): void {
  if (!dom.composerModelPopup || !dom.composerModelBtn) return;
  const wasOpen = !dom.composerModelPopup.hidden;
  dom.composerModelPopup.hidden = true;
  dom.composerModelBtn.setAttribute('aria-expanded', 'false');
  selectedModel = null;
  if (returnFocus && wasOpen) queueMicrotask(() => dom.composerModelBtn?.focus());
}

registerComposerPopover('models', closeComposerModelPicker);

export async function openComposerModelPicker(forceReload = false): Promise<void> {
  if (!dom.composerModelPopup || !dom.composerModelBtn) return;
  closeComposerPopovers('models');
  const blockReason = modelSwitchBlockReason();
  if (blockReason) {
    dom.composerModelPopup.hidden = false;
    dom.composerModelBtn.setAttribute('aria-expanded', 'true');
    renderBlocked(blockReason);
    return;
  }
  if (forceReload) invalidateComposerModelCatalog();
  dom.composerModelPopup.hidden = false;
  dom.composerModelBtn.setAttribute('aria-expanded', 'true');
  renderLoading();
  try {
    await loadCatalog();
    if (dom.composerModelPopup.hidden) return;
    renderModelList();
  } catch (error) {
    renderError(error);
  }
}

export function refreshLocalizedComposerModelPicker(): void {
  syncComposerModelControls();
  if (!dom.composerModelPopup?.hidden) {
    if (selectedModel) renderEffortList(selectedModel);
    else renderModelList();
  }
}

export function refreshComposerModelCatalog(): void {
  invalidateComposerModelCatalog();
  if (state.activeGroupId || !state.activeSessionId) {
    syncComposerModelControls();
    return;
  }
  void loadCatalog()
    .then(() => {
      if (!dom.composerModelPopup?.hidden) renderModelList();
    })
    .catch((error) => {
      if (!dom.composerModelPopup?.hidden) renderError(error);
    });
}

export function initComposerModelPicker(): void {
  if (initialized) return;
  initialized = true;
  dom.composerModelBtn?.addEventListener('click', (event) => {
    event.stopPropagation();
    if (dom.composerModelPopup?.hidden) void openComposerModelPicker();
    else closeComposerModelPicker(true);
  });
  document.addEventListener('click', (event) => {
    if (dom.composerModelPopup?.hidden) return;
    const wrapper = dom.composerModelBtn?.closest('.composer-model-wrapper');
    if (wrapper && !event.composedPath().includes(wrapper)) closeComposerModelPicker(false);
  });
  document.addEventListener('keydown', (event) => {
    if (event.key !== 'Escape' || dom.composerModelPopup?.hidden) return;
    event.preventDefault();
    closeComposerModelPicker(true);
  });
  document.addEventListener('lingclaw:composer-state-change', syncComposerModelControls);
  syncComposerModelControls();
}
