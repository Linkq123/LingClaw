import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';

import { iconHref } from '../icons.js';
import { isChinese, subscribeLanguageChange, tr } from '../i18n.js';
import { validateModelsConfigDraftShape, validateProviderName } from '../settingsValidation.js';
import { THINKING_EFFORT_LEVELS } from '../types/config.js';
import type { AppConfig, ModelEffortConfig, ThinkingEffort } from '../types/config.js';
import { trapDialogFocus } from './dialogFocus.js';
import {
  buildProviderForms,
  createModelFormEntry,
  createProviderForm,
  normalizeModelsConfig,
  serializeProviderForms,
} from './settingsModels.js';
import type { ModelFormEntry, ProviderFormData } from './settingsModels.js';

import '../css/models-console.css';

export interface ModelsConsoleProps {
  config: AppConfig;
  onChange: (config: AppConfig) => void;
  onStatus: (message: string, type?: string) => void;
  /**
   * Incremented by the settings shell whenever the server-backed configuration
   * baseline is accepted (load, reload, save, or discard). Local editor state
   * is rebuilt on that boundary so a completed save cannot leave a stale form
   * or Raw JSON conflict behind.
   */
  baselineRevision?: number;
  /** Reports drafts that have not reached `config` yet (Raw JSON or a blank model card). */
  onDraftDirtyChange?: (dirty: boolean) => void;
}

type CapabilityFilter = 'text' | 'image' | 'reasoning';
type DeleteIntent =
  | { kind: 'provider'; providerKey: string }
  | { kind: 'model'; providerKey: string; modelKey: string }
  | null;

const THINKING_FORMAT_OPTIONS = [
  'openai',
  'qwen',
  'doubao',
  'deepseek-v4',
  'ollama',
  'gpt-oss',
  'ollama-gpt-oss',
] as const;

const API_OPTIONS: ReadonlyArray<{ value: string; label: string }> = [
  { value: 'openai-completions', label: 'OpenAI Completions' },
  { value: 'openai-responses', label: 'OpenAI Responses' },
  { value: 'anthropic', label: 'Anthropic' },
  { value: 'ollama', label: 'Ollama' },
  { value: 'gemini', label: 'Gemini' },
];

const COPY = {
  en: {
    catalog: 'Model catalog',
    catalogHint: 'Search configured models and edit their runtime capabilities.',
    search: 'Search models or providers',
    allProviders: 'All providers',
    filters: 'Capability filters',
    modelsConfigured: '{count} configured models',
    providerConnection: 'Provider connection',
    providerConnectionHint: 'Endpoint and authentication used by every model in this provider.',
    provider: 'Provider',
    noProviders: 'No providers configured',
    noProvidersHint: 'Add a provider to connect LingClaw to a model endpoint.',
    noMatches: 'No models match these filters',
    noMatchesHint: 'Clear the search or remove a capability filter.',
    clearFilters: 'Clear filters',
    addProvider: 'Add provider',
    addProviderTitle: 'Create a model provider',
    addProviderHint: "Use a short identifier such as 'openai' or 'local-ollama'.",
    providerName: 'Provider name',
    createProvider: 'Create provider',
    addModel: 'Add model',
    untitledModel: 'Untitled model',
    editModel: 'Model details',
    editModelHint: 'Runtime identity, limits, and supported inputs.',
    displayName: 'Display name',
    capabilities: 'Capabilities',
    connectionTest: 'Connection test',
    testConnection: 'Test connection',
    deleteProviderQuestion: 'Delete this provider and all of its configured models?',
    deleteModelQuestion: 'Remove this model from the provider?',
    deleteCannotUndo:
      'This change is added to the settings draft and can be reverted before saving.',
    confirmDelete: 'Confirm delete',
    syncJson: 'Refresh JSON from form',
    rawConflict: 'The visual form has newer changes. Refresh the JSON draft before applying it.',
    jsonDraft: 'Raw Models JSON',
    jsonHint: 'Advanced editing preserves supported and unknown provider metadata.',
    appliedJson: 'Applied Models JSON',
    jsonRefreshed: 'Raw JSON refreshed from the visual form.',
    selectedModel: 'Selected model',
    selectModelHint: 'Select a card to inspect or edit its configuration.',
    customThinkingFormat: 'Custom values are supported.',
    thinkingEffort: 'Thinking effort',
    effortHint: 'Choose the efforts this model accepts and the Session default.',
    defaultEffort: 'Default effort',
    effortSummary: '{default} · {count} levels',
    externalConflict:
      'Model configuration changed outside this editor. Resolve the current draft before reloading.',
    modelCount: '{count} models',
    apiKeyHint: 'Stored only in your local LingClaw configuration.',
    showApiKey: 'Show API key',
    hideApiKey: 'Hide API key',
    selectModelForTest: 'Select a model before testing the connection.',
    connectionFailed: 'Connection test failed.',
  },
  zh: {
    catalog: '模型目录',
    catalogHint: '搜索已配置模型，并编辑运行能力。',
    search: '搜索模型或服务商',
    allProviders: '全部服务商',
    filters: '能力筛选',
    modelsConfigured: '已配置 {count} 个模型',
    providerConnection: '服务商连接',
    providerConnectionHint: '该服务商下所有模型共用的端点与身份验证。',
    provider: '服务商',
    noProviders: '尚未配置服务商',
    noProvidersHint: '添加服务商，将 LingClaw 连接到模型端点。',
    noMatches: '没有符合筛选条件的模型',
    noMatchesHint: '清空搜索或移除能力筛选后重试。',
    clearFilters: '清除筛选',
    addProvider: '添加服务商',
    addProviderTitle: '创建模型服务商',
    addProviderHint: '使用简短标识，例如“openai”或“local-ollama”。',
    providerName: '服务商名称',
    createProvider: '创建服务商',
    addModel: '添加模型',
    untitledModel: '未命名模型',
    editModel: '模型详情',
    editModelHint: '运行标识、限制与支持的输入类型。',
    displayName: '显示名称',
    capabilities: '模型能力',
    connectionTest: '连接测试',
    testConnection: '测试连接',
    deleteProviderQuestion: '删除此服务商及其配置的全部模型？',
    deleteModelQuestion: '从服务商中移除此模型？',
    deleteCannotUndo: '此操作会加入设置草稿，保存前仍可放弃更改。',
    confirmDelete: '确认删除',
    syncJson: '从表单刷新 JSON',
    rawConflict: '可视化表单包含更新的改动。应用 JSON 前请先刷新 JSON 草稿。',
    jsonDraft: 'Models 原始 JSON',
    jsonHint: '高级编辑会保留受支持字段和未知的服务商元数据。',
    appliedJson: 'Models JSON 已应用',
    jsonRefreshed: '已从可视化表单刷新原始 JSON。',
    selectedModel: '已选模型',
    selectModelHint: '选择模型卡片以检查或编辑配置。',
    customThinkingFormat: '支持输入自定义值。',
    thinkingEffort: '思考强度',
    effortHint: '选择此模型允许的强度，并指定 Session 默认值。',
    defaultEffort: '默认强度',
    effortSummary: '{default} · {count} 档',
    externalConflict: '模型配置已在此编辑器之外发生变化，请先处理当前草稿再重新加载。',
    modelCount: '{count} 个模型',
    apiKeyHint: '仅保存在本机 LingClaw 配置中。',
    showApiKey: '显示 API 密钥',
    hideApiKey: '隐藏 API 密钥',
    selectModelForTest: '请先选择一个模型，再测试连接。',
    connectionFailed: '连接测试失败。',
  },
} as const;

type CopyKey = keyof (typeof COPY)['en'];

function useCopy(): (key: CopyKey, vars?: Record<string, string | number>) => string {
  const [, setLanguageVersion] = useState(0);
  useEffect(() => subscribeLanguageChange(() => setLanguageVersion((current) => current + 1)), []);
  return useCallback((key: CopyKey, vars?: Record<string, string | number>) => {
    const translationKey = `modelsConsole.${key}`;
    const translated = tr(translationKey, vars);
    if (translated !== translationKey) return translated;
    let value: string = COPY[isChinese() ? 'zh' : 'en'][key];
    for (const [name, replacement] of Object.entries(vars || {})) {
      value = value.replaceAll(`{${name}}`, String(replacement));
    }
    return value;
  }, []);
}

function modelsSignature(models: AppConfig['models']): string {
  return JSON.stringify(normalizeModelsConfig(models) || null);
}

function providersSignature(providers: ProviderFormData[]): string {
  return modelsSignature(serializeProviderForms(providers));
}

function parseOptionalInteger(value: string): number | undefined {
  const trimmed = value.trim();
  if (!trimmed) return undefined;
  const parsed = Number.parseInt(trimmed, 10);
  return Number.isFinite(parsed) ? parsed : undefined;
}

function plainRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? { ...(value as Record<string, unknown>) }
    : {};
}

function thinkingFormat(model: ModelFormEntry): string {
  const value = plainRecord(model.compat).thinkingFormat;
  return typeof value === 'string' ? value : '';
}

function withThinkingFormat(model: ModelFormEntry, value: string): ModelFormEntry {
  const compat = plainRecord(model.compat);
  const trimmed = value.trim();
  if (trimmed) compat.thinkingFormat = trimmed;
  else delete compat.thinkingFormat;
  return { ...model, compat: Object.keys(compat).length > 0 ? compat : undefined };
}

function effectiveModelEffort(model: ModelFormEntry): ModelEffortConfig {
  if (!model.reasoning) return { levels: ['off'], default: 'off' };
  const configured = model.effort;
  if (configured?.levels?.length) {
    const selected = new Set(configured.levels);
    const levels = THINKING_EFFORT_LEVELS.filter((level) => selected.has(level));
    if (levels.length > 0) {
      return {
        levels: [...levels],
        default: levels.includes(configured.default) ? configured.default : levels[0],
      };
    }
  }
  return { levels: [...THINKING_EFFORT_LEVELS], default: 'auto' };
}

function withModelEffort(
  model: ModelFormEntry,
  levels: ThinkingEffort[],
  defaultEffort: ThinkingEffort,
): ModelEffortConfig {
  return {
    ...plainRecord(model.effort),
    levels: [...levels],
    default: defaultEffort,
  } as ModelEffortConfig;
}

function effortLabel(effort: ThinkingEffort): string {
  const key = `composer.effort.${effort}`;
  const translated = tr(key);
  return translated === key ? effort : translated;
}

function providerInitial(name: string): string {
  const match = name.trim().match(/[A-Za-z0-9]/);
  return (match?.[0] || 'P').toUpperCase();
}

function providerApiLabel(api: string): string {
  return API_OPTIONS.find((option) => option.value === api)?.label || api || 'OpenAI Completions';
}

function hasBlankModelDraft(providers: ProviderFormData[]): boolean {
  return providers.some((provider) => provider.models.some((model) => !model.id.trim()));
}

function modelCapabilities(model: ModelFormEntry): Set<CapabilityFilter> {
  const capabilities = new Set<CapabilityFilter>();
  const input = Array.isArray(model.input) ? model.input : ['text'];
  if (input.includes('text')) capabilities.add('text');
  if (input.includes('image')) capabilities.add('image');
  if (model.reasoning) capabilities.add('reasoning');
  return capabilities;
}

function localizedTestLabel(provider: ProviderFormData): string {
  if (provider.testState === 'idle') return tr('settings.test');
  if (provider.testState === 'testing') return tr('settings.testing');
  if (provider.testState === 'fail') return tr('common.failed');
  return tr('settings.connected');
}

function Icon({
  name,
}: {
  name: 'check' | 'chevron-right' | 'close' | 'plus' | 'refresh' | 'search' | 'trash';
}) {
  return (
    <svg className="icon" aria-hidden="true" focusable="false">
      <use href={iconHref(name)} />
    </svg>
  );
}

export function ModelsConsole({
  config,
  onChange,
  onStatus,
  baselineRevision = 0,
  onDraftDirtyChange,
}: ModelsConsoleProps) {
  const copy = useCopy();
  const initialProviders = config.models?.providers || {};
  const [providers, setProviders] = useState<ProviderFormData[]>(() =>
    buildProviderForms(initialProviders),
  );
  const providersRef = useRef(providers);
  const localSignatureRef = useRef('');
  const formDirtyRef = useRef(false);
  const jsonDirtyRef = useRef(false);
  const reportedExternalSignatureRef = useRef('');
  const configRef = useRef(config);
  const onChangeRef = useRef(onChange);
  const onStatusRef = useRef(onStatus);
  const onDraftDirtyChangeRef = useRef(onDraftDirtyChange);
  const appliedBaselineRevisionRef = useRef(baselineRevision);
  const resetTimersRef = useRef(new Map<string, number>());
  const addProviderDialogRef = useRef<HTMLFormElement | null>(null);
  const previousAddProviderFocusRef = useRef<HTMLElement | null>(null);
  const inspectorRef = useRef<HTMLElement | null>(null);
  const modelCardRefs = useRef(new Map<string, HTMLButtonElement>());
  const providerTabRefs = useRef(new Map<string, HTMLButtonElement>());
  const allProvidersTabRef = useRef<HTMLButtonElement | null>(null);
  const addProviderButtonRef = useRef<HTMLButtonElement | null>(null);
  const addModelButtonRef = useRef<HTMLButtonElement | null>(null);
  const modelSearchInputRef = useRef<HTMLInputElement | null>(null);
  const selectedModelOriginRef = useRef<HTMLButtonElement | null>(null);
  const providerDeleteButtonRef = useRef<HTMLButtonElement | null>(null);
  const modelDeleteButtonRef = useRef<HTMLButtonElement | null>(null);
  const deleteConfirmButtonRef = useRef<HTMLButtonElement | null>(null);
  const deleteOriginRef = useRef<HTMLButtonElement | null>(null);
  const pendingDeleteFocusRestoreRef = useRef<NonNullable<DeleteIntent> | null>(null);
  const mobileCatalogScrollRef = useRef<{
    container: HTMLElement;
    top: number;
    left: number;
  } | null>(null);

  const [providerFilter, setProviderFilter] = useState('all');
  const [activeProviderKey, setActiveProviderKey] = useState(providers[0]?._key || '');
  const [selectedModelKey, setSelectedModelKey] = useState('');
  const [searchText, setSearchText] = useState('');
  const [capabilityFilters, setCapabilityFilters] = useState<Set<CapabilityFilter>>(
    () => new Set(),
  );
  const [addProviderOpen, setAddProviderOpen] = useState(false);
  const [newProviderName, setNewProviderName] = useState('');
  const [newProviderError, setNewProviderError] = useState('');
  const [deleteIntent, setDeleteIntent] = useState<DeleteIntent>(null);
  const [showApiKey, setShowApiKey] = useState(false);
  const [jsonText, setJsonText] = useState(() =>
    JSON.stringify(config.models || { providers: {} }, null, 2),
  );
  const [jsonError, setJsonError] = useState('');
  const [jsonDirty, setJsonDirty] = useState(false);
  const [formDirty, setFormDirty] = useState(false);

  const cancelDelete = useCallback((): void => {
    if (!deleteIntent) return;
    pendingDeleteFocusRestoreRef.current = deleteIntent;
    setDeleteIntent(null);
  }, [deleteIntent]);

  useEffect(() => {
    if (deleteIntent) {
      deleteConfirmButtonRef.current?.focus({ preventScroll: true });
      return;
    }

    const restoreIntent = pendingDeleteFocusRestoreRef.current;
    if (!restoreIntent) return;
    pendingDeleteFocusRestoreRef.current = null;
    const origin = deleteOriginRef.current;
    deleteOriginRef.current = null;
    const fallback =
      restoreIntent.kind === 'provider'
        ? providerDeleteButtonRef.current
        : modelDeleteButtonRef.current;
    (origin?.isConnected ? origin : fallback)?.focus({ preventScroll: true });
  }, [deleteIntent]);

  useEffect(() => {
    if (!deleteIntent) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return;
      event.preventDefault();
      event.stopPropagation();
      cancelDelete();
    };
    window.addEventListener('keydown', onKeyDown, true);
    return () => window.removeEventListener('keydown', onKeyDown, true);
  }, [cancelDelete, deleteIntent]);

  useEffect(() => {
    configRef.current = config;
  }, [config]);

  useEffect(() => {
    onChangeRef.current = onChange;
  }, [onChange]);

  useEffect(() => {
    onStatusRef.current = onStatus;
  }, [onStatus]);

  useEffect(() => {
    onDraftDirtyChangeRef.current = onDraftDirtyChange;
  }, [onDraftDirtyChange]);

  useEffect(() => {
    providersRef.current = providers;
  }, [providers]);

  useEffect(() => {
    formDirtyRef.current = formDirty;
  }, [formDirty]);

  useEffect(() => {
    jsonDirtyRef.current = jsonDirty;
  }, [jsonDirty]);

  const reportUncommittedDraft = useCallback(
    (nextProviders: ProviderFormData[], nextJsonDirty = jsonDirtyRef.current) => {
      onDraftDirtyChangeRef.current?.(nextJsonDirty || hasBlankModelDraft(nextProviders));
    },
    [],
  );

  const replaceProviders = useCallback(
    (next: ProviderFormData[], options: { markFormDirty?: boolean; local?: boolean } = {}) => {
      providersRef.current = next;
      if (options.local !== false) localSignatureRef.current = providersSignature(next);
      setProviders(next);
      reportUncommittedDraft(next);
      if (options.markFormDirty) {
        setFormDirty(true);
        formDirtyRef.current = true;
      }
    },
    [reportUncommittedDraft],
  );

  const mutateProviders = useCallback(
    (mutation: (current: ProviderFormData[]) => ProviderFormData[], markFormDirty = true) => {
      replaceProviders(mutation(providersRef.current), { markFormDirty, local: true });
    },
    [replaceProviders],
  );

  useEffect(() => {
    if (appliedBaselineRevisionRef.current === baselineRevision) return;
    appliedBaselineRevisionRef.current = baselineRevision;
    const next = buildProviderForms(config.models?.providers, providersRef.current);
    providersRef.current = next;
    localSignatureRef.current = '';
    reportedExternalSignatureRef.current = '';
    formDirtyRef.current = false;
    jsonDirtyRef.current = false;
    setProviders(next);
    setFormDirty(false);
    setJsonDirty(false);
    setJsonText(JSON.stringify(config.models || { providers: {} }, null, 2));
    setJsonError('');
    setDeleteIntent(null);
    setProviderFilter('all');
    setSelectedModelKey('');
    setActiveProviderKey(next[0]?._key || '');
    onDraftDirtyChangeRef.current?.(false);
  }, [baselineRevision, config.models]);

  useEffect(() => {
    const local = providersSignature(providers);
    const incoming = modelsSignature(config.models);
    if (local === incoming) {
      localSignatureRef.current = '';
      reportedExternalSignatureRef.current = '';
      return;
    }
    if (localSignatureRef.current === local) return;
    if (formDirtyRef.current || jsonDirtyRef.current) {
      if (reportedExternalSignatureRef.current !== incoming) {
        reportedExternalSignatureRef.current = incoming;
        onStatusRef.current(copy('externalConflict'), 'error');
      }
      return;
    }
    const next = buildProviderForms(config.models?.providers, providersRef.current);
    providersRef.current = next;
    setProviders(next);
    setJsonText(JSON.stringify(config.models || { providers: {} }, null, 2));
    setJsonError('');
    setProviderFilter('all');
    setSelectedModelKey('');
    setActiveProviderKey(next[0]?._key || '');
  }, [config.models, copy, providers]);

  useEffect(() => {
    const serialized = serializeProviderForms(providers);
    const local = modelsSignature(serialized);
    if (localSignatureRef.current !== local) return;
    if (local === modelsSignature(config.models)) {
      localSignatureRef.current = '';
      return;
    }
    onChangeRef.current({ ...configRef.current, models: serialized });
  }, [config.models, providers]);

  useEffect(
    () => () => {
      for (const timer of resetTimersRef.current.values()) window.clearTimeout(timer);
      resetTimersRef.current.clear();
      onDraftDirtyChangeRef.current?.(false);
    },
    [],
  );

  const focusModelInspector = useCallback(() => {
    const inspector = inspectorRef.current;
    const target = inspector?.querySelector<HTMLElement>(
      '.models-console-inspector-body input:not([type="hidden"]), .models-console-inspector-body select, .models-console-inspector-body textarea',
    );
    (target || inspector)?.focus({ preventScroll: true });
  }, []);

  const captureCatalogScroll = useCallback((origin: HTMLElement | null) => {
    const container = origin?.closest<HTMLElement>('.settings-body');
    mobileCatalogScrollRef.current = container
      ? { container, top: container.scrollTop, left: container.scrollLeft }
      : null;
  }, []);

  const captureMobileCatalogScroll = useCallback(
    (origin: HTMLElement | null) => {
      const mobileViewport = window.matchMedia?.('(max-width: 768px)');
      if (!mobileViewport?.matches) {
        mobileCatalogScrollRef.current = null;
        return;
      }
      captureCatalogScroll(origin);
    },
    [captureCatalogScroll],
  );

  const restoreMobileCatalogScroll = useCallback(() => {
    const snapshot = mobileCatalogScrollRef.current;
    mobileCatalogScrollRef.current = null;
    if (!snapshot?.container.isConnected) return;
    snapshot.container.scrollTop = snapshot.top;
    snapshot.container.scrollLeft = snapshot.left;
  }, []);

  useEffect(() => {
    if (!selectedModelKey) return;
    const mobileViewport = window.matchMedia?.('(max-width: 768px)');
    if (!mobileViewport) return;

    const syncResponsiveInspector = (mobile: boolean) => {
      if (!mobile) {
        restoreMobileCatalogScroll();
        return;
      }
      if (!mobileCatalogScrollRef.current) {
        captureCatalogScroll(selectedModelOriginRef.current);
      }
      inspectorRef.current?.scrollIntoView?.({ block: 'start', inline: 'nearest' });
      focusModelInspector();
    };
    const onViewportChange = (event: MediaQueryListEvent) => {
      syncResponsiveInspector(event.matches);
    };

    syncResponsiveInspector(mobileViewport.matches);
    if (typeof mobileViewport.addEventListener === 'function') {
      mobileViewport.addEventListener('change', onViewportChange);
      return () => mobileViewport.removeEventListener('change', onViewportChange);
    }
    mobileViewport.addListener?.(onViewportChange);
    return () => mobileViewport.removeListener?.(onViewportChange);
  }, [captureCatalogScroll, focusModelInspector, restoreMobileCatalogScroll, selectedModelKey]);

  const previousSelectedModelKeyRef = useRef(selectedModelKey);
  useEffect(() => {
    const previousSelectedModelKey = previousSelectedModelKeyRef.current;
    previousSelectedModelKeyRef.current = selectedModelKey;
    if (previousSelectedModelKey && !selectedModelKey) restoreMobileCatalogScroll();
  }, [restoreMobileCatalogScroll, selectedModelKey]);

  const closeInspector = useCallback(() => {
    const closingKey = selectedModelKey;
    const origin = selectedModelOriginRef.current;
    selectedModelOriginRef.current = null;
    setDeleteIntent(null);
    setSelectedModelKey('');
    window.setTimeout(() => {
      const sameCard = modelCardRefs.current.get(closingKey);
      const firstVisibleCard = Array.from(modelCardRefs.current.values()).find(
        (card) => card.isConnected,
      );
      const target =
        (origin?.isConnected ? origin : undefined) ||
        (sameCard?.isConnected ? sameCard : undefined) ||
        firstVisibleCard ||
        modelSearchInputRef.current ||
        addModelButtonRef.current;
      target?.focus({ preventScroll: true });
    }, 0);
  }, [selectedModelKey]);

  const openAddProvider = useCallback(
    (origin: HTMLElement) => {
      previousAddProviderFocusRef.current = origin;
      setAddProviderOpen(true);
    },
    [setAddProviderOpen],
  );

  useEffect(() => {
    if (!addProviderOpen) return;
    const close = (event: KeyboardEvent) => {
      if (event.key === 'Tab' && trapDialogFocus(event, addProviderDialogRef.current)) {
        event.stopPropagation();
        return;
      }
      if (event.key !== 'Escape') return;
      event.preventDefault();
      event.stopPropagation();
      setAddProviderOpen(false);
      setNewProviderError('');
    };
    window.addEventListener('keydown', close, true);
    return () => {
      window.removeEventListener('keydown', close, true);
      const previous = previousAddProviderFocusRef.current;
      previousAddProviderFocusRef.current = null;
      if (previous?.isConnected) previous.focus();
    };
  }, [addProviderOpen]);

  const updateProvider = useCallback(
    (providerKey: string, fields: Partial<ProviderFormData>) => {
      mutateProviders((current) =>
        current.map((provider) =>
          provider._key === providerKey ? { ...provider, ...fields } : provider,
        ),
      );
    },
    [mutateProviders],
  );

  const updateModel = useCallback(
    (providerKey: string, modelKey: string, fields: Partial<ModelFormEntry>) => {
      mutateProviders((current) =>
        current.map((provider) =>
          provider._key === providerKey
            ? {
                ...provider,
                models: provider.models.map((model) =>
                  model._key === modelKey ? { ...model, ...fields } : model,
                ),
              }
            : provider,
        ),
      );
    },
    [mutateProviders],
  );

  const flatModels = useMemo(
    () => providers.flatMap((provider) => provider.models.map((model) => ({ provider, model }))),
    [providers],
  );

  const selectedModel = useMemo(
    () => flatModels.find(({ model }) => model._key === selectedModelKey),
    [flatModels, selectedModelKey],
  );

  const connectionProvider = useMemo(() => {
    if (selectedModel) return selectedModel.provider;
    const active = providers.find((provider) => provider._key === activeProviderKey);
    if (active) return active;
    return providers[0];
  }, [activeProviderKey, providers, selectedModel]);

  useEffect(() => {
    setShowApiKey(false);
  }, [connectionProvider?._key]);

  const filteredModels = useMemo(() => {
    const query = searchText.trim().toLocaleLowerCase();
    return flatModels.filter(({ provider, model }) => {
      if (providerFilter !== 'all' && provider._key !== providerFilter) return false;
      const haystack = `${provider.name} ${model.id || ''} ${model.name || ''}`.toLocaleLowerCase();
      if (query && !haystack.includes(query)) return false;
      const capabilities = modelCapabilities(model);
      return [...capabilityFilters].every((capability) => capabilities.has(capability));
    });
  }, [capabilityFilters, flatModels, providerFilter, searchText]);

  const clearProviderReset = useCallback((providerKey: string) => {
    const timer = resetTimersRef.current.get(providerKey);
    if (timer !== undefined) window.clearTimeout(timer);
    resetTimersRef.current.delete(providerKey);
  }, []);

  const testModel = useCallback(
    async (provider: ProviderFormData, modelId: string) => {
      if (!modelId.trim()) {
        onStatusRef.current(copy('selectModelForTest'), 'error');
        return;
      }
      clearProviderReset(provider._key);
      const applyState = (testState: ProviderFormData['testState'], testLabel: string) => {
        mutateProviders(
          (current) =>
            current.map((candidate) =>
              candidate._key === provider._key
                ? { ...candidate, testState, testLabel, selectedTestModel: modelId }
                : candidate,
            ),
          false,
        );
      };
      applyState('testing', 'Testing...');
      try {
        const response = await fetch('/api/config/test-model', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({
            providerName: provider.name,
            baseUrl: provider.baseUrl,
            apiKey: provider.apiKey,
            api: provider.api || 'openai-completions',
            modelId,
          }),
        });
        const data = await response.json();
        if (response.ok && data.ok) {
          applyState('ok', 'Connected');
          onStatusRef.current(tr('settings.connected'), 'success');
        } else {
          applyState('fail', 'Failed');
          onStatusRef.current(data.error || copy('connectionFailed'), 'error');
        }
      } catch (error: unknown) {
        applyState('fail', 'Error');
        onStatusRef.current((error as Error).message, 'error');
      }
      const timer = window.setTimeout(() => {
        resetTimersRef.current.delete(provider._key);
        mutateProviders(
          (current) =>
            current.map((candidate) =>
              candidate._key === provider._key
                ? { ...candidate, testState: 'idle', testLabel: 'Test' }
                : candidate,
            ),
          false,
        );
      }, 4000);
      resetTimersRef.current.set(provider._key, timer);
    },
    [clearProviderReset, copy, mutateProviders],
  );

  const addModel = useCallback(() => {
    const provider = connectionProvider;
    if (!provider) return;
    captureMobileCatalogScroll(addModelButtonRef.current);
    const model = createModelFormEntry(provider.name, { id: '', input: ['text'] });
    mutateProviders((current) =>
      current.map((candidate) =>
        candidate._key === provider._key
          ? { ...candidate, models: [...candidate.models, model] }
          : candidate,
      ),
    );
    setActiveProviderKey(provider._key);
    setProviderFilter(provider._key);
    setSelectedModelKey(model._key);
    setDeleteIntent(null);
  }, [captureMobileCatalogScroll, connectionProvider, mutateProviders]);

  const createProvider = (event: React.FormEvent) => {
    event.preventDefault();
    try {
      const existing = Object.fromEntries(
        providersRef.current.map((provider) => [provider.name, true]),
      );
      const name = validateProviderName(newProviderName, existing);
      const provider = createProviderForm(name);
      replaceProviders([...providersRef.current, provider], { markFormDirty: true, local: true });
      setActiveProviderKey(provider._key);
      setProviderFilter(provider._key);
      setSelectedModelKey('');
      setNewProviderName('');
      setNewProviderError('');
      setAddProviderOpen(false);
    } catch (error: unknown) {
      setNewProviderError((error as Error).message);
    }
  };

  const requestDelete = (intent: NonNullable<DeleteIntent>, origin: HTMLButtonElement): void => {
    deleteOriginRef.current = origin;
    pendingDeleteFocusRestoreRef.current = null;
    setDeleteIntent(intent);
  };

  const confirmDelete = () => {
    if (!deleteIntent) return;
    deleteOriginRef.current = null;
    pendingDeleteFocusRestoreRef.current = null;
    let focusAfterDelete: (() => HTMLElement | null | undefined) | undefined;
    if (deleteIntent.kind === 'provider') {
      clearProviderReset(deleteIntent.providerKey);
      const currentIndex = providersRef.current.findIndex(
        (provider) => provider._key === deleteIntent.providerKey,
      );
      const next = providersRef.current.filter(
        (provider) => provider._key !== deleteIntent.providerKey,
      );
      const nextProviderKey =
        next[Math.min(Math.max(currentIndex, 0), Math.max(next.length - 1, 0))]?._key;
      replaceProviders(next, { markFormDirty: true, local: true });
      if (activeProviderKey === deleteIntent.providerKey) setActiveProviderKey(next[0]?._key || '');
      if (providerFilter === deleteIntent.providerKey) setProviderFilter('all');
      const selectedStillExists = next.some((provider) =>
        provider.models.some((model) => model._key === selectedModelKey),
      );
      if (!selectedStillExists) {
        selectedModelOriginRef.current = null;
        setSelectedModelKey('');
      }
      focusAfterDelete = () =>
        (nextProviderKey ? providerTabRefs.current.get(nextProviderKey) : undefined) ||
        allProvidersTabRef.current ||
        addProviderButtonRef.current;
    } else {
      const visibleModelKeys = filteredModels.map(({ model }) => model._key);
      const currentIndex = visibleModelKeys.indexOf(deleteIntent.modelKey);
      const remainingVisibleKeys = visibleModelKeys.filter(
        (modelKey) => modelKey !== deleteIntent.modelKey,
      );
      const nextModelKey =
        remainingVisibleKeys[
          Math.min(Math.max(currentIndex, 0), Math.max(remainingVisibleKeys.length - 1, 0))
        ];
      mutateProviders((current) =>
        current.map((provider) =>
          provider._key === deleteIntent.providerKey
            ? {
                ...provider,
                models: provider.models.filter((model) => model._key !== deleteIntent.modelKey),
              }
            : provider,
        ),
      );
      if (selectedModelKey === deleteIntent.modelKey) {
        selectedModelOriginRef.current = null;
        setSelectedModelKey('');
      }
      focusAfterDelete = () =>
        (nextModelKey ? modelCardRefs.current.get(nextModelKey) : undefined) ||
        addModelButtonRef.current;
    }
    setDeleteIntent(null);
    window.setTimeout(() => focusAfterDelete?.()?.focus(), 0);
  };

  const toggleCapabilityFilter = (filter: CapabilityFilter) => {
    setCapabilityFilters((current) => {
      const next = new Set(current);
      if (next.has(filter)) next.delete(filter);
      else next.add(filter);
      return next;
    });
  };

  const clearFilters = () => {
    setSearchText('');
    setProviderFilter('all');
    setCapabilityFilters(new Set());
  };

  const syncJsonFromForm = () => {
    const serialized = serializeProviderForms(providersRef.current);
    setJsonText(JSON.stringify(serialized || { providers: {} }, null, 2));
    setJsonError('');
    setJsonDirty(false);
    setFormDirty(false);
    jsonDirtyRef.current = false;
    formDirtyRef.current = false;
    reportUncommittedDraft(providersRef.current, false);
    onStatusRef.current(copy('jsonRefreshed'), 'success');
  };

  const applyJson = () => {
    if (formDirtyRef.current) {
      onStatusRef.current(copy('rawConflict'), 'error');
      return;
    }
    try {
      const parsed = JSON.parse(jsonText.trim() || '{}');
      validateModelsConfigDraftShape(parsed);
      const normalized = normalizeModelsConfig(parsed as AppConfig['models']);
      const next = buildProviderForms(normalized?.providers, providersRef.current);
      replaceProviders(next, { local: true });
      setJsonText(JSON.stringify(normalized || { providers: {} }, null, 2));
      setJsonError('');
      setJsonDirty(false);
      setFormDirty(false);
      jsonDirtyRef.current = false;
      formDirtyRef.current = false;
      reportUncommittedDraft(next, false);
      setProviderFilter('all');
      setActiveProviderKey(next[0]?._key || '');
      setSelectedModelKey('');
      onStatusRef.current(copy('appliedJson'), 'success');
    } catch (error: unknown) {
      setJsonError((error as Error).message);
    }
  };

  const inputForSelected = selectedModel
    ? Array.isArray(selectedModel.model.input)
      ? selectedModel.model.input
      : ['text']
    : [];
  const effortForSelected = selectedModel
    ? effectiveModelEffort(selectedModel.model)
    : ({ levels: ['off'], default: 'off' } satisfies ModelEffortConfig);

  return (
    <section className="models-console" aria-labelledby="models-console-heading">
      <header className="models-console-heading">
        <div>
          <p className="models-console-eyebrow">{tr('settings.tab.models')}</p>
          <h2 id="models-console-heading">{copy('catalog')}</h2>
          <p>{copy('catalogHint')}</p>
        </div>
        <div className="models-console-heading-actions">
          <span className="models-console-count">
            {copy('modelsConfigured', { count: flatModels.length })}
          </span>
          <button
            ref={addProviderButtonRef}
            className="btn-primary models-console-add-provider"
            type="button"
            onClick={(event) => openAddProvider(event.currentTarget)}
          >
            <Icon name="plus" />
            {copy('addProvider')}
          </button>
        </div>
      </header>

      {providers.length > 0 ? (
        <>
          <div className="models-console-toolbar" aria-label={copy('filters')}>
            <label className="models-console-search">
              <Icon name="search" />
              <span className="visually-hidden">{copy('search')}</span>
              <input
                ref={modelSearchInputRef}
                type="search"
                value={searchText}
                placeholder={copy('search')}
                onChange={(event) => setSearchText(event.target.value)}
              />
            </label>
            <div className="models-console-capabilities" role="group" aria-label={copy('filters')}>
              {(['text', 'image', 'reasoning'] as const).map((filter) => (
                <button
                  key={filter}
                  type="button"
                  aria-pressed={capabilityFilters.has(filter)}
                  onClick={() => toggleCapabilityFilter(filter)}
                >
                  {filter === 'text'
                    ? tr('settings.field.text')
                    : filter === 'image'
                      ? tr('settings.field.image')
                      : tr('settings.field.reasoning')}
                </button>
              ))}
            </div>
          </div>

          <nav className="models-console-provider-tabs" aria-label={copy('provider')}>
            <button
              ref={allProvidersTabRef}
              type="button"
              className={providerFilter === 'all' ? 'active' : ''}
              aria-current={providerFilter === 'all' ? 'page' : undefined}
              onClick={() => setProviderFilter('all')}
            >
              {copy('allProviders')}
              <span>{flatModels.length}</span>
            </button>
            {providers.map((provider) => (
              <button
                key={provider._key}
                ref={(node) => {
                  if (node) providerTabRefs.current.set(provider._key, node);
                  else providerTabRefs.current.delete(provider._key);
                }}
                type="button"
                className={providerFilter === provider._key ? 'active' : ''}
                aria-current={providerFilter === provider._key ? 'page' : undefined}
                onClick={() => {
                  setProviderFilter(provider._key);
                  setActiveProviderKey(provider._key);
                  setSelectedModelKey('');
                  setDeleteIntent(null);
                }}
              >
                {provider.name}
                <span>{provider.models.length}</span>
              </button>
            ))}
          </nav>

          <div className={`models-console-layout${selectedModel ? ' has-inspector' : ''}`}>
            <div className="models-console-main">
              {connectionProvider ? (
                <section
                  className="models-console-connection"
                  aria-labelledby="provider-connection-heading"
                >
                  <div className="models-console-section-heading">
                    <div className="models-console-provider-identity">
                      <span className="models-console-provider-mark" aria-hidden="true">
                        {providerInitial(connectionProvider.name)}
                      </span>
                      <div>
                        <h3 id="provider-connection-heading">{copy('providerConnection')}</h3>
                        <p>{copy('providerConnectionHint')}</p>
                      </div>
                    </div>
                    <div className="models-console-connection-actions">
                      {providers.length > 1 ? (
                        <label>
                          <span className="visually-hidden">{copy('provider')}</span>
                          <select
                            value={connectionProvider._key}
                            onChange={(event) => {
                              setActiveProviderKey(event.target.value);
                              if (providerFilter !== 'all') setProviderFilter(event.target.value);
                              setSelectedModelKey('');
                              setDeleteIntent(null);
                            }}
                          >
                            {providers.map((provider) => (
                              <option key={provider._key} value={provider._key}>
                                {provider.name}
                              </option>
                            ))}
                          </select>
                        </label>
                      ) : null}
                      <button
                        type="button"
                        className={`btn-test ${connectionProvider.testState === 'testing' ? 'testing' : connectionProvider.testState === 'ok' ? 'test-ok' : connectionProvider.testState === 'fail' ? 'test-fail' : ''}`}
                        disabled={
                          !connectionProvider.models.some((model) => model.id.trim()) ||
                          connectionProvider.testState === 'testing'
                        }
                        onClick={() => {
                          const selectedId =
                            selectedModel?.provider._key === connectionProvider._key
                              ? selectedModel.model.id.trim()
                              : '';
                          const retainedTestId = connectionProvider.models.some(
                            (model) => model.id === connectionProvider.selectedTestModel,
                          )
                            ? connectionProvider.selectedTestModel
                            : '';
                          const modelId =
                            selectedId ||
                            retainedTestId ||
                            connectionProvider.models.find((model) => model.id.trim())?.id ||
                            '';
                          void testModel(connectionProvider, modelId);
                        }}
                      >
                        {connectionProvider.testState === 'testing' ? (
                          <Icon name="refresh" />
                        ) : null}
                        {localizedTestLabel(connectionProvider)}
                      </button>
                      <button
                        ref={providerDeleteButtonRef}
                        type="button"
                        className="models-console-danger-icon"
                        aria-label={tr('settings.deleteProvider')}
                        title={tr('settings.deleteProvider')}
                        onClick={(event) =>
                          requestDelete(
                            {
                              kind: 'provider',
                              providerKey: connectionProvider._key,
                            },
                            event.currentTarget,
                          )
                        }
                      >
                        <Icon name="trash" />
                      </button>
                    </div>
                  </div>

                  {deleteIntent?.kind === 'provider' &&
                  deleteIntent.providerKey === connectionProvider._key ? (
                    <div
                      className="models-console-confirm"
                      role="alert"
                      data-console-escape-layer="true"
                    >
                      <div>
                        <strong>{copy('deleteProviderQuestion')}</strong>
                        <span>
                          {connectionProvider.name} ·{' '}
                          {copy('modelCount', { count: connectionProvider.models.length })}
                        </span>
                        <small>{copy('deleteCannotUndo')}</small>
                      </div>
                      <div>
                        <button type="button" className="btn-secondary" onClick={cancelDelete}>
                          {tr('common.cancel')}
                        </button>
                        <button
                          ref={deleteConfirmButtonRef}
                          type="button"
                          className="btn-primary btn-danger"
                          onClick={confirmDelete}
                        >
                          {copy('confirmDelete')}
                        </button>
                      </div>
                    </div>
                  ) : null}

                  <div className="models-console-connection-grid">
                    <label>
                      <span>{tr('settings.field.apiType')}</span>
                      <select
                        value={connectionProvider.api}
                        onChange={(event) =>
                          updateProvider(connectionProvider._key, { api: event.target.value })
                        }
                      >
                        {API_OPTIONS.map((option) => (
                          <option key={option.value} value={option.value}>
                            {option.label}
                          </option>
                        ))}
                      </select>
                    </label>
                    <label className="models-console-base-url">
                      <span>{tr('settings.field.baseUrl')}</span>
                      <input
                        type="url"
                        value={connectionProvider.baseUrl}
                        placeholder="https://api.example.com/v1"
                        onChange={(event) =>
                          updateProvider(connectionProvider._key, { baseUrl: event.target.value })
                        }
                      />
                    </label>
                    <label className="models-console-api-key">
                      <span>{tr('settings.field.apiKey')}</span>
                      <span className="models-console-secret-input">
                        <input
                          type={showApiKey ? 'text' : 'password'}
                          value={connectionProvider.apiKey}
                          autoComplete="off"
                          onChange={(event) =>
                            updateProvider(connectionProvider._key, { apiKey: event.target.value })
                          }
                        />
                        <button
                          type="button"
                          aria-pressed={showApiKey}
                          onClick={() => setShowApiKey((visible) => !visible)}
                        >
                          {showApiKey ? copy('hideApiKey') : copy('showApiKey')}
                        </button>
                      </span>
                      <small>{copy('apiKeyHint')}</small>
                    </label>
                  </div>
                </section>
              ) : null}

              <div className="models-console-list-heading">
                <div>
                  <h3>{tr('settings.field.models')}</h3>
                  <span>{copy('modelsConfigured', { count: filteredModels.length })}</span>
                </div>
                <button
                  ref={addModelButtonRef}
                  type="button"
                  className="btn-secondary"
                  disabled={!connectionProvider}
                  onClick={addModel}
                >
                  <Icon name="plus" />
                  {copy('addModel')}
                </button>
              </div>

              {filteredModels.length > 0 ? (
                <div className="models-console-grid">
                  {filteredModels.map(({ provider, model }) => {
                    const capabilities = modelCapabilities(model);
                    const selected = selectedModelKey === model._key;
                    return (
                      <button
                        key={model._key}
                        ref={(node) => {
                          if (node) modelCardRefs.current.set(model._key, node);
                          else modelCardRefs.current.delete(model._key);
                        }}
                        type="button"
                        className={`models-console-card${selected ? ' selected' : ''}`}
                        aria-pressed={selected}
                        aria-controls="models-console-inspector"
                        onClick={(event) => {
                          const keyboardActivated = event.detail === 0;
                          captureMobileCatalogScroll(event.currentTarget);
                          selectedModelOriginRef.current = event.currentTarget;
                          setSelectedModelKey(model._key);
                          setActiveProviderKey(provider._key);
                          setDeleteIntent(null);
                          if (keyboardActivated) {
                            window.setTimeout(focusModelInspector, 0);
                          }
                        }}
                      >
                        <span className="models-console-card-head">
                          <span className="models-console-provider-mark" aria-hidden="true">
                            {providerInitial(provider.name)}
                          </span>
                          <span className="models-console-card-title">
                            <strong>{model.name || model.id || copy('untitledModel')}</strong>
                            <small>
                              {provider.name} / {model.id || 'model-id'}
                            </small>
                          </span>
                          <span className="models-console-card-arrow" aria-hidden="true">
                            <Icon name="chevron-right" />
                          </span>
                        </span>
                        <span className="models-console-tags">
                          <span data-capability="api">{providerApiLabel(provider.api)}</span>
                          {[...capabilities].map((capability) => (
                            <span key={capability} data-capability={capability}>
                              {capability === 'text'
                                ? tr('settings.field.text')
                                : capability === 'image'
                                  ? tr('settings.field.image')
                                  : tr('settings.field.reasoning')}
                            </span>
                          ))}
                        </span>
                        <span className="models-console-card-metrics">
                          <span>
                            <small>{tr('settings.field.contextWindow')}</small>
                            <strong>{model.contextWindow?.toLocaleString() || '—'}</strong>
                          </span>
                          <span>
                            <small>{tr('settings.field.maxTokens')}</small>
                            <strong>{model.maxTokens?.toLocaleString() || '—'}</strong>
                          </span>
                          <span>
                            <small>{copy('thinkingEffort')}</small>
                            <strong>
                              {copy('effortSummary', {
                                default: effortLabel(effectiveModelEffort(model).default),
                                count: effectiveModelEffort(model).levels.length,
                              })}
                            </strong>
                          </span>
                        </span>
                      </button>
                    );
                  })}
                </div>
              ) : (
                <div className="models-console-empty models-console-empty-inline">
                  <span className="models-console-empty-icon">
                    <Icon name="search" />
                  </span>
                  <h3>{copy('noMatches')}</h3>
                  <p>{copy('noMatchesHint')}</p>
                  <button type="button" className="btn-secondary" onClick={clearFilters}>
                    {copy('clearFilters')}
                  </button>
                </div>
              )}
            </div>

            <aside
              id="models-console-inspector"
              ref={inspectorRef}
              className="models-console-inspector"
              aria-label={copy('selectedModel')}
              tabIndex={-1}
            >
              {selectedModel ? (
                <>
                  <div className="models-console-inspector-head">
                    <div>
                      <p>{selectedModel.provider.name}</p>
                      <h3>{copy('editModel')}</h3>
                      <span>{copy('editModelHint')}</span>
                    </div>
                    <button
                      type="button"
                      aria-label={tr('common.close')}
                      title={tr('common.close')}
                      onClick={closeInspector}
                    >
                      <Icon name="close" />
                    </button>
                  </div>

                  <div className="models-console-inspector-body">
                    <label>
                      <span>{tr('settings.field.modelId')}</span>
                      <input
                        type="text"
                        value={selectedModel.model.id || ''}
                        placeholder="model-id"
                        onChange={(event) =>
                          updateModel(selectedModel.provider._key, selectedModel.model._key, {
                            id: event.target.value,
                          })
                        }
                      />
                    </label>
                    <label>
                      <span>{copy('displayName')}</span>
                      <input
                        type="text"
                        value={selectedModel.model.name || ''}
                        onChange={(event) =>
                          updateModel(selectedModel.provider._key, selectedModel.model._key, {
                            name: event.target.value || undefined,
                          })
                        }
                      />
                    </label>

                    <fieldset className="models-console-capability-editor">
                      <legend>{copy('capabilities')}</legend>
                      <label>
                        <input
                          type="checkbox"
                          checked={inputForSelected.includes('text')}
                          onChange={(event) => {
                            const next = new Set(inputForSelected);
                            if (event.target.checked) next.add('text');
                            else next.delete('text');
                            updateModel(selectedModel.provider._key, selectedModel.model._key, {
                              input: next.size > 0 ? [...next] : undefined,
                            });
                          }}
                        />
                        {tr('settings.field.text')}
                      </label>
                      <label>
                        <input
                          type="checkbox"
                          checked={inputForSelected.includes('image')}
                          onChange={(event) => {
                            const next = new Set(inputForSelected);
                            if (event.target.checked) next.add('image');
                            else next.delete('image');
                            updateModel(selectedModel.provider._key, selectedModel.model._key, {
                              input: next.size > 0 ? [...next] : undefined,
                            });
                          }}
                        />
                        {tr('settings.field.image')}
                      </label>
                      <label>
                        <input
                          type="checkbox"
                          checked={!!selectedModel.model.reasoning}
                          onChange={(event) => {
                            const reasoning = event.target.checked;
                            updateModel(selectedModel.provider._key, selectedModel.model._key, {
                              reasoning: reasoning || undefined,
                              effort: reasoning
                                ? withModelEffort(
                                    selectedModel.model,
                                    [...THINKING_EFFORT_LEVELS],
                                    'auto',
                                  )
                                : selectedModel.model.effort
                                  ? withModelEffort(selectedModel.model, ['off'], 'off')
                                  : undefined,
                            });
                          }}
                        />
                        {tr('settings.field.reasoning')}
                      </label>
                    </fieldset>

                    {selectedModel.model.reasoning ? (
                      <fieldset className="models-console-effort-editor">
                        <legend>{copy('thinkingEffort')}</legend>
                        <p>{copy('effortHint')}</p>
                        <div className="models-console-effort-levels">
                          {THINKING_EFFORT_LEVELS.map((effort) => {
                            const checked = effortForSelected.levels.includes(effort);
                            return (
                              <label key={effort}>
                                <input
                                  type="checkbox"
                                  checked={checked}
                                  disabled={checked && effortForSelected.levels.length === 1}
                                  onChange={(event) => {
                                    const selected = new Set(effortForSelected.levels);
                                    if (event.target.checked) selected.add(effort);
                                    else selected.delete(effort);
                                    const levels = THINKING_EFFORT_LEVELS.filter((level) =>
                                      selected.has(level),
                                    );
                                    if (levels.length === 0) return;
                                    updateModel(
                                      selectedModel.provider._key,
                                      selectedModel.model._key,
                                      {
                                        effort: withModelEffort(
                                          selectedModel.model,
                                          [...levels],
                                          levels.includes(effortForSelected.default)
                                            ? effortForSelected.default
                                            : levels[0],
                                        ),
                                      },
                                    );
                                  }}
                                />
                                {effortLabel(effort)}
                              </label>
                            );
                          })}
                        </div>
                        <label className="models-console-effort-default">
                          <span>{copy('defaultEffort')}</span>
                          <select
                            value={effortForSelected.default}
                            onChange={(event) =>
                              updateModel(selectedModel.provider._key, selectedModel.model._key, {
                                effort: withModelEffort(
                                  selectedModel.model,
                                  [...effortForSelected.levels],
                                  event.target.value as ThinkingEffort,
                                ),
                              })
                            }
                          >
                            {effortForSelected.levels.map((effort) => (
                              <option key={effort} value={effort}>
                                {effortLabel(effort)}
                              </option>
                            ))}
                          </select>
                        </label>
                      </fieldset>
                    ) : null}

                    <div className="models-console-number-grid">
                      <label>
                        <span>{tr('settings.field.contextWindow')}</span>
                        <input
                          type="number"
                          min="0"
                          value={selectedModel.model.contextWindow ?? ''}
                          onChange={(event) =>
                            updateModel(selectedModel.provider._key, selectedModel.model._key, {
                              contextWindow: parseOptionalInteger(event.target.value),
                            })
                          }
                        />
                      </label>
                      <label>
                        <span>{tr('settings.field.maxTokens')}</span>
                        <input
                          type="number"
                          min="0"
                          value={selectedModel.model.maxTokens ?? ''}
                          onChange={(event) =>
                            updateModel(selectedModel.provider._key, selectedModel.model._key, {
                              maxTokens: parseOptionalInteger(event.target.value),
                            })
                          }
                        />
                      </label>
                    </div>

                    <label>
                      <span>{tr('settings.thinkingFormat')}</span>
                      <input
                        type="text"
                        list="models-console-thinking-formats"
                        value={thinkingFormat(selectedModel.model)}
                        placeholder={tr('settings.thinkingFormatDefault')}
                        onChange={(event) => {
                          const next = withThinkingFormat(selectedModel.model, event.target.value);
                          updateModel(selectedModel.provider._key, selectedModel.model._key, {
                            compat: next.compat,
                          });
                        }}
                      />
                      <small>{copy('customThinkingFormat')}</small>
                    </label>
                    <datalist id="models-console-thinking-formats">
                      {THINKING_FORMAT_OPTIONS.map((format) => (
                        <option key={format} value={format} />
                      ))}
                    </datalist>
                  </div>

                  {deleteIntent?.kind === 'model' &&
                  deleteIntent.modelKey === selectedModel.model._key ? (
                    <div
                      className="models-console-confirm models-console-inspector-confirm"
                      role="alert"
                      data-console-escape-layer="true"
                    >
                      <div>
                        <strong>{copy('deleteModelQuestion')}</strong>
                        <span>{selectedModel.model.id || copy('untitledModel')}</span>
                        <small>{copy('deleteCannotUndo')}</small>
                      </div>
                      <div>
                        <button type="button" className="btn-secondary" onClick={cancelDelete}>
                          {tr('common.cancel')}
                        </button>
                        <button
                          ref={deleteConfirmButtonRef}
                          type="button"
                          className="btn-primary btn-danger"
                          onClick={confirmDelete}
                        >
                          {copy('confirmDelete')}
                        </button>
                      </div>
                    </div>
                  ) : (
                    <div className="models-console-inspector-actions">
                      <button
                        type="button"
                        className="btn-secondary"
                        disabled={
                          !selectedModel.model.id.trim() ||
                          selectedModel.provider.testState === 'testing'
                        }
                        onClick={() =>
                          void testModel(selectedModel.provider, selectedModel.model.id)
                        }
                      >
                        {selectedModel.provider.testState === 'testing' ? (
                          <Icon name="refresh" />
                        ) : (
                          <Icon name="check" />
                        )}
                        {copy('testConnection')}
                      </button>
                      <button
                        ref={modelDeleteButtonRef}
                        type="button"
                        className="models-console-delete-button"
                        onClick={(event) =>
                          requestDelete(
                            {
                              kind: 'model',
                              providerKey: selectedModel.provider._key,
                              modelKey: selectedModel.model._key,
                            },
                            event.currentTarget,
                          )
                        }
                      >
                        <Icon name="trash" />
                        {tr('settings.removeModel')}
                      </button>
                    </div>
                  )}
                </>
              ) : (
                <div className="models-console-inspector-empty">
                  <span className="models-console-empty-icon">
                    <Icon name="check" />
                  </span>
                  <h3>{copy('selectedModel')}</h3>
                  <p>{copy('selectModelHint')}</p>
                </div>
              )}
            </aside>
          </div>
        </>
      ) : (
        <div className="models-console-empty">
          <span className="models-console-empty-icon">
            <Icon name="plus" />
          </span>
          <h3>{copy('noProviders')}</h3>
          <p>{copy('noProvidersHint')}</p>
          <button
            type="button"
            className="btn-primary"
            onClick={(event) => openAddProvider(event.currentTarget)}
          >
            {copy('addProvider')}
          </button>
        </div>
      )}

      <details className="models-console-json">
        <summary>{tr('settings.advancedRawJson')}</summary>
        <div className="models-console-json-head">
          <div>
            <h3>{copy('jsonDraft')}</h3>
            <p>{copy('jsonHint')}</p>
          </div>
          <button type="button" className="btn-secondary" onClick={syncJsonFromForm}>
            <Icon name="refresh" />
            {copy('syncJson')}
          </button>
        </div>
        {formDirty ? (
          <div className="models-console-json-warning" role="status">
            {copy('rawConflict')}
          </div>
        ) : null}
        <textarea
          className={`json-editor${jsonError ? ' has-error' : ''}`}
          aria-label={copy('jsonDraft')}
          spellCheck={false}
          value={jsonText}
          onChange={(event) => {
            setJsonText(event.target.value);
            setJsonDirty(true);
            jsonDirtyRef.current = true;
            onDraftDirtyChangeRef.current?.(true);
            setJsonError('');
          }}
        />
        {jsonError ? (
          <div className="json-editor-error" role="alert">
            {jsonError}
          </div>
        ) : null}
        <button
          type="button"
          className="btn-secondary"
          disabled={formDirty || !jsonDirty}
          onClick={applyJson}
        >
          {tr('settings.applyJson')}
        </button>
      </details>

      {addProviderOpen ? (
        <div
          className="models-console-dialog-backdrop"
          role="presentation"
          onMouseDown={(event) => {
            if (event.target === event.currentTarget) {
              setAddProviderOpen(false);
              setNewProviderError('');
            }
          }}
        >
          <form
            ref={addProviderDialogRef}
            className="models-console-dialog"
            role="dialog"
            aria-modal="true"
            aria-labelledby="add-provider-title"
            onSubmit={createProvider}
          >
            <div className="models-console-dialog-head">
              <div>
                <h3 id="add-provider-title">{copy('addProviderTitle')}</h3>
                <p>{copy('addProviderHint')}</p>
              </div>
              <button
                type="button"
                aria-label={tr('common.close')}
                onClick={() => {
                  setAddProviderOpen(false);
                  setNewProviderError('');
                }}
              >
                <Icon name="close" />
              </button>
            </div>
            <label>
              <span>{copy('providerName')}</span>
              <input
                autoFocus
                type="text"
                value={newProviderName}
                aria-invalid={!!newProviderError}
                aria-describedby={newProviderError ? 'new-provider-error' : undefined}
                placeholder="openai"
                onChange={(event) => {
                  setNewProviderName(event.target.value);
                  setNewProviderError('');
                }}
              />
            </label>
            {newProviderError ? (
              <div id="new-provider-error" className="models-console-dialog-error" role="alert">
                {newProviderError}
              </div>
            ) : null}
            <div className="models-console-dialog-actions">
              <button
                type="button"
                className="btn-secondary"
                onClick={() => {
                  setAddProviderOpen(false);
                  setNewProviderError('');
                }}
              >
                {tr('common.cancel')}
              </button>
              <button type="submit" className="btn-primary" disabled={!newProviderName.trim()}>
                {copy('createProvider')}
              </button>
            </div>
          </form>
        </div>
      ) : null}
    </section>
  );
}

export default ModelsConsole;
