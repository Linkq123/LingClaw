import { describe, expect, it } from 'vitest';

import {
  buildProviderForms,
  createProviderForm,
  normalizeModelsConfig,
  serializeProviderForms,
} from '../src/pages/settingsModels.js';

describe('settings model helpers', () => {
  it('preserves provider row keys when provider forms are rehydrated', () => {
    const initial = buildProviderForms({
      openai: {
        api: 'openai-completions',
        baseUrl: 'https://api.openai.com/v1',
        apiKey: 'sk-test',
        models: [{ id: 'gpt-4o-mini', input: ['text'] }],
      },
    });

    const originalKey = initial[0]._key;
    const updated = buildProviderForms(
      {
        openai: {
          api: 'openai-completions',
          baseUrl: 'https://api.openai.com/v1',
          apiKey: 'sk-test',
          models: [{ id: 'gpt-4.1-mini', input: ['text'] }],
        },
      },
      initial,
    );

    expect(updated[0]._key).toBe(originalKey);
  });

  it('creates a new provider row key when the same name is removed and re-added', () => {
    const initial = buildProviderForms({
      openai: {
        api: 'openai-completions',
        models: [{ id: 'gpt-4o-mini', input: ['text'] }],
      },
    });

    const recreated = buildProviderForms(
      {
        openai: {
          api: 'openai-completions',
          models: [{ id: 'gpt-4o-mini', input: ['text'] }],
        },
      },
      [],
    );

    expect(recreated[0]._key).not.toBe(initial[0]._key);
  });

  it('preserves model row keys when provider models are rehydrated', () => {
    const initial = buildProviderForms({
      openai: {
        api: 'openai-completions',
        baseUrl: 'https://api.openai.com/v1',
        apiKey: 'sk-test',
        models: [{ id: 'gpt-4o-mini', input: ['text'] }],
      },
    });

    const originalKey = initial[0].models[0]._key;
    const updated = buildProviderForms(
      {
        openai: {
          api: 'openai-completions',
          baseUrl: 'https://api.openai.com/v1',
          apiKey: 'sk-test',
          models: [{ id: 'gpt-4.1-mini', input: ['text'] }],
        },
      },
      initial,
    );

    expect(updated[0].models[0]._key).toBe(originalKey);
  });

  it('creates distinct keys for newly added model rows', () => {
    const provider = createProviderForm('openai');
    const withModel = buildProviderForms(
      {
        openai: {
          api: 'openai-completions',
          models: [{ id: 'gpt-4o-mini', input: ['text'] }],
        },
      },
      [provider],
    );
    const expanded = buildProviderForms(
      {
        openai: {
          api: 'openai-completions',
          models: [
            { id: 'gpt-4o-mini', input: ['text'] },
            { id: 'gpt-4.1-mini', input: ['text'] },
          ],
        },
      },
      withModel,
    );

    expect(expanded[0].models).toHaveLength(2);
    expect(expanded[0].models[0]._key).toBe(withModel[0].models[0]._key);
    expect(expanded[0].models[1]._key).not.toBe(expanded[0].models[0]._key);
  });

  it('omits blank model ids when serializing provider forms', () => {
    const providers = [
      {
        ...createProviderForm('openai'),
        models: [
          { id: ' gpt-4o-mini ', input: ['text'], _key: 'first' },
          { id: '   ', input: ['text'], _key: 'second' },
        ],
      },
    ];

    const serialized = serializeProviderForms(providers);

    expect(serialized).toEqual({
      providers: {
        openai: {
          api: 'openai-completions',
          baseUrl: '',
          apiKey: '',
          models: [{ id: 'gpt-4o-mini', input: ['text'] }],
        },
      },
    });
  });

  it('preserves empty apiKey strings and unknown provider metadata when serializing', () => {
    const providers = buildProviderForms({
      ollama: {
        api: 'ollama',
        baseUrl: 'http://127.0.0.1:11434',
        apiKey: '',
        models: [
          {
            id: 'gemma4:e4b',
            input: ['text', 'image'],
            maxTokens: 12800,
            reasoning: true,
            name: 'gemma4:e4b',
            compat: { thinkingFormat: 'ollama' },
            cost: { input: 0, output: 0 },
          },
        ],
      },
    });

    const serialized = serializeProviderForms(providers);

    expect(serialized).toEqual({
      providers: {
        ollama: {
          api: 'ollama',
          baseUrl: 'http://127.0.0.1:11434',
          apiKey: '',
          models: [
            {
              id: 'gemma4:e4b',
              input: ['text', 'image'],
              maxTokens: 12800,
              reasoning: true,
              name: 'gemma4:e4b',
              compat: { thinkingFormat: 'ollama' },
              cost: { input: 0, output: 0 },
            },
          ],
        },
      },
    });
  });

  it('normalizes missing provider auth fields to backend-compatible empty strings', () => {
    const normalized = normalizeModelsConfig({
      providers: {
        ollama: {
          api: 'ollama',
          baseUrl: 'http://127.0.0.1:11434',
          models: [{ id: 'gemma4:e4b', input: ['text'] }],
        },
      },
    });

    expect(normalized).toEqual({
      providers: {
        ollama: {
          api: 'ollama',
          baseUrl: 'http://127.0.0.1:11434',
          apiKey: '',
          models: [{ id: 'gemma4:e4b', input: ['text'] }],
        },
      },
    });
  });
});
