import { describe, expect, it } from 'vitest';

import {
  isBuiltinProviderName,
  validateMcpConfigDraftShape,
  validateModelsConfigDraftShape,
} from '../src/settingsValidation.js';

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

  it('accepts OpenAI Responses provider api kind', () => {
    expect(() =>
      validateModelsConfigDraftShape({
        providers: {
          openaiResponses: {
            api: 'openai-responses',
            baseUrl: 'https://api.openai.com/v1',
            apiKey: 'test-key',
            models: [{ id: 'gpt-5.5', input: ['text', 'image'] }],
          },
        },
      }),
    ).not.toThrow();
  });

  it('accepts OpenAI Responses as a built-in provider prefix', () => {
    expect(isBuiltinProviderName('openai-responses')).toBe(true);
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

  it('validates MCP effective transport requirements', () => {
    expect(() =>
      validateMcpConfigDraftShape({
        local: { command: 'uvx', args: ['demo'] },
        remote: { url: 'https://example.com/mcp' },
        legacy: { command: 'uvx', url: 'not-a-streamable-http-url' },
      }),
    ).not.toThrow();

    expect(() => validateMcpConfigDraftShape({ broken: { args: [] } })).toThrow('stdio');
    expect(() =>
      validateMcpConfigDraftShape({
        remote: { transport: 'streamable-http', url: 'ftp://example.com/mcp' },
      }),
    ).toThrow('http:// or https://');
  });

  it('validates MCP auth object shape', () => {
    expect(() =>
      validateMcpConfigDraftShape({
        remote: {
          url: 'https://example.com/mcp',
          auth: {
            clientId: 'client-id',
            clientSecret: '${MCP_CLIENT_SECRET}',
            scopes: ['repo', 'read:user'],
          },
        },
      }),
    ).not.toThrow();

    expect(() =>
      validateMcpConfigDraftShape({
        remote: { url: 'https://example.com/mcp', auth: { scopes: 'repo' } },
      }),
    ).toThrow('auth.scopes');

    expect(() =>
      validateMcpConfigDraftShape({
        remote: { url: 'https://example.com/mcp', auth: { clientId: 123 } },
      }),
    ).toThrow('auth.clientId');
  });
});
