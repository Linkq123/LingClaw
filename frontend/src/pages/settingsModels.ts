import type { AppConfig, ModelEntry, ProviderConfig } from '../types/config.js';

export interface ModelFormEntry extends ModelEntry {
  _key: string;
  [key: string]: unknown;
}

export interface ProviderFormData {
  _key: string;
  name: string;
  api: string;
  baseUrl: string;
  apiKey: string;
  models: ModelFormEntry[];
  testState: 'idle' | 'testing' | 'ok' | 'fail';
  testLabel: string;
  selectedTestModel: string;
  [key: string]: unknown;
}

let providerFormKeyCounter = 0;
let modelFormKeyCounter = 0;

function nextProviderFormKey(name: string): string {
  providerFormKeyCounter += 1;
  return `${name}-${providerFormKeyCounter}`;
}

function nextModelFormKey(providerName: string): string {
  modelFormKeyCounter += 1;
  return `${providerName}-${modelFormKeyCounter}`;
}

export function createModelFormEntry(
  providerName: string,
  model: Partial<ModelFormEntry> = {},
): ModelFormEntry {
  return {
    id: '',
    input: ['text'],
    ...model,
    _key: model._key || nextModelFormKey(providerName),
  };
}

export function createProviderForm(
  name: string,
  provider: ProviderConfig = {},
  previous?: ProviderFormData,
): ProviderFormData {
  const previousModels = previous?.models || [];
  const models = (provider.models || []).map((model, index) =>
    createModelFormEntry(name, {
      ...model,
      _key: previousModels[index]?._key,
    }),
  );
  const selectedTestModel =
    previous?.selectedTestModel && models.some((model) => model.id === previous.selectedTestModel)
      ? previous.selectedTestModel
      : models[0]?.id || '';

  return {
    ...(provider as Record<string, unknown>),
    _key: previous?._key || nextProviderFormKey(name),
    name,
    api: provider.api || 'openai-completions',
    baseUrl: provider.baseUrl ?? '',
    apiKey: provider.apiKey ?? '',
    models,
    testState: previous?.testState || 'idle',
    testLabel: previous?.testLabel || 'Test',
    selectedTestModel,
  };
}

export function buildProviderForms(
  providers: Record<string, ProviderConfig> | undefined,
  previousForms: ProviderFormData[] = [],
): ProviderFormData[] {
  const previousByName = new Map(previousForms.map((provider) => [provider.name, provider]));

  return Object.entries(providers || {})
    .sort(([lhs], [rhs]) => lhs.localeCompare(rhs))
    .map(([name, provider]) => createProviderForm(name, provider, previousByName.get(name)));
}

export function serializeProviderForms(providers: ProviderFormData[]): AppConfig['models'] {
  const nextProviders: Record<string, ProviderConfig> = {};
  for (const provider of providers) {
    const {
      _key,
      name,
      models: providerModels,
      testState,
      testLabel,
      selectedTestModel,
      ...rest
    } = provider;
    void _key;
    void testState;
    void testLabel;
    void selectedTestModel;

    const models = providerModels
      .filter((model) => model.id.trim() !== '')
      .map((model) => {
        const { _key, ...modelFields } = model;
        void _key;
        const entry: ModelEntry = {
          ...(modelFields as ModelEntry),
          id: model.id.trim(),
        };

        if (!model.reasoning) delete entry.reasoning;
        if (model.contextWindow == null) delete entry.contextWindow;
        if (model.maxTokens == null) delete entry.maxTokens;
        if (!model.input || model.input.length === 0) delete entry.input;
        return entry;
      });

    nextProviders[name] = {
      ...(rest as ProviderConfig),
      api: provider.api as ProviderConfig['api'],
      baseUrl: provider.baseUrl,
      apiKey: provider.apiKey,
      models,
    };
  }

  return Object.keys(nextProviders).length > 0 ? { providers: nextProviders } : undefined;
}

export function normalizeModelsConfig(models: AppConfig['models']): AppConfig['models'] {
  return serializeProviderForms(buildProviderForms(models?.providers));
}
