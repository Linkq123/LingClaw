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

async function openAndLoad(sessionId?: string): Promise<void> {
  await act(async () => {
    openSettingsPage(sessionId);
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

function findSkillCheckbox(skillId: string): HTMLInputElement {
  const row = Array.from(document.querySelectorAll('.skill-row')).find((node) =>
    node.textContent?.includes(skillId),
  );
  const input = row?.querySelector('input[type="checkbox"]');
  if (!(input instanceof HTMLInputElement)) {
    throw new Error(`Skill checkbox not found: ${skillId}`);
  }
  return input;
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

    const savedConfig = (
      savedBody as { config?: { agents?: { defaults?: { model?: Record<string, string> } } } }
    )?.config;
    expect(savedConfig?.agents?.defaults?.model?.['sub-agent-reviewer']).toBe('openai/gpt-4o-mini');
  });
});

describe('SettingsPage session skills', () => {
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

  it('loads current-session skills and saves enabled system skill ids', async () => {
    let savedBody: unknown;
    const fetchMock = vi.fn<typeof fetch>((input, init) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config') {
        return Promise.resolve(jsonResponse({ path: '/tmp/config.json', config: {} }));
      }
      if (url === '/api/session-skills?session=research') {
        if (init?.method === 'PUT') {
          savedBody = JSON.parse(String(init.body || '{}'));
          return Promise.resolve(
            jsonResponse({
              ok: true,
              session: { id: 'research', name: 'Research' },
              skills: [
                {
                  id: 'anthropics/pdf',
                  name: 'pdf',
                  path: 'system://skills/anthropics/pdf/SKILL.md',
                  enabled: true,
                },
                {
                  id: 'anthropics/xlsx',
                  name: 'xlsx',
                  path: 'system://skills/anthropics/xlsx/SKILL.md',
                  enabled: true,
                },
              ],
              disabledSystemSkills: [],
            }),
          );
        }
        return Promise.resolve(
          jsonResponse({
            session: { id: 'research', name: 'Research' },
            skills: [
              {
                id: 'anthropics/pdf',
                name: 'pdf',
                description: 'Read PDFs',
                path: 'system://skills/anthropics/pdf/SKILL.md',
                group: 'anthropics',
                enabled: true,
              },
              {
                id: 'anthropics/xlsx',
                name: 'xlsx',
                path: 'system://skills/anthropics/xlsx/SKILL.md',
                group: 'anthropics',
                enabled: false,
              },
            ],
            disabledSystemSkills: ['anthropics/xlsx'],
          }),
        );
      }
      throw new Error(`Unexpected fetch URL: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);

    ({ root } = await renderSettingsPage());
    await openAndLoad('research');

    await act(async () => {
      findButtonByText('Skills').click();
      await flushMicrotasks();
    });

    expect(document.getElementById('settings-save-btn')).toBeNull();
    expect(document.body.textContent).toContain('Session:');
    expect(findSkillCheckbox('anthropics/pdf').checked).toBe(true);
    expect(findSkillCheckbox('anthropics/xlsx').checked).toBe(false);

    await act(async () => {
      findButtonByText('Disable all').click();
      await flushMicrotasks();
    });
    expect(findSkillCheckbox('anthropics/pdf').checked).toBe(false);
    expect(findSkillCheckbox('anthropics/xlsx').checked).toBe(false);

    await act(async () => {
      findButtonByText('Revert').click();
      await flushMicrotasks();
    });
    expect(findSkillCheckbox('anthropics/pdf').checked).toBe(true);
    expect(findSkillCheckbox('anthropics/xlsx').checked).toBe(false);

    await act(async () => {
      findButtonByText('Enable all').click();
      await flushMicrotasks();
    });
    expect(findSkillCheckbox('anthropics/pdf').checked).toBe(true);
    expect(findSkillCheckbox('anthropics/xlsx').checked).toBe(true);

    await act(async () => {
      findButtonByText('Save Skills').click();
      await flushMicrotasks();
    });

    expect(savedBody).toEqual({
      enabledSystemSkills: ['anthropics/pdf', 'anthropics/xlsx'],
      knownSystemSkills: ['anthropics/pdf', 'anthropics/xlsx'],
    });
  });

  it('shows session skill load errors', async () => {
    const fetchMock = vi.fn<typeof fetch>((input) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config') {
        return Promise.resolve(jsonResponse({ path: '/tmp/config.json', config: {} }));
      }
      if (url === '/api/session-skills?session=research') {
        return Promise.resolve(
          new Response(JSON.stringify({ error: 'Session not found' }), {
            status: 404,
            headers: { 'Content-Type': 'application/json' },
          }),
        );
      }
      throw new Error(`Unexpected fetch URL: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);

    ({ root } = await renderSettingsPage());
    await openAndLoad('research');

    await act(async () => {
      findButtonByText('Skills').click();
      await flushMicrotasks();
    });

    expect(document.body.textContent).toContain('Load failed: Session not found');
  });
});

describe('SettingsPage model compat thinking format', () => {
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

  it('shows compat.thinkingFormat in the Models tab', async () => {
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
                    baseUrl: 'https://gateway.example/v1',
                    apiKey: 'sk-test',
                    models: [
                      {
                        id: 'gpt-5.4',
                        input: ['text'],
                        compat: { thinkingFormat: 'qwen' },
                      },
                    ],
                  },
                },
              },
            },
          }),
        );
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
      const input = document.querySelector('input[aria-label="Thinking Format"]');
      if (!(input instanceof HTMLInputElement)) throw new Error('Thinking Format input not found');
      expect(input.value).toBe('qwen');
    });
  });

  it('loads openai-responses providers in the API type selector', async () => {
    const fetchMock = vi.fn<typeof fetch>((input, init) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config' && (!init || !('method' in init) || !init.method)) {
        return Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: {
              models: {
                providers: {
                  openaiResponses: {
                    api: 'openai-responses',
                    baseUrl: 'https://api.openai.com/v1',
                    apiKey: 'sk-test',
                    models: [{ id: 'gpt-5.5', input: ['text'] }],
                  },
                },
              },
            },
          }),
        );
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
      const select = document.querySelector('.provider-form select');
      if (!(select instanceof HTMLSelectElement)) throw new Error('API type select not found');
      expect(select.value).toBe('openai-responses');
      expect(Array.from(select.options).some((option) => option.value === 'openai-responses')).toBe(
        true,
      );
    });
  });
});
