import { describe, expect, it } from 'vitest';

import {
  buildProviderForms,
  createProviderForm,
  serializeProviderForms,
} from '../src/pages/settingsModels.js';

describe('settings model helpers', () => {
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
          baseUrl: undefined,
          apiKey: undefined,
          models: [{ id: 'gpt-4o-mini', input: ['text'] }],
        },
      },
    });
  });
});
