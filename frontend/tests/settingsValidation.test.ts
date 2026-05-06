import { describe, expect, it } from 'vitest';

import { validateModelsConfigDraftShape } from '../src/settingsValidation.js';

describe('settings validation', () => {
  it('accepts providers with an explicit empty apiKey string', () => {
    expect(() =>
      validateModelsConfigDraftShape({
        providers: {
          ollama: {
            api: 'ollama',
            baseUrl: 'http://127.0.0.1:11434',
            apiKey: '',
            models: [{ id: 'gemma4:e4b', input: ['text', 'image'] }],
          },
        },
      }),
    ).not.toThrow();
  });

  it('accepts Gemini provider api kind', () => {
    expect(() =>
      validateModelsConfigDraftShape({
        providers: {
          gemini: {
            api: 'gemini',
            baseUrl: 'https://generativelanguage.googleapis.com/v1beta',
            apiKey: 'test-key',
            models: [{ id: 'gemini-2.5-flash', input: ['text', 'image'] }],
          },
        },
      }),
    ).not.toThrow();
  });

  it('accepts compat.thinkingFormat as a string', () => {
    expect(() =>
      validateModelsConfigDraftShape({
        providers: {
          openai: {
            api: 'openai-completions',
            baseUrl: 'https://gateway.example/v1',
            apiKey: 'test-key',
            models: [
              {
                id: 'gpt-5.4',
                input: ['text'],
                compat: { thinkingFormat: 'openai' },
              },
            ],
          },
        },
      }),
    ).not.toThrow();
  });

  it('rejects providers that omit apiKey', () => {
    expect(() =>
      validateModelsConfigDraftShape({
        providers: {
          ollama: {
            api: 'ollama',
            baseUrl: 'http://127.0.0.1:11434',
            models: [{ id: 'gemma4:e4b', input: ['text'] }],
          },
        },
      }),
    ).toThrow('apiKey');
  });

  it('rejects providers with an empty baseUrl', () => {
    expect(() =>
      validateModelsConfigDraftShape({
        providers: {
          ollama: {
            api: 'ollama',
            baseUrl: '',
            apiKey: '',
            models: [{ id: 'gemma4:e4b', input: ['text'] }],
          },
        },
      }),
    ).toThrow('baseUrl');
  });

  it('rejects non-object compat and non-string compat.thinkingFormat', () => {
    expect(() =>
      validateModelsConfigDraftShape({
        providers: {
          openai: {
            api: 'openai-completions',
            baseUrl: 'https://gateway.example/v1',
            apiKey: 'test-key',
            models: [{ id: 'gpt-5.4', compat: 'openai' }],
          },
        },
      }),
    ).toThrow('compat');

    expect(() =>
      validateModelsConfigDraftShape({
        providers: {
          openai: {
            api: 'openai-completions',
            baseUrl: 'https://gateway.example/v1',
            apiKey: 'test-key',
            models: [{ id: 'gpt-5.4', compat: { thinkingFormat: 123 } }],
          },
        },
      }),
    ).toThrow('thinkingFormat');
  });
});
