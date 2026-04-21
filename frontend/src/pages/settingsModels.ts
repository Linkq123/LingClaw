import type { AppConfig, ModelEntry, ProviderConfig } from '../types/config.js';

export interface ModelFormEntry extends ModelEntry {
  _key: string;
}

export interface ProviderFormData {
  name: string;
  api: string;
  baseUrl: string;
  apiKey: string;
  models: ModelFormEntry[];
  testState: 'idle' | 'testing' | 'ok' | 'fail';
  testLabel: string;
  selectedTestModel: string;
}

let modelFormKeyCounter = 0;

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
    name,
    api: provider.api || 'openai-completions',
    baseUrl: provider.baseUrl || '',
    apiKey: provider.apiKey || '',
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
    const models = provider.models
      .filter((model) => model.id.trim() !== '')
      .map((model) => {
        const entry: ModelEntry = { id: model.id.trim() };
        if (model.reasoning) entry.reasoning = true;
        if (model.contextWindow != null) entry.contextWindow = model.contextWindow;
        if (model.maxTokens != null) entry.maxTokens = model.maxTokens;
        if (model.input && model.input.length > 0) entry.input = model.input;
        return entry;
      });
    nextProviders[provider.name] = {
      api: provider.api as ProviderConfig['api'],
      baseUrl: provider.baseUrl || undefined,
      apiKey: provider.apiKey || undefined,
      models,
    };
  }

  return Object.keys(nextProviders).length > 0 ? { providers: nextProviders } : undefined;
}
