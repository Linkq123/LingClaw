import { tr } from './i18n.js';
import { mentionedGroupTargets } from './groupMentions.js';
import { dom, state } from './state.js';
import type { AppConfig, ConfigApiResponse } from './types/config.js';

export const CONFIG_SAVED_EVENT = 'lingclaw:config-saved';

let availabilityRequestGeneration = 0;
let explicitStateGeneration = 0;
let lastComposerConfig: AppConfig = {};
let lastConfiguredModelsAvailable: boolean | undefined;
let configLoadState: 'pending' | 'loaded' | 'unavailable' = 'pending';
let composerRevisionHandshakePending = false;
let composerConnectionGeneration = 0;
let restorableSessionTransition: {
  modelOverridePresent: boolean;
  effectiveModelConfigured: boolean | null;
  modelRevision: number | null;
  sourceSessionId: string;
  imageCapable: boolean;
  s3Capable: boolean;
  s3ConfigId: string;
  pendingImages: typeof state.pendingImages;
  targetCapabilitiesApplied: boolean;
} | null = null;

function clonePendingImages(): typeof state.pendingImages {
  return state.pendingImages.map((image) => ({ ...image }));
}

export type ComposerModelAvailability =
  | 'checking'
  | 'models-unconfigured'
  | 'agent-model-unconfigured'
  | 'session-model-unconfigured'
  | 'config-unavailable'
  | 'ready';

function normalizeConfigRevision(value: unknown): number | null {
  if (value === null || value === undefined || value === '') return null;
  const revision = typeof value === 'number' ? value : Number(value);
  return Number.isSafeInteger(revision) && revision >= 0 ? revision : null;
}

function configRevisionIsMissing(value: unknown): boolean {
  return value === null || value === undefined;
}

function modelRevisionIsCurrent(revision: number | null): boolean {
  return (
    !composerRevisionHandshakePending &&
    (state.composerConfigRevision === null || revision === state.composerConfigRevision)
  );
}

export function acceptComposerConfigRevision(value: unknown): boolean {
  const revision = normalizeConfigRevision(value);
  if (revision === null) {
    return configRevisionIsMissing(value) && state.composerConfigRevision === null;
  }
  if (state.composerConfigRevision !== null && revision < state.composerConfigRevision) {
    return false;
  }
  if (state.composerConfigRevision === null || revision > state.composerConfigRevision) {
    state.composerConfigRevision = revision;
    explicitStateGeneration += 1;
    recomputeComposerAvailability();
  }
  return true;
}

/**
 * Start a connection-scoped revision handshake. A reconnect to the same
 * backend keeps the current revision; only the first model payload may
 * establish a lower baseline, which identifies a newly started process.
 */
export function beginComposerRevisionHandshake(): void {
  availabilityRequestGeneration += 1;
  composerConnectionGeneration += 1;
  composerRevisionHandshakePending = true;
  state.composerSessionIdentityPending = true;
  recomputeComposerAvailability();
  void refreshComposerAvailability();
}

export function getComposerConnectionGeneration(): number {
  return composerConnectionGeneration;
}

export function acceptComposerSocketModelPayloadRevision(value: unknown): boolean {
  const revision = normalizeConfigRevision(value);
  const previousRevision = state.composerConfigRevision;
  const completesHandshake =
    composerRevisionHandshakePending &&
    (revision !== null || (configRevisionIsMissing(value) && previousRevision === null));
  if (composerRevisionHandshakePending && !completesHandshake) return false;
  if (completesHandshake) composerRevisionHandshakePending = false;
  if (
    completesHandshake &&
    revision !== null &&
    state.composerConfigRevision !== null &&
    revision < state.composerConfigRevision
  ) {
    state.composerConfigRevision = null;
    state.composerSessionModelRevision = null;
    state.composerGroupModelRevision = null;
    explicitStateGeneration += 1;
  }
  const accepted = acceptComposerConfigRevision(value);
  if (
    accepted &&
    (completesHandshake ||
      (revision !== null && (previousRevision === null || revision > previousRevision)))
  ) {
    void refreshComposerAvailability();
  }
  return accepted;
}

/** Accept model status from an HTTP Group detail without consuming the socket handshake. */
export function acceptComposerHttpModelPayloadRevision(value: unknown): boolean {
  const revision = normalizeConfigRevision(value);
  const previousRevision = state.composerConfigRevision;
  const accepted = acceptComposerConfigRevision(value);
  if (accepted && revision !== null && (previousRevision === null || revision > previousRevision)) {
    void refreshComposerAvailability();
  }
  return accepted;
}

export function resolveComposerModelAvailability(
  config: AppConfig,
  hasExplicitPrimaryModel = false,
  hasSessionModel = false,
  configuredModelsAvailable?: boolean,
): ComposerModelAvailability {
  if (hasExplicitPrimaryModel || hasSessionModel) return 'ready';

  const hasConfiguredModel =
    configuredModelsAvailable ??
    Object.values(config.models?.providers || {}).some((provider) =>
      (provider.models || []).some((model) => model.id.trim().length > 0),
    );
  return hasConfiguredModel ? 'agent-model-unconfigured' : 'models-unconfigured';
}

function placeholderKey(): string {
  switch (state.composerModelAvailability) {
    case 'checking':
      return 'composer.checkingModel';
    case 'models-unconfigured':
      return 'composer.modelsUnconfigured';
    case 'agent-model-unconfigured':
      return 'composer.agentModelUnconfigured';
    case 'session-model-unconfigured':
      return 'composer.sessionModelUnconfigured';
    case 'config-unavailable':
      return 'composer.configUnavailable';
    case 'ready':
      return state.busy ? 'composer.placeholderBusy' : 'composer.placeholder';
  }
}

export function isComposerModelReady(): boolean {
  return state.composerModelAvailability === 'ready';
}

function currentGroupTargets(value: string): string[] {
  return state.groupTargetMode === 'selected'
    ? state.groupSelectedTargets.filter((target) => state.activeGroupMembers.includes(target))
    : state.groupTargetMode === 'mentions'
      ? mentionedGroupTargets(value, state.activeGroupMembers)
      : state.activeGroupMembers;
}

function targetedSwitchCommand(value: string): boolean {
  const trimmed = value.trim();
  const command = trimmed.split(/\s+/, 1)[0].toLowerCase();
  return command === '/switch' && trimmed.slice(command.length).trim().length > 0;
}

function missingGroupModelTargets(value: string): string[] {
  if (!state.activeGroupId || !modelRevisionIsCurrent(state.composerGroupModelRevision)) return [];
  return currentGroupTargets(value).filter(
    (target) => !state.groupModelConfiguredMembers.has(target),
  );
}

export function areGroupMessageTargetsModelReady(value: string): boolean {
  if (!state.activeGroupId) return isComposerModelReady();
  if (!modelRevisionIsCurrent(state.composerGroupModelRevision)) return false;
  const targets = currentGroupTargets(value);
  return (
    targets.length > 0 && targets.every((target) => state.groupModelConfiguredMembers.has(target))
  );
}

export function canBypassComposerModelGate(value: string): boolean {
  if (state.pendingImages.length > 0) return false;
  const trimmed = value.trim();
  if (!trimmed.startsWith('/')) return false;
  if (state.activeGroupId) return false;
  const command = trimmed.split(/\s+/, 1)[0].toLowerCase();
  if (command === '/switch') {
    if (
      state.composerSessionTransitionPending ||
      state.sessionSwitchInFlight ||
      state.sessionIdentityMutationInFlight
    ) {
      return false;
    }
    const target = trimmed.slice(command.length).trim();
    if (target && (state.composerSessionIdentityPending || state.imageUploadInFlight)) return false;
  }
  return command !== '/new';
}

export function syncComposerAvailability(): void {
  const ready = isComposerModelReady();
  const inputValue = dom.input?.value || '';
  const groupTargetsMissing = Boolean(
    state.activeGroupId && currentGroupTargets(inputValue).length === 0,
  );
  const groupReady = areGroupMessageTargetsModelReady(inputValue);
  const missingGroupTargets = missingGroupModelTargets(inputValue);
  const groupSlashUnsupported = Boolean(
    state.activeGroupId && inputValue.trimStart().startsWith('/'),
  );
  const modelFreeSlashCommand = canBypassComposerModelGate(inputValue);
  const uploadBlocksSubmission = state.imageUploadInFlight && !modelFreeSlashCommand;
  const identityChangeBlocked =
    state.sessionSwitchInFlight || state.sessionIdentityMutationInFlight;
  const targetedSwitchBlocked = Boolean(
    targetedSwitchCommand(inputValue) &&
    (state.composerSessionIdentityPending || state.imageUploadInFlight || identityChangeBlocked),
  );
  const key = groupSlashUnsupported
    ? 'group.slashUnsupported'
    : uploadBlocksSubmission
      ? 'composer.uploadInProgress'
      : identityChangeBlocked
        ? 'composer.sessionChangeInProgress'
        : groupTargetsMissing
          ? 'group.selectMember'
          : state.activeGroupId && groupReady
            ? state.busy
              ? 'composer.placeholderBusy'
              : 'composer.placeholder'
            : missingGroupTargets.length > 0
              ? 'composer.groupTargetsUnconfigured'
              : placeholderKey();
  const vars =
    missingGroupTargets.length > 0
      ? {
          targets: missingGroupTargets
            .map(
              (target) =>
                state.activeGroupMemberDetails.find((member) => member.id === target)?.name ||
                target,
            )
            .join(', '),
        }
      : undefined;
  const canSubmit =
    !groupSlashUnsupported &&
    !uploadBlocksSubmission &&
    !identityChangeBlocked &&
    !targetedSwitchBlocked &&
    (groupReady || modelFreeSlashCommand);
  if (dom.input) {
    dom.input.dataset.i18nPlaceholder = key;
    dom.input.placeholder = tr(key, vars);
  }
  if (dom.sendBtn) {
    dom.sendBtn.disabled = !canSubmit;
    dom.sendBtn.title = canSubmit ? '' : tr(key, vars);
  }
  if (dom.composerAvailabilityStatus) {
    dom.composerAvailabilityStatus.hidden = canSubmit;
    const message = document.getElementById('composer-availability-message');
    if (message) {
      message.textContent = canSubmit ? '' : tr(key, vars);
    }
  }
  if (dom.composerAvailabilityRetry) {
    dom.composerAvailabilityRetry.hidden =
      canSubmit || state.composerModelAvailability !== 'config-unavailable';
  }
  const attachmentChangesBlocked = Boolean(
    state.sessionSwitchInFlight ||
    state.sessionIdentityMutationInFlight ||
    state.composerSessionTransitionPending ||
    state.composerSessionIdentityPending ||
    state.imageUploadInFlight,
  );
  if (dom.attachBtn) {
    dom.attachBtn.disabled = attachmentChangesBlocked;
    dom.attachBtn.setAttribute('aria-disabled', String(attachmentChangesBlocked));
  }
  if (
    (state.sessionSwitchInFlight ||
      state.sessionIdentityMutationInFlight ||
      state.composerSessionTransitionPending ||
      state.composerSessionIdentityPending) &&
    dom.attachPopup
  ) {
    dom.attachPopup.style.display = 'none';
  }
  document
    .querySelectorAll<HTMLButtonElement>('.image-preview-item .remove-btn')
    .forEach((button) => {
      button.disabled = attachmentChangesBlocked;
    });
  document.querySelectorAll<HTMLButtonElement>('.plan-execute-btn').forEach((button) => {
    const executing = state.pendingPlanExecutionId === button.dataset.planId;
    const canExecute =
      ready &&
      !state.imageUploadInFlight &&
      !state.sessionSwitchInFlight &&
      !state.sessionIdentityMutationInFlight &&
      !state.composerSessionTransitionPending &&
      !state.composerSessionIdentityPending;
    button.disabled = !canExecute || executing;
    button.title = canExecute ? '' : tr(key);
    if (canExecute) button.removeAttribute('aria-describedby');
    else button.setAttribute('aria-describedby', 'composer-availability-status');
  });
}

function recomputeComposerAvailability(): void {
  if (composerRevisionHandshakePending || state.composerSessionTransitionPending) {
    state.composerModelAvailability = 'checking';
  } else if (state.activeGroupId) {
    if (!modelRevisionIsCurrent(state.composerGroupModelRevision)) {
      state.composerModelAvailability = 'checking';
    } else if (state.composerExplicitPrimaryModelConfigured) {
      state.composerModelAvailability = 'ready';
    } else if (configLoadState === 'pending') {
      state.composerModelAvailability = 'checking';
    } else if (configLoadState === 'unavailable') {
      state.composerModelAvailability = 'config-unavailable';
    } else {
      state.composerModelAvailability = resolveComposerModelAvailability(
        lastComposerConfig,
        false,
        false,
        lastConfiguredModelsAvailable,
      );
    }
  } else if (!modelRevisionIsCurrent(state.composerSessionModelRevision)) {
    state.composerModelAvailability = 'checking';
  } else if (state.composerEffectiveModelConfigured === true) {
    state.composerModelAvailability = 'ready';
  } else if (state.composerEffectiveModelConfigured === null) {
    state.composerModelAvailability = 'checking';
  } else if (state.composerSessionModelOverridePresent) {
    state.composerModelAvailability = 'session-model-unconfigured';
  } else if (configLoadState === 'pending') {
    state.composerModelAvailability = 'checking';
  } else if (configLoadState === 'unavailable') {
    state.composerModelAvailability = 'config-unavailable';
  } else {
    state.composerModelAvailability = resolveComposerModelAvailability(
      lastComposerConfig,
      false,
      false,
      lastConfiguredModelsAvailable,
    );
  }
  syncComposerAvailability();
}

export function applyComposerConfig(
  config: AppConfig,
  configuredModelsAvailable?: boolean,
  configRevision?: unknown,
): boolean {
  if (!acceptComposerConfigRevision(configRevision)) return false;
  lastComposerConfig = config;
  lastConfiguredModelsAvailable = configuredModelsAvailable;
  configLoadState = 'loaded';
  recomputeComposerAvailability();
  return true;
}

export async function refreshComposerAvailability(staleRetriesRemaining = 1): Promise<void> {
  const requestGeneration = ++availabilityRequestGeneration;
  const explicitGenerationAtRequest = explicitStateGeneration;
  if (document.activeElement === dom.composerAvailabilityRetry) {
    dom.input?.focus();
  }
  configLoadState = 'pending';
  recomputeComposerAvailability();
  try {
    const response = await fetch('/api/config', { cache: 'no-store' });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    const data: ConfigApiResponse = await response.json();
    if (requestGeneration !== availabilityRequestGeneration) return;
    if (explicitGenerationAtRequest !== explicitStateGeneration) {
      if (staleRetriesRemaining > 0) {
        void refreshComposerAvailability(staleRetriesRemaining - 1);
      } else {
        configLoadState = 'unavailable';
        recomputeComposerAvailability();
      }
      return;
    }
    if (!acceptComposerConfigRevision(data.configRevision)) {
      if (!composerRevisionHandshakePending) {
        if (staleRetriesRemaining > 0) {
          void refreshComposerAvailability(staleRetriesRemaining - 1);
        } else {
          configLoadState = 'unavailable';
          recomputeComposerAvailability();
        }
      }
      return;
    }
    if (typeof data.explicitPrimaryModelConfigured === 'boolean') {
      setComposerExplicitPrimaryModelConfigured(
        data.explicitPrimaryModelConfigured,
        data.configRevision,
      );
    }
    if (data.parse_error || data.error) {
      configLoadState = 'unavailable';
      recomputeComposerAvailability();
      return;
    }
    applyComposerConfig(data.config || {}, data.configuredModelsAvailable, data.configRevision);
  } catch {
    if (requestGeneration !== availabilityRequestGeneration) return;
    if (explicitGenerationAtRequest !== explicitStateGeneration) {
      if (staleRetriesRemaining > 0) {
        void refreshComposerAvailability(staleRetriesRemaining - 1);
      } else {
        configLoadState = 'unavailable';
        recomputeComposerAvailability();
      }
      return;
    }
    configLoadState = 'unavailable';
    recomputeComposerAvailability();
  }
}

export function handleComposerConfigSaved(event: Event): void {
  const detail = (
    event as CustomEvent<{
      config?: AppConfig;
      explicitPrimaryModelConfigured?: boolean;
      configuredModelsAvailable?: boolean;
      configRevision?: number;
    }>
  ).detail;
  if (!acceptComposerConfigRevision(detail?.configRevision)) return;
  availabilityRequestGeneration += 1;
  if (typeof detail?.explicitPrimaryModelConfigured === 'boolean') {
    setComposerExplicitPrimaryModelConfigured(
      detail.explicitPrimaryModelConfigured,
      detail.configRevision,
    );
  }
  const config = detail?.config;
  if (config) {
    applyComposerConfig(config, detail?.configuredModelsAvailable, detail?.configRevision);
  } else void refreshComposerAvailability();
}

export function setComposerSessionModelConfigured(
  modelOverridePresent: boolean,
  modelOverrideConfigured: boolean,
  effectiveModelConfigured?: boolean,
  configRevision?: unknown,
  completeTransition = true,
): boolean {
  if (!acceptComposerConfigRevision(configRevision)) return false;
  state.composerSessionModelOverridePresent = modelOverridePresent;
  state.composerEffectiveModelConfigured =
    typeof effectiveModelConfigured === 'boolean'
      ? effectiveModelConfigured
      : modelOverridePresent
        ? modelOverrideConfigured
        : modelOverrideConfigured || state.composerExplicitPrimaryModelConfigured;
  state.composerSessionModelRevision = normalizeConfigRevision(configRevision);
  if (completeTransition) {
    completeComposerSessionTransition();
    return true;
  }
  recomputeComposerAvailability();
  return true;
}

export function setComposerExplicitPrimaryModelConfigured(
  configured: boolean,
  configRevision?: unknown,
): boolean {
  if (!acceptComposerConfigRevision(configRevision)) return false;
  if (
    normalizeConfigRevision(configRevision) === null ||
    configured !== state.composerExplicitPrimaryModelConfigured
  ) {
    explicitStateGeneration += 1;
  }
  state.composerExplicitPrimaryModelConfigured = configured;
  recomputeComposerAvailability();
  return true;
}

export function beginComposerSessionTransition(
  restoreOnCommandResult = false,
  expectedSessionId = '',
): void {
  restorableSessionTransition = restoreOnCommandResult
    ? {
        modelOverridePresent: state.composerSessionModelOverridePresent,
        effectiveModelConfigured: state.composerEffectiveModelConfigured,
        modelRevision: state.composerSessionModelRevision,
        sourceSessionId: state.activeSessionId,
        imageCapable: state.imageCapable,
        s3Capable: state.s3Capable,
        s3ConfigId: state.s3ConfigId,
        pendingImages: clonePendingImages(),
        targetCapabilitiesApplied: false,
      }
    : null;
  state.composerSessionModelOverridePresent = false;
  state.composerEffectiveModelConfigured = null;
  state.composerSessionModelRevision = null;
  state.composerSessionTransitionPending = true;
  // A slash /switch reuses the current socket, so its source Session payloads
  // must be filtered until the requested target arrives. UI-driven switches
  // create a new socket whose first Session payload is authoritative, including
  // a server fallback when the requested saved Session disappeared or is corrupt.
  state.composerSessionTransitionTarget = restoreOnCommandResult
    ? String(expectedSessionId || '').trim()
    : '';
  recomputeComposerAvailability();
}

export function completeComposerSessionTransition(): void {
  state.composerSessionTransitionPending = false;
  state.composerSessionTransitionTarget = '';
  restorableSessionTransition = null;
  recomputeComposerAvailability();
}

export function composerSessionPayloadMatchesTransition(sessionId: unknown): boolean {
  if (!state.composerSessionTransitionPending || !state.composerSessionTransitionTarget) {
    return true;
  }
  const session = normalizedSessionId(sessionId);
  const target = normalizedSessionId(state.composerSessionTransitionTarget);
  if (sessionIdsMatchExactly(session, target)) return true;
  if (!sessionMatchesTransitionTarget(session, target)) return false;
  const source = normalizedSessionId(restorableSessionTransition?.sourceSessionId);
  return target.length > 0 && !sessionIdsMatchExactly(session, source);
}

function normalizedSessionId(value: unknown): string {
  return String(value || '').trim();
}

function knownSessionIds(): string[] {
  return [
    ...new Set(state.sessions.map((session) => normalizedSessionId(session.id)).filter(Boolean)),
  ];
}

export function sessionIdsMatchExactly(left: unknown, right: unknown): boolean {
  const leftId = normalizedSessionId(left);
  const rightId = normalizedSessionId(right);
  return leftId.length > 0 && leftId === rightId;
}

function sessionMatchesTransitionTarget(sessionId: string, target: string): boolean {
  if (!sessionId || !target) return false;
  if (sessionId.startsWith(target)) return true;

  // On Windows a full-id case variant can resolve to the existing file. On
  // case-sensitive platforms the server can instead create the exact target,
  // which is handled above. Only accept a known folded alias when it is unique,
  // so Linux sessions such as `Foo` and `foo` never collapse into one another.
  const foldedTarget = target.toLowerCase();
  const aliases = knownSessionIds().filter((knownId) => knownId.toLowerCase() === foldedTarget);
  return aliases.length === 1 && aliases[0] === sessionId;
}

export function updateComposerSessionTransitionFallback(
  sessionId: unknown,
  modelOverridePresent: boolean,
  modelOverrideConfigured: boolean,
  effectiveModelConfigured: boolean | undefined,
  explicitPrimaryModelConfigured: boolean | undefined,
  configRevision?: unknown,
): boolean {
  if (
    !restorableSessionTransition ||
    !sessionIdsMatchExactly(sessionId, restorableSessionTransition.sourceSessionId) ||
    !acceptComposerSocketModelPayloadRevision(configRevision)
  ) {
    return false;
  }
  if (typeof explicitPrimaryModelConfigured === 'boolean') {
    setComposerExplicitPrimaryModelConfigured(explicitPrimaryModelConfigured, configRevision);
  }
  restorableSessionTransition = {
    ...restorableSessionTransition,
    modelOverridePresent,
    effectiveModelConfigured:
      typeof effectiveModelConfigured === 'boolean'
        ? effectiveModelConfigured
        : modelOverridePresent
          ? modelOverrideConfigured
          : modelOverrideConfigured || state.composerExplicitPrimaryModelConfigured,
    modelRevision: normalizeConfigRevision(configRevision),
    sourceSessionId: String(sessionId || '').trim(),
  };
  return true;
}

export function captureComposerSessionTransitionFallbackCapabilities(): void {
  if (!restorableSessionTransition) return;
  restorableSessionTransition = {
    ...restorableSessionTransition,
    imageCapable: state.imageCapable,
    s3Capable: state.s3Capable,
    s3ConfigId: state.s3ConfigId,
    pendingImages: clonePendingImages(),
    targetCapabilitiesApplied: false,
  };
}

export function updateComposerSessionTransitionFallbackCapabilities(
  imageCapable: boolean | undefined,
  s3Capable: boolean | undefined,
  s3ConfigId?: string,
): boolean {
  if (!restorableSessionTransition) return false;
  const nextImageCapable =
    typeof imageCapable === 'boolean' ? imageCapable : restorableSessionTransition.imageCapable;
  const nextS3Capable =
    typeof s3Capable === 'boolean' ? s3Capable : restorableSessionTransition.s3Capable;
  const nextS3ConfigId =
    typeof s3ConfigId === 'string' ? s3ConfigId : restorableSessionTransition.s3ConfigId;
  let pendingImages = restorableSessionTransition.pendingImages.map((image) => ({ ...image }));
  if (!nextImageCapable) {
    pendingImages = [];
  } else if (!nextS3Capable) {
    pendingImages = pendingImages.filter((image) => !(image.object_key || image.attachment_token));
  } else if (nextS3ConfigId !== restorableSessionTransition.s3ConfigId) {
    pendingImages = pendingImages.filter((image) => !(image.object_key || image.attachment_token));
  }
  restorableSessionTransition = {
    ...restorableSessionTransition,
    imageCapable: nextImageCapable,
    s3Capable: nextS3Capable,
    s3ConfigId: nextS3ConfigId,
    pendingImages,
  };
  return restorableSessionTransition.targetCapabilitiesApplied;
}

export function captureComposerSessionTransitionTargetCapabilitiesBaseline(): void {
  if (!restorableSessionTransition || restorableSessionTransition.targetCapabilitiesApplied) {
    return;
  }
  restorableSessionTransition = {
    ...restorableSessionTransition,
    imageCapable: state.imageCapable,
    s3Capable: state.s3Capable,
    s3ConfigId: state.s3ConfigId,
    pendingImages: clonePendingImages(),
    targetCapabilitiesApplied: true,
  };
}

export function restoreComposerSessionTransition(): boolean {
  if (!restorableSessionTransition) return false;
  state.composerSessionModelOverridePresent = restorableSessionTransition.modelOverridePresent;
  state.composerEffectiveModelConfigured = restorableSessionTransition.effectiveModelConfigured;
  state.composerSessionModelRevision = restorableSessionTransition.modelRevision;
  state.imageCapable = restorableSessionTransition.imageCapable;
  state.s3Capable = restorableSessionTransition.s3Capable;
  state.s3ConfigId = restorableSessionTransition.s3ConfigId;
  state.pendingImages = restorableSessionTransition.pendingImages.map((image) => ({ ...image }));
  state.composerSessionTransitionPending = false;
  state.composerSessionTransitionTarget = '';
  restorableSessionTransition = null;
  recomputeComposerAvailability();
  return true;
}

export function resetComposerGroupModelConfiguration(): void {
  state.groupModelConfiguredMembers = new Set();
  state.composerGroupModelRevision = null;
  recomputeComposerAvailability();
}

export function setGroupModelConfiguredMembers(
  members: unknown,
  configRevision?: unknown,
): boolean {
  if (!acceptComposerConfigRevision(configRevision)) return false;
  state.groupModelConfiguredMembers = new Set(
    Array.isArray(members)
      ? members.map((member) => String(member).trim()).filter((member) => member.length > 0)
      : [],
  );
  state.composerGroupModelRevision = normalizeConfigRevision(configRevision);
  if (state.activeGroupId) {
    state.composerSessionTransitionPending = false;
    state.composerSessionTransitionTarget = '';
    restorableSessionTransition = null;
  }
  recomputeComposerAvailability();
  return true;
}

export function groupModelRosterMatches(members: unknown[]): boolean {
  const payloadMembers = new Set(
    members.map((member) => String(member).trim()).filter((member) => member.length > 0),
  );
  const activeMembers = new Set(state.activeGroupMembers);
  return (
    payloadMembers.size === activeMembers.size &&
    [...payloadMembers].every((member) => activeMembers.has(member))
  );
}
