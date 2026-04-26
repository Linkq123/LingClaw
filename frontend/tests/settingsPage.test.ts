import React from 'react';
import { act } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createRoot, type Root } from 'react-dom/client';

import { SettingsPage, openSettingsPage } from '../src/pages/SettingsPage.js';

function jsonResponse(payload: unknown): Response {
  return new Response(JSON.stringify(payload), {
    status: 200,
    headers: { 'Content-Type': 'application/json' },
  });
}

async function flushMicrotasks(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

async function renderSettingsPage(): Promise<{ root: Root; container: HTMLDivElement }> {
  document.body.innerHTML = '<div id="settings-page"></div>';
  const container = document.getElementById('settings-page') as HTMLDivElement;
  const root = createRoot(container);

  await act(async () => {
    root.render(React.createElement(SettingsPage));
    await flushMicrotasks();
  });

  return { root, container };
}

async function openAndLoad(): Promise<void> {
  await act(async () => {
    openSettingsPage();
    await flushMicrotasks();
  });
}

function findButtonByText(text: string): HTMLButtonElement {
  const button = Array.from(document.querySelectorAll('button')).find(
    (node) => node.textContent?.trim() === text,
  );
  if (!(button instanceof HTMLButtonElement)) {
    throw new Error(`Button not found: ${text}`);
  }
  return button;
}

function findPrimaryTestButton(): HTMLButtonElement {
  const button = document.querySelector('button.btn-test');
  if (!(button instanceof HTMLButtonElement)) {
    throw new Error('Test button not found');
  }
  return button;
}

describe('SettingsPage test button timers', () => {
  let root: Root | null = null;

  beforeEach(() => {
    (
      globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }
    ).IS_REACT_ACT_ENVIRONMENT = true;
    vi.useFakeTimers();
  });

  afterEach(async () => {
    if (root) {
      await act(async () => {
        root?.unmount();
        await flushMicrotasks();
      });
      root = null;
    }
    document.body.innerHTML = '';
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean })
      .IS_REACT_ACT_ENVIRONMENT;
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  it('keeps the latest provider test result visible until its own reset timer fires', async () => {
    const fetchMock = vi.fn<typeof fetch>((input) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config') {
        return Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: {
              models: {
                providers: {
                  openai: {
                    api: 'openai-completions',
                    baseUrl: 'https://api.openai.com/v1',
                    apiKey: 'sk-test',
                    models: [{ id: 'gpt-4o-mini', input: ['text'] }],
                  },
                },
              },
            },
          }),
        );
      }
      if (url === '/api/config/test-model') {
        return Promise.resolve(jsonResponse({ ok: true }));
      }
      throw new Error(`Unexpected fetch URL: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);

    ({ root } = await renderSettingsPage());
    await openAndLoad();

    await act(async () => {
      findButtonByText('Models').click();
      await flushMicrotasks();
    });

    await act(async () => {
      findPrimaryTestButton().click();
      await flushMicrotasks();
    });

    expect(findPrimaryTestButton().textContent).toBe('✓ Connected');

    await act(async () => {
      vi.advanceTimersByTime(2000);
      await flushMicrotasks();
    });

    await act(async () => {
      findPrimaryTestButton().click();
      await flushMicrotasks();
    });

    expect(findPrimaryTestButton().textContent).toBe('✓ Connected');

    await act(async () => {
      vi.advanceTimersByTime(2500);
      await flushMicrotasks();
    });

    expect(findPrimaryTestButton().textContent).toBe('✓ Connected');

    await act(async () => {
      vi.advanceTimersByTime(1500);
      await flushMicrotasks();
    });

    expect(findPrimaryTestButton().textContent).toBe('Test');
  });

  it('keeps the latest MCP test result visible until its own reset timer fires', async () => {
    const fetchMock = vi.fn<typeof fetch>((input) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config') {
        return Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: {
              mcpServers: {
                demo: {
                  command: 'uvx',
                  args: ['server'],
                  env: { TOKEN: 'secret' },
                  enabled: true,
                },
              },
            },
          }),
        );
      }
      if (url === '/api/config/test-mcp') {
        return Promise.resolve(jsonResponse({ ok: true, tools: 3 }));
      }
      throw new Error(`Unexpected fetch URL: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);

    ({ root } = await renderSettingsPage());
    await openAndLoad();

    await act(async () => {
      findButtonByText('MCP').click();
      await flushMicrotasks();
    });

    await act(async () => {
      findPrimaryTestButton().click();
      await flushMicrotasks();
    });

    expect(findPrimaryTestButton().textContent).toBe('✓ 3 tools');

    await act(async () => {
      vi.advanceTimersByTime(2000);
      await flushMicrotasks();
    });

    await act(async () => {
      findPrimaryTestButton().click();
      await flushMicrotasks();
    });

    expect(findPrimaryTestButton().textContent).toBe('✓ 3 tools');

    await act(async () => {
      vi.advanceTimersByTime(2500);
      await flushMicrotasks();
    });

    expect(findPrimaryTestButton().textContent).toBe('✓ 3 tools');

    await act(async () => {
      vi.advanceTimersByTime(1500);
      await flushMicrotasks();
    });

    expect(findPrimaryTestButton().textContent).toBe('Test');
  });
});

describe('SettingsPage sub-agent model overrides', () => {
  let root: Root | null = null;

  beforeEach(() => {
    (
      globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }
    ).IS_REACT_ACT_ENVIRONMENT = true;
  });

  afterEach(async () => {
    if (root) {
      await act(async () => {
        root?.unmount();
        await flushMicrotasks();
      });
      root = null;
    }
    document.body.innerHTML = '';
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean })
      .IS_REACT_ACT_ENVIRONMENT;
    vi.unstubAllGlobals();
  });

  it('saves per-sub-agent overrides using discovered agent names', async () => {
    let savedBody: unknown;
    const fetchMock = vi.fn<typeof fetch>((input, init) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config' && (!init || !('method' in init) || !init.method)) {
        return Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: {
              models: {
                providers: {
                  openai: {
                    api: 'openai-completions',
                    baseUrl: 'https://api.openai.com/v1',
                    apiKey: 'sk-test',
                    models: [{ id: 'gpt-4o-mini', input: ['text'] }],
                  },
                },
              },
              agents: {
                defaults: {
                  model: {
                    primary: 'openai/gpt-4o-mini',
                    'sub-agent': 'openai/gpt-4o-mini',
                  },
                },
              },
            },
            discoveredAgents: [{ name: 'reviewer', source: 'system' }],
          }),
        );
      }
      if (url === '/api/config' && init?.method === 'PUT') {
        savedBody = JSON.parse(String(init.body || '{}'));
        return Promise.resolve(jsonResponse({ ok: true }));
      }
      throw new Error(`Unexpected fetch URL: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);

    ({ root } = await renderSettingsPage());
    await openAndLoad();

    await act(async () => {
      findButtonByText('Agents').click();
      await flushMicrotasks();
    });

    await act(async () => {
      findButtonByText('+ Add Sub-Agent Override').click();
      await flushMicrotasks();
    });

    await act(async () => {
      const save = document.getElementById('settings-save-btn');
      if (!(save instanceof HTMLButtonElement)) throw new Error('Save button not found');
      save.click();
      await flushMicrotasks();
    });

    const savedConfig = (savedBody as { config?: { agents?: { defaults?: { model?: Record<string, string> } } } })
      ?.config;
    expect(savedConfig?.agents?.defaults?.model?.['sub-agent-reviewer']).toBe('openai/gpt-4o-mini');
  });
});
