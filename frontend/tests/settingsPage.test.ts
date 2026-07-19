import React from 'react';
import { act } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createRoot, type Root } from 'react-dom/client';

import {
  SettingsPage,
  closeSettingsPage,
  openSettingsPage,
  type SettingsSection,
} from '../src/pages/SettingsPage.js';
import {
  CONFIG_SAVED_EVENT,
  acceptComposerSocketModelPayloadRevision,
  beginComposerRevisionHandshake,
} from '../src/composerAvailability.js';
import { setLanguage } from '../src/i18n.js';
import { state } from '../src/state.js';

beforeEach(() => {
  setLanguage('en');
  state.composerConfigRevision = null;
  state.composerSessionModelRevision = null;
  state.composerGroupModelRevision = null;
});

function jsonResponse(payload: unknown, status = 200): Response {
  return new Response(JSON.stringify(payload), {
    status,
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

async function openAndLoad(sessionId?: string, initialSection?: SettingsSection): Promise<void> {
  await act(async () => {
    openSettingsPage(sessionId, initialSection);
    await flushMicrotasks();
  });
}

function findButtonByText(text: string): HTMLButtonElement {
  const button = Array.from(document.querySelectorAll('button')).find(
    (node) =>
      (node.querySelector('.settings-nav-label')?.textContent?.trim() ||
        node.textContent?.trim()) === text,
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

function findCloseButton(): HTMLButtonElement {
  const button = document.querySelector('button.console-return-button');
  if (!(button instanceof HTMLButtonElement)) {
    throw new Error('Console return button not found');
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

function findCheckboxByLabel(text: string): HTMLInputElement {
  const label = Array.from(document.querySelectorAll('label')).find((node) =>
    node.textContent?.includes(text),
  );
  const input = label?.querySelector('input[type="checkbox"]');
  if (!(input instanceof HTMLInputElement)) {
    throw new Error(`Checkbox not found: ${text}`);
  }
  return input;
}

function findMcpServerEnabledCheckbox(serverName: string): HTMLInputElement {
  const card = Array.from(document.querySelectorAll('.provider-card')).find((node) =>
    node.textContent?.includes(`${serverName} ·`),
  );
  const label = Array.from(card?.querySelectorAll('label') || []).find((node) =>
    node.textContent?.includes('Enabled for session'),
  );
  const input = label?.querySelector('input[type="checkbox"]');
  if (!(input instanceof HTMLInputElement)) {
    throw new Error(`MCP server checkbox not found: ${serverName}`);
  }
  return input;
}

function findInputByPlaceholder(placeholder: string): HTMLInputElement {
  const input = document.querySelector(`input[placeholder="${placeholder}"]`);
  if (!(input instanceof HTMLInputElement)) {
    throw new Error(`Input not found: ${placeholder}`);
  }
  return input;
}

function findSelectBySettingsLabel(text: string): HTMLSelectElement {
  const row = Array.from(document.querySelectorAll('.settings-row')).find((node) =>
    node.querySelector('label')?.textContent?.includes(text),
  );
  const select = row?.querySelector('select');
  if (!(select instanceof HTMLSelectElement)) {
    throw new Error(`Select not found: ${text}`);
  }
  return select;
}

function setInputValue(input: HTMLInputElement, value: string): void {
  const valueSetter = Object.getOwnPropertyDescriptor(
    window.HTMLInputElement.prototype,
    'value',
  )?.set;
  valueSetter?.call(input, value);
  input.dispatchEvent(new Event('input', { bubbles: true }));
}

function setTextareaValue(textarea: HTMLTextAreaElement, value: string): void {
  const valueSetter = Object.getOwnPropertyDescriptor(
    window.HTMLTextAreaElement.prototype,
    'value',
  )?.set;
  valueSetter?.call(textarea, value);
  textarea.dispatchEvent(new Event('input', { bubbles: true }));
}

function deferred<T>(): {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (error: unknown) => void;
} {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function setSelectValue(select: HTMLSelectElement, value: string): void {
  const valueSetter = Object.getOwnPropertyDescriptor(
    window.HTMLSelectElement.prototype,
    'value',
  )?.set;
  valueSetter?.call(select, value);
  select.dispatchEvent(new Event('change', { bubbles: true }));
}

describe('SettingsPage shell layout and dirty state', () => {
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
    delete (document as Document & { startViewTransition?: unknown }).startViewTransition;
  });

  it('keeps the Console rendered when an open request arrives before the lazy root mounts', async () => {
    vi.stubGlobal('matchMedia', vi.fn().mockReturnValue({ matches: true }));
    vi.stubGlobal(
      'fetch',
      vi.fn<typeof fetch>(() =>
        Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: {},
            discoveredAgents: [],
          }),
        ),
      ),
    );
    document.body.innerHTML = `
      <div id="app-workspace">
        <textarea id="input"></textarea>
        <button id="console-opener">Open</button>
      </div>
      <section id="console-page" hidden><div id="settings-page"></div></section>
    `;

    const opener = document.getElementById('console-opener') as HTMLButtonElement;
    opener.focus();
    openSettingsPage('main', 'tab-models');
    const container = document.getElementById('settings-page') as HTMLDivElement;
    root = createRoot(container);
    await act(async () => {
      root?.render(React.createElement(SettingsPage));
      await flushMicrotasks();
    });

    expect(document.getElementById('console-page')?.hidden).toBe(false);
    expect(document.querySelector('.console-page-surface')).not.toBeNull();
    expect(document.getElementById('tab-models-panel')?.hasAttribute('hidden')).toBe(false);

    await act(async () => {
      findCloseButton().click();
      await flushMicrotasks();
    });
    expect(document.activeElement).toBe(opener);
  });

  it('renders left-navigation tabs and saves dirty config from the settings action bar', async () => {
    let savedBody: unknown;
    const fetchMock = vi.fn<typeof fetch>((input, init) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config' && (!init || !('method' in init) || !init.method)) {
        return Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: { settings: { port: 18989 } },
            configFileEtag: 'a'.repeat(64),
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

    const tabs = Array.from(document.querySelectorAll('[role="tab"]'));
    expect(document.querySelector('[role="tablist"]')).not.toBeNull();
    expect(tabs.map((tab) => tab.textContent?.trim()).join(' ')).toContain('General');
    expect(tabs).toHaveLength(7);
    expect(tabs.map((tab) => tab.textContent?.trim()).join(' ')).toContain('Usage');
    expect(findCloseButton().getAttribute('aria-label')).toBe('Back to workspace');
    expect(
      tabs.every(
        (tab) =>
          tab.getAttribute('aria-label') === tab.querySelector('.settings-nav-label')?.textContent,
      ),
    ).toBe(true);

    await act(async () => {
      findButtonByText('S3').click();
      await flushMicrotasks();
    });
    expect(document.getElementById('tab-s3-panel')?.hasAttribute('hidden')).toBe(false);

    await act(async () => {
      findButtonByText('General').click();
      await flushMicrotasks();
    });

    const save = document.getElementById('settings-save-btn');
    if (!(save instanceof HTMLButtonElement)) throw new Error('Save button not found');
    expect(save.disabled).toBe(true);

    await act(async () => {
      const input = findInputByPlaceholder('18989');
      setInputValue(input, '19000');
      await flushMicrotasks();
    });
    expect(save.disabled).toBe(false);
    expect(document.body.textContent).toContain('You have unsaved changes.');

    await act(async () => {
      save.click();
      await flushMicrotasks();
    });

    expect(savedBody).toEqual({
      config: { settings: { port: 19000 } },
      baseConfigFileEtag: 'a'.repeat(64),
    });
    expect(save.disabled).toBe(true);
  });

  it('keeps an unsaved Settings draft mounted while visiting Usage', async () => {
    const fetchMock = vi.fn<typeof fetch>((input, init) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config' && (!init || !('method' in init) || !init.method)) {
        return Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: { settings: { port: 18989 } },
            configFileEtag: 'b'.repeat(64),
          }),
        );
      }
      if (url === '/api/usage') {
        return Promise.resolve(
          jsonResponse({
            daily_input: 0,
            daily_output: 0,
            total_input: 0,
            total_output: 0,
            usage_history: [],
          }),
        );
      }
      throw new Error(`Unexpected fetch URL: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);

    ({ root } = await renderSettingsPage());
    await openAndLoad();

    await act(async () => {
      setInputValue(findInputByPlaceholder('18989'), '19000');
      await flushMicrotasks();
    });

    await act(async () => {
      findButtonByText('Token Usage').click();
      await flushMicrotasks();
    });
    expect(document.getElementById('tab-usage-panel')?.hasAttribute('hidden')).toBe(false);
    expect(document.querySelector('[role="dialog"][aria-modal="true"]')).toBeNull();

    await act(async () => {
      findButtonByText('General').click();
      await flushMicrotasks();
    });

    expect(findInputByPlaceholder('18989').value).toBe('19000');
    expect((document.getElementById('settings-save-btn') as HTMLButtonElement).disabled).toBe(
      false,
    );
    expect(document.body.textContent).toContain('You have unsaved changes.');
  });

  it('does not revive discarded child drafts when the Console is reopened mid-transition', async () => {
    const transitions: Array<ReturnType<typeof deferred<void>>> = [];
    (
      document as Document & {
        startViewTransition: (update: () => void) => {
          updateCallbackDone: Promise<void>;
          finished: Promise<void>;
        };
      }
    ).startViewTransition = (update) => {
      update();
      const transition = deferred<void>();
      transitions.push(transition);
      return { updateCallbackDone: Promise.resolve(), finished: transition.promise };
    };
    const fetchMock = vi.fn<typeof fetch>((input) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config') {
        return Promise.resolve(jsonResponse({ path: '/tmp/config.json', config: {} }));
      }
      if (url === '/api/session-skills?session=main') {
        return Promise.resolve(
          jsonResponse({
            session: { id: 'main', name: 'Main' },
            skills: [
              {
                id: 'reviewer',
                name: 'Reviewer',
                path: '/tmp/reviewer/SKILL.md',
                enabled: true,
              },
            ],
          }),
        );
      }
      throw new Error(`Unexpected fetch URL: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);
    document.body.innerHTML = `
      <div id="app-workspace"><button id="console-opener">Open</button></div>
      <div id="workspace-portal-root"></div>
      <section id="console-page" hidden><div id="settings-page"></div></section>
    `;
    const container = document.getElementById('settings-page') as HTMLDivElement;
    root = createRoot(container);
    await act(async () => {
      root?.render(React.createElement(SettingsPage));
      await flushMicrotasks();
      openSettingsPage('main', 'tab-skills');
      await flushMicrotasks();
    });

    await act(async () => {
      findSkillCheckbox('reviewer').click();
      await flushMicrotasks();
    });
    expect(findSkillCheckbox('reviewer').checked).toBe(false);
    expect(document.body.textContent).toContain('You have unsaved changes.');

    await act(async () => {
      findCloseButton().click();
      await flushMicrotasks();
    });
    const discard = Array.from(document.querySelectorAll<HTMLButtonElement>('button')).find(
      (button) => button.textContent?.trim() === 'Discard Changes',
    );
    if (!discard) throw new Error('Discard button not found');

    await act(async () => {
      discard.click();
      await flushMicrotasks();
      openSettingsPage('main', 'tab-skills');
      await flushMicrotasks();
    });

    expect(findSkillCheckbox('reviewer').checked).toBe(true);
    expect(
      fetchMock.mock.calls.filter(([input]) => {
        const url = typeof input === 'string' ? input : input.url;
        return url === '/api/session-skills?session=main';
      }),
    ).toHaveLength(2);

    await act(async () => {
      for (const transition of transitions) transition.resolve(undefined);
      await flushMicrotasks();
    });
  });

  it('does not remount previously visited panels on a later Console visit', async () => {
    vi.stubGlobal('matchMedia', vi.fn().mockReturnValue({ matches: true }));
    const fetchMock = vi.fn<typeof fetch>((input) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config') {
        return Promise.resolve(jsonResponse({ path: '/tmp/config.json', config: {} }));
      }
      if (url === '/api/session-skills?session=main') {
        return Promise.resolve(
          jsonResponse({
            session: { id: 'main', name: 'Main' },
            skills: [],
          }),
        );
      }
      throw new Error(`Unexpected fetch URL: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);
    document.body.innerHTML = `
      <div id="app-workspace"><button id="console-opener">Open</button></div>
      <div id="workspace-portal-root"></div>
      <section id="console-page" hidden><div id="settings-page"></div></section>
    `;
    const container = document.getElementById('settings-page') as HTMLDivElement;
    root = createRoot(container);
    await act(async () => {
      root?.render(React.createElement(SettingsPage));
      await flushMicrotasks();
      openSettingsPage('main', 'tab-skills');
      await flushMicrotasks();
    });

    expect(document.getElementById('tab-skills-panel')).not.toBeNull();
    expect(
      fetchMock.mock.calls.filter(([input]) => {
        const url = typeof input === 'string' ? input : input.url;
        return url === '/api/session-skills?session=main';
      }),
    ).toHaveLength(1);

    await act(async () => {
      findCloseButton().click();
      await flushMicrotasks();
      openSettingsPage('main', 'tab-general');
      await flushMicrotasks();
    });

    expect(document.getElementById('tab-general-panel')).not.toBeNull();
    expect(document.getElementById('tab-skills-panel')).toBeNull();
    expect(
      fetchMock.mock.calls.filter(([input]) => {
        const url = typeof input === 'string' ? input : input.url;
        return url === '/api/session-skills?session=main';
      }),
    ).toHaveLength(1);
  });

  it('returns to and focuses the composer after inserting an MCP resource', async () => {
    vi.stubGlobal('matchMedia', vi.fn().mockReturnValue({ matches: true }));
    const fetchMock = vi.fn<typeof fetch>((input, init) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config') {
        return Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: {
              mcpServers: {
                docs: {
                  transport: 'streamable-http',
                  url: 'https://docs.example/mcp',
                  enabled: true,
                },
              },
            },
          }),
        );
      }
      if (url === '/api/mcp/catalog?session=main') {
        return Promise.resolve(
          jsonResponse({
            session: { id: 'main', name: 'Main' },
            policy: {
              enabledServers: ['docs'],
              enabledTools: [],
              confirmMutatingTools: false,
              clientCapabilities: {},
            },
            servers: [
              {
                id: 'docs',
                name: 'docs',
                transport: 'streamable-http',
                configuredEnabled: true,
                enabled: true,
                authenticated: false,
                toolCount: 0,
                resourceCount: 1,
                promptCount: 0,
              },
            ],
            tools: [],
            resources: [{ server: 'docs', uri: 'docs://guide', name: 'Guide' }],
            prompts: [],
          }),
        );
      }
      if (url === '/api/mcp/resource/read?session=main' && init?.method === 'POST') {
        return Promise.resolve(jsonResponse({ ok: true, result: { text: 'Guide content' } }));
      }
      throw new Error(`Unexpected fetch URL: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);
    document.body.innerHTML = `
      <div id="app-workspace">
        <button id="console-opener">Open</button>
        <textarea id="input"></textarea>
      </div>
      <div id="workspace-portal-root"></div>
      <section id="console-page" hidden><div id="settings-page"></div></section>
    `;
    const composer = document.getElementById('input') as HTMLTextAreaElement;
    const container = document.getElementById('settings-page') as HTMLDivElement;
    root = createRoot(container);
    await act(async () => {
      root?.render(React.createElement(SettingsPage));
      await flushMicrotasks();
      openSettingsPage('main', 'tab-mcp');
      await flushMicrotasks();
    });

    await act(async () => {
      findButtonByText('Read').click();
      await flushMicrotasks();
    });

    expect(composer.value).toContain('Guide content');
    expect(document.getElementById('app-workspace')?.hidden).toBe(false);
    expect(document.getElementById('console-page')?.hidden).toBe(true);
    expect(document.activeElement).toBe(composer);
  });

  it('opens a requested section and keeps the mobile section picker synchronized', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn<typeof fetch>(() =>
        Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: {},
            discoveredAgents: [],
          }),
        ),
      ),
    );

    ({ root } = await renderSettingsPage());
    await openAndLoad('main', 'tab-models');

    const picker = document.querySelector<HTMLSelectElement>(
      '.settings-mobile-section-picker select',
    );
    expect(picker?.value).toBe('tab-models');
    expect(document.getElementById('tab-models-panel')?.hasAttribute('hidden')).toBe(false);

    await act(async () => {
      if (!picker) throw new Error('Mobile settings picker not found');
      setSelectValue(picker, 'tab-agents');
      await flushMicrotasks();
    });

    expect(document.getElementById('tab-agents-panel')?.hasAttribute('hidden')).toBe(false);
    expect(
      document.querySelector('[role="tab"][aria-selected="true"]')?.getAttribute('data-tab'),
    ).toBe('tab-agents');
  });

  it('preserves edits made while a config save is in flight', async () => {
    const pendingSave = deferred<Response>();
    let savedBody: unknown;
    let getCount = 0;
    const fetchMock = vi.fn<typeof fetch>((input, init) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config' && init?.method === 'PUT') {
        savedBody = JSON.parse(String(init.body || '{}'));
        return pendingSave.promise;
      }
      if (url === '/api/config') {
        getCount += 1;
        return Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: { settings: { port: getCount === 1 ? 18989 : 19000 } },
            configRevision: getCount === 1 ? 10 : 12,
            configFileEtag: (getCount === 1 ? 'a' : 'b').repeat(64),
          }),
        );
      }
      throw new Error(`Unexpected fetch URL: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);

    ({ root } = await renderSettingsPage());
    await openAndLoad();
    const save = document.getElementById('settings-save-btn');
    if (!(save instanceof HTMLButtonElement)) throw new Error('Save button not found');

    await act(async () => {
      setInputValue(findInputByPlaceholder('18989'), '19000');
      await flushMicrotasks();
      save.click();
      await flushMicrotasks();
    });
    await act(async () => {
      setInputValue(findInputByPlaceholder('18989'), '19001');
      await flushMicrotasks();
    });
    await act(async () => {
      // A concurrent /model update advances only the model-status revision;
      // the successful file save must still keep the newer local edit.
      state.composerConfigRevision = 12;
      pendingSave.resolve(
        jsonResponse({
          ok: true,
          configRevision: 11,
          configFileEtag: 'b'.repeat(64),
        }),
      );
      await pendingSave.promise;
      await flushMicrotasks();
    });

    expect(savedBody).toMatchObject({ baseConfigFileEtag: 'a'.repeat(64) });
    expect(getCount).toBe(2);
    expect(findInputByPlaceholder('18989').value).toBe('19001');
    expect(save.disabled).toBe(false);
    expect(document.body.textContent).toContain('You have unsaved changes.');
  });

  it('does not discard a blank model draft created while another config save is in flight', async () => {
    const pendingSave = deferred<Response>();
    const fetchMock = vi.fn<typeof fetch>((input, init) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config' && init?.method === 'PUT') return pendingSave.promise;
      if (url === '/api/config') {
        return Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: {
              settings: { port: 18989 },
              models: {
                providers: {
                  gateway: {
                    api: 'openai-completions',
                    models: [{ id: 'model-one', input: ['text'] }],
                  },
                },
              },
            },
            configFileEtag: 'a'.repeat(64),
          }),
        );
      }
      throw new Error(`Unexpected fetch URL: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);

    ({ root } = await renderSettingsPage());
    await openAndLoad();
    const save = document.getElementById('settings-save-btn');
    if (!(save instanceof HTMLButtonElement)) throw new Error('Save button not found');

    await act(async () => {
      setInputValue(findInputByPlaceholder('18989'), '19000');
      await flushMicrotasks();
      save.click();
      await flushMicrotasks();
    });
    await act(async () => {
      findButtonByText('Models').click();
      await flushMicrotasks();
      const addModel = Array.from(document.querySelectorAll('button')).find(
        (button) => button.textContent?.trim() === 'Add model',
      );
      if (!(addModel instanceof HTMLButtonElement)) throw new Error('Add model button not found');
      addModel.click();
      await flushMicrotasks();
    });
    expect(document.querySelectorAll('.models-console-card')).toHaveLength(2);

    await act(async () => {
      pendingSave.resolve(jsonResponse({ ok: true, configFileEtag: 'b'.repeat(64) }));
      await pendingSave.promise;
      await flushMicrotasks();
    });

    expect(document.querySelectorAll('.models-console-card')).toHaveLength(2);
    await act(async () => {
      findCloseButton().click();
      await flushMicrotasks();
    });
    expect(document.querySelector('.settings-discard-dialog')).not.toBeNull();
  });

  it('keeps an internal model draft when a rejected save revision requires a newer readback', async () => {
    const pendingSave = deferred<Response>();
    let getCount = 0;
    const fetchMock = vi.fn<typeof fetch>((input, init) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config' && init?.method === 'PUT') {
        return pendingSave.promise;
      }
      if (url === '/api/config') {
        getCount += 1;
        return Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: {
              settings: { port: getCount === 1 ? 18989 : 19191 },
              models: {
                providers: {
                  gateway: {
                    api: 'openai-completions',
                    models: [{ id: 'model-one', input: ['text'] }],
                  },
                },
              },
            },
            configRevision: getCount === 1 ? 10 : 12,
            configFileEtag: (getCount === 1 ? 'a' : 'c').repeat(64),
          }),
        );
      }
      throw new Error(`Unexpected fetch URL: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);

    ({ root } = await renderSettingsPage());
    await openAndLoad();
    const save = document.getElementById('settings-save-btn');
    if (!(save instanceof HTMLButtonElement)) throw new Error('Save button not found');
    await act(async () => {
      setInputValue(findInputByPlaceholder('18989'), '19000');
      await flushMicrotasks();
      save.click();
      await flushMicrotasks();
    });
    await act(async () => {
      findButtonByText('Models').click();
      await flushMicrotasks();
    });
    const addModel = Array.from(document.querySelectorAll('button')).find(
      (button) => button.textContent?.trim() === 'Add model',
    );
    if (!(addModel instanceof HTMLButtonElement)) throw new Error('Add model button not found');

    await act(async () => {
      addModel.click();
      await flushMicrotasks();
    });
    expect(document.querySelectorAll('.models-console-card')).toHaveLength(2);

    await act(async () => {
      pendingSave.resolve(
        jsonResponse({
          ok: true,
          configRevision: 9,
          configFileEtag: 'b'.repeat(64),
        }),
      );
      await pendingSave.promise;
      await flushMicrotasks();
    });
    await vi.waitFor(() => expect(getCount).toBe(2));

    expect(document.querySelectorAll('.models-console-card')).toHaveLength(2);
    expect(document.body.textContent).toContain('Configuration changed elsewhere.');
    expect(save.disabled).toBe(true);
  });

  it('does not overwrite config edits made while the initial load is in flight', async () => {
    const initialLoad = deferred<Response>();
    let getCount = 0;
    const fetchMock = vi.fn<typeof fetch>((input) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url !== '/api/config') throw new Error(`Unexpected fetch URL: ${url}`);
      getCount += 1;
      if (getCount === 1) return initialLoad.promise;
      return Promise.resolve(
        jsonResponse({
          path: '/tmp/config.json',
          config: { settings: { port: 19191 } },
          configRevision: 21,
          configFileEtag: 'b'.repeat(64),
        }),
      );
    });
    vi.stubGlobal('fetch', fetchMock);

    ({ root } = await renderSettingsPage());
    await openAndLoad();

    await act(async () => {
      setInputValue(findInputByPlaceholder('18989'), '19000');
      await flushMicrotasks();
    });
    await act(async () => {
      initialLoad.resolve(
        jsonResponse({
          path: '/tmp/config.json',
          config: { settings: { port: 18989 } },
          configRevision: 20,
          configFileEtag: 'a'.repeat(64),
        }),
      );
      await initialLoad.promise;
      await flushMicrotasks();
    });

    const save = document.getElementById('settings-save-btn');
    if (!(save instanceof HTMLButtonElement)) throw new Error('Save button not found');
    expect(findInputByPlaceholder('18989').value).toBe('19000');
    expect(save.disabled).toBe(true);
    expect(document.body.textContent).toContain('Configuration changed elsewhere.');

    const mobileReload = document.querySelector<HTMLButtonElement>('.settings-mobile-reload');
    expect(mobileReload?.textContent).toContain('Reload latest');

    await act(async () => {
      mobileReload?.click();
      await flushMicrotasks();
    });

    expect(getCount).toBe(2);
    expect(findInputByPlaceholder('18989').value).toBe('19191');
    expect(save.disabled).toBe(true);
  });

  it('keeps local edits on a config conflict and reloads only on request', async () => {
    let getCount = 0;
    const fetchMock = vi.fn<typeof fetch>((input, init) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config' && init?.method === 'PUT') {
        return Promise.resolve(
          jsonResponse(
            {
              error: 'Configuration changed',
              configRevision: 21,
              configFileEtag: 'b'.repeat(64),
            },
            409,
          ),
        );
      }
      if (url === '/api/config') {
        getCount += 1;
        return Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: { settings: { port: getCount === 1 ? 18989 : 19191 } },
            configRevision: getCount === 1 ? 20 : 21,
            configFileEtag: (getCount === 1 ? 'a' : 'b').repeat(64),
          }),
        );
      }
      throw new Error(`Unexpected fetch URL: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);

    ({ root } = await renderSettingsPage());
    await openAndLoad();
    const save = document.getElementById('settings-save-btn');
    if (!(save instanceof HTMLButtonElement)) throw new Error('Save button not found');

    await act(async () => {
      setInputValue(findInputByPlaceholder('18989'), '19000');
      await flushMicrotasks();
      save.click();
      await flushMicrotasks();
    });

    expect(findInputByPlaceholder('18989').value).toBe('19000');
    expect(save.disabled).toBe(true);
    expect(document.body.textContent).toContain('Configuration changed elsewhere.');

    await act(async () => {
      findButtonByText('Reload latest').click();
      await flushMicrotasks();
    });

    expect(getCount).toBe(2);
    expect(findInputByPlaceholder('18989').value).toBe('19191');
    expect(save.disabled).toBe(true);
  });

  it('lets the corrupt-config editor reload its ETag after a save conflict', async () => {
    let getCount = 0;
    const putBodies: Array<{ baseConfigFileEtag?: string }> = [];
    const fetchMock = vi.fn<typeof fetch>((input, init) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config' && init?.method === 'PUT') {
        putBodies.push(JSON.parse(String(init.body || '{}')));
        if (putBodies.length === 1) {
          return Promise.resolve(
            jsonResponse(
              {
                error: 'Configuration changed',
                configRevision: 21,
                configFileEtag: 'b'.repeat(64),
              },
              409,
            ),
          );
        }
        return Promise.resolve(jsonResponse({ error: 'Stop after request inspection' }, 400));
      }
      if (url === '/api/config') {
        getCount += 1;
        return Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            parse_error: 'Unexpected end of JSON input',
            raw: getCount === 1 ? '{"broken":' : '{"newer":',
            configRevision: getCount === 1 ? 20 : 21,
            configFileEtag: (getCount === 1 ? 'a' : 'b').repeat(64),
          }),
        );
      }
      throw new Error(`Unexpected fetch URL: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);

    ({ root } = await renderSettingsPage());
    await openAndLoad();
    const editor = document.querySelector('.json-editor');
    if (!(editor instanceof HTMLTextAreaElement)) throw new Error('JSON editor not found');

    await act(async () => {
      setTextareaValue(editor, '{"settings":{"port":19000}}');
      findButtonByText('Save & Recover').click();
      await flushMicrotasks();
    });

    expect(putBodies[0]?.baseConfigFileEtag).toBe('a'.repeat(64));
    expect(editor.value).toBe('{"settings":{"port":19000}}');
    expect(document.body.textContent).toContain('Configuration changed elsewhere.');
    expect(findButtonByText('Save & Recover').disabled).toBe(true);

    await act(async () => {
      findButtonByText('Reload latest').click();
      await flushMicrotasks();
    });

    expect(getCount).toBe(2);
    const reloadedEditor = document.querySelector('.json-editor');
    if (!(reloadedEditor instanceof HTMLTextAreaElement)) {
      throw new Error('Reloaded JSON editor not found');
    }
    expect(reloadedEditor.value).toBe('{"newer":');

    await act(async () => {
      setTextareaValue(reloadedEditor, '{"settings":{"port":19191}}');
      findButtonByText('Save & Recover').click();
      await flushMicrotasks();
    });

    expect(putBodies[1]?.baseConfigFileEtag).toBe('b'.repeat(64));
  });

  it('reloads the newest config instead of applying a stale save response', async () => {
    let getCount = 0;
    const configEvents: Array<{
      config?: { settings?: { port?: number } };
      configRevision?: number;
    }> = [];
    const onConfig = (event: Event) => {
      configEvents.push(
        (
          event as CustomEvent<{
            config?: { settings?: { port?: number } };
            configRevision?: number;
          }>
        ).detail,
      );
    };
    window.addEventListener(CONFIG_SAVED_EVENT, onConfig);
    const fetchMock = vi.fn<typeof fetch>((input, init) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config' && init?.method === 'PUT') {
        return Promise.resolve(jsonResponse({ ok: true, configRevision: 19 }));
      }
      if (url === '/api/config') {
        getCount += 1;
        return Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: { settings: { port: getCount === 1 ? 18989 : 19191 } },
            configRevision: 20,
          }),
        );
      }
      throw new Error(`Unexpected fetch URL: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);

    try {
      ({ root } = await renderSettingsPage());
      await openAndLoad();
      const save = document.getElementById('settings-save-btn');
      if (!(save instanceof HTMLButtonElement)) throw new Error('Save button not found');

      await act(async () => {
        setInputValue(findInputByPlaceholder('18989'), '19000');
        await flushMicrotasks();
      });
      await act(async () => {
        save.click();
        await flushMicrotasks();
      });
      await vi.waitFor(() => expect(getCount).toBe(2));

      expect(findInputByPlaceholder('18989').value).toBe('19191');
      expect(save.disabled).toBe(true);
      expect(state.composerConfigRevision).toBe(20);
      expect(configEvents.at(-1)).toMatchObject({
        config: { settings: { port: 19191 } },
        configRevision: 20,
      });
    } finally {
      window.removeEventListener(CONFIG_SAVED_EVENT, onConfig);
    }
  });

  it('retries an older Settings GET and emits the accepted save revision', async () => {
    state.composerConfigRevision = 30;
    let getCount = 0;
    const configEvents: Array<{ configRevision?: number }> = [];
    const onConfig = (event: Event) => {
      configEvents.push((event as CustomEvent<{ configRevision?: number }>).detail);
    };
    window.addEventListener(CONFIG_SAVED_EVENT, onConfig);
    const fetchMock = vi.fn<typeof fetch>((input, init) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config' && init?.method === 'PUT') {
        return Promise.resolve(
          jsonResponse({
            ok: true,
            configRevision: 31,
            explicitPrimaryModelConfigured: true,
          }),
        );
      }
      if (url === '/api/config') {
        getCount += 1;
        return Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: { settings: { port: getCount === 1 ? 18000 : 19001 } },
            configRevision: getCount === 1 ? 29 : 30,
          }),
        );
      }
      throw new Error(`Unexpected fetch URL: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);

    try {
      ({ root } = await renderSettingsPage());
      await openAndLoad();
      expect(getCount).toBe(2);
      expect(findInputByPlaceholder('18989').value).toBe('19001');

      const save = document.getElementById('settings-save-btn');
      if (!(save instanceof HTMLButtonElement)) throw new Error('Save button not found');
      await act(async () => {
        setInputValue(findInputByPlaceholder('18989'), '19002');
        await flushMicrotasks();
      });
      await act(async () => {
        save.click();
        await flushMicrotasks();
      });

      expect(configEvents.at(-1)?.configRevision).toBe(31);
      expect(state.composerConfigRevision).toBe(31);
    } finally {
      window.removeEventListener(CONFIG_SAVED_EVENT, onConfig);
    }
  });

  it('discards a Settings GET response from the previous socket generation', async () => {
    state.composerConfigRevision = 100;
    const oldResponse = deferred<Response>();
    let getCount = 0;
    const fetchMock = vi.fn<typeof fetch>((input) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url !== '/api/config') throw new Error(`Unexpected fetch URL: ${url}`);
      getCount += 1;
      if (getCount === 1) return oldResponse.promise;
      return Promise.resolve(
        jsonResponse({
          path: '/tmp/config.json',
          config: { settings: { port: 19005 } },
          configRevision: 5,
          configFileEtag: 'b'.repeat(64),
        }),
      );
    });
    vi.stubGlobal('fetch', fetchMock);

    ({ root } = await renderSettingsPage());
    await openAndLoad();
    await act(async () => {
      beginComposerRevisionHandshake();
      expect(acceptComposerSocketModelPayloadRevision(5)).toBe(true);
      oldResponse.resolve(
        jsonResponse({
          path: '/tmp/old-config.json',
          config: { settings: { port: 19100 } },
          configRevision: 100,
          configFileEtag: 'a'.repeat(64),
        }),
      );
      await vi.waitFor(() => expect(getCount).toBeGreaterThanOrEqual(4));
      await flushMicrotasks();
    });

    expect(findInputByPlaceholder('18989').value).toBe('19005');
    expect(state.composerConfigRevision).toBe(5);
    expect(findInputByPlaceholder('18989').value).not.toBe('19100');
  });

  it('saves the Task Plan feature switch as enableTaskPlan', async () => {
    let savedBody: unknown;
    const fetchMock = vi.fn<typeof fetch>((input, init) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config' && (!init || !('method' in init) || !init.method)) {
        return Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: { settings: { enableTaskPlan: false } },
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

    const save = document.getElementById('settings-save-btn');
    if (!(save instanceof HTMLButtonElement)) throw new Error('Save button not found');
    expect(save.disabled).toBe(true);

    await act(async () => {
      setSelectValue(findSelectBySettingsLabel('Task Plan'), 'true');
      await flushMicrotasks();
    });

    expect(save.disabled).toBe(false);

    await act(async () => {
      save.click();
      await flushMicrotasks();
    });

    expect(savedBody).toEqual({ config: { settings: { enableTaskPlan: true } } });
  });

  it('does not prompt on tab changes but prompts before closing dirty settings', async () => {
    const fetchMock = vi.fn<typeof fetch>((input) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config') {
        return Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: { settings: { port: 18989 } },
          }),
        );
      }
      if (url === '/api/session-skills?session=main') {
        return Promise.resolve(
          jsonResponse({
            session: { id: 'main', name: 'main' },
            skills: [],
            disabledSystemSkills: [],
          }),
        );
      }
      throw new Error(`Unexpected fetch URL: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);

    const rendered = await renderSettingsPage();
    root = rendered.root;
    await openAndLoad();

    await act(async () => {
      const input = findInputByPlaceholder('18989');
      setInputValue(input, '19000');
      await flushMicrotasks();
    });

    await act(async () => {
      findButtonByText('Skills').click();
      await flushMicrotasks();
    });
    expect(document.body.textContent).not.toContain('Discard unsaved changes?');

    await act(async () => {
      findCloseButton().click();
      await flushMicrotasks();
    });
    expect(document.body.textContent).toContain('Discard unsaved changes?');
    expect(rendered.container.hidden).toBe(false);

    await act(async () => {
      findButtonByText('Keep Editing').click();
      await flushMicrotasks();
    });
    expect(document.body.textContent).not.toContain('Discard unsaved changes?');
    expect(rendered.container.hidden).toBe(false);

    await act(async () => {
      closeSettingsPage();
      await flushMicrotasks();
    });
    expect(document.body.textContent).toContain('Discard unsaved changes?');

    await act(async () => {
      findButtonByText('Discard Changes').click();
      await flushMicrotasks();
    });
    expect(rendered.container.hidden).toBe(true);

    await act(async () => {
      openSettingsPage();
      await flushMicrotasks();
    });
    expect(findInputByPlaceholder('18989').value).toBe('18989');
  });

  it('captures Escape for dirty settings instead of letting document handlers close other overlays', async () => {
    const fetchMock = vi.fn<typeof fetch>((input) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config') {
        return Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: { settings: { port: 18989 } },
          }),
        );
      }
      throw new Error(`Unexpected fetch URL: ${url}`);
    });
    const documentEscapeHandler = vi.fn();
    document.addEventListener('keydown', documentEscapeHandler);
    vi.stubGlobal('fetch', fetchMock);

    ({ root } = await renderSettingsPage());
    await openAndLoad();

    await act(async () => {
      const input = findInputByPlaceholder('18989');
      setInputValue(input, '19000');
      await flushMicrotasks();
    });

    await act(async () => {
      document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
      await flushMicrotasks();
    });

    expect(document.body.textContent).toContain('Discard unsaved changes?');
    expect(documentEscapeHandler).not.toHaveBeenCalled();
    document.removeEventListener('keydown', documentEscapeHandler);
  });

  it('does not switch the Skills session when reopening dirty settings for another session', async () => {
    const fetchMock = vi.fn<typeof fetch>((input) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config') {
        return Promise.resolve(jsonResponse({ path: '/tmp/config.json', config: {} }));
      }
      if (url === '/api/session-skills?session=alpha') {
        return Promise.resolve(
          jsonResponse({
            session: { id: 'alpha', name: 'Alpha' },
            skills: [
              {
                id: 'anthropics/pdf',
                name: 'pdf',
                path: 'system://skills/anthropics/pdf/SKILL.md',
                enabled: true,
              },
            ],
            disabledSystemSkills: [],
          }),
        );
      }
      if (url === '/api/session-skills?session=beta') {
        return Promise.resolve(
          jsonResponse({
            session: { id: 'beta', name: 'Beta' },
            skills: [],
            disabledSystemSkills: [],
          }),
        );
      }
      throw new Error(`Unexpected fetch URL: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);

    ({ root } = await renderSettingsPage());
    await openAndLoad('alpha');

    await act(async () => {
      findButtonByText('Skills').click();
      await flushMicrotasks();
    });

    await act(async () => {
      findSkillCheckbox('anthropics/pdf').click();
      await flushMicrotasks();
    });
    expect(findSkillCheckbox('anthropics/pdf').checked).toBe(false);

    await act(async () => {
      openSettingsPage('beta');
      await flushMicrotasks();
    });

    expect(document.body.textContent).toContain('Discard unsaved changes?');
    expect(document.body.textContent).toContain('Alpha');
    expect(findSkillCheckbox('anthropics/pdf').checked).toBe(false);
    expect(fetchMock).not.toHaveBeenCalledWith(
      '/api/session-skills?session=beta',
      expect.anything(),
    );
  });

  it('keeps skills dirty when a slow config load resolves after skill edits', async () => {
    const configRequest = deferred<Response>();
    const fetchMock = vi.fn<typeof fetch>((input) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config') {
        return configRequest.promise;
      }
      if (url === '/api/session-skills?session=main') {
        return Promise.resolve(
          jsonResponse({
            session: { id: 'main', name: 'main' },
            skills: [
              {
                id: 'anthropics/pdf',
                name: 'pdf',
                path: 'system://skills/anthropics/pdf/SKILL.md',
                enabled: true,
              },
            ],
            disabledSystemSkills: [],
          }),
        );
      }
      throw new Error(`Unexpected fetch URL: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);

    ({ root } = await renderSettingsPage());
    await openAndLoad();

    await act(async () => {
      findButtonByText('Skills').click();
      await flushMicrotasks();
    });
    await act(async () => {
      findSkillCheckbox('anthropics/pdf').click();
      await flushMicrotasks();
    });
    expect(findSkillCheckbox('anthropics/pdf').checked).toBe(false);

    await act(async () => {
      configRequest.resolve(jsonResponse({ path: '/tmp/config.json', config: {} }));
      await flushMicrotasks();
    });

    await act(async () => {
      findCloseButton().click();
      await flushMicrotasks();
    });
    expect(document.body.textContent).toContain('Discard unsaved changes?');
  });
});

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

    expect(findPrimaryTestButton().textContent).toBe('Connected');

    await act(async () => {
      vi.advanceTimersByTime(2000);
      await flushMicrotasks();
    });

    await act(async () => {
      findPrimaryTestButton().click();
      await flushMicrotasks();
    });

    expect(findPrimaryTestButton().textContent).toBe('Connected');

    await act(async () => {
      vi.advanceTimersByTime(2500);
      await flushMicrotasks();
    });

    expect(findPrimaryTestButton().textContent).toBe('Connected');

    await act(async () => {
      vi.advanceTimersByTime(1500);
      await flushMicrotasks();
    });

    expect(findPrimaryTestButton().textContent).toBe('Test');
  });

  it('keeps the latest MCP test result visible until its own reset timer fires', async () => {
    let testBody: Record<string, unknown> | undefined;
    const fetchMock = vi.fn<typeof fetch>((input, init) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config') {
        return Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: {
              mcpServers: {
                demo: {
                  command: 'uvx',
                  url: 'https://legacy.example/mcp',
                  args: ['server'],
                  env: { TOKEN: 'secret' },
                  auth: { clientId: 'client-id', scopes: ['repo'] },
                  enabled: true,
                },
              },
            },
          }),
        );
      }
      if (url === '/api/config/test-mcp?session=main') {
        testBody = JSON.parse(String(init?.body ?? '{}')) as Record<string, unknown>;
        return Promise.resolve(jsonResponse({ ok: true, tools: 3 }));
      }
      if (url === '/api/mcp/catalog?session=main') {
        return Promise.resolve(
          jsonResponse({
            session: { id: 'main', name: 'Main' },
            policy: { enabledServers: [], enabledTools: [] },
            servers: [],
            tools: [],
            resources: [],
            prompts: [],
          }),
        );
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

    expect(findPrimaryTestButton().textContent).toBe('3 tools');
    expect(testBody).toMatchObject({
      server: 'demo',
      transport: 'stdio',
      command: 'uvx',
      url: 'https://legacy.example/mcp',
      args: ['server'],
      env: { TOKEN: 'secret' },
      auth: { clientId: 'client-id', scopes: ['repo'] },
    });

    await act(async () => {
      vi.advanceTimersByTime(2000);
      await flushMicrotasks();
    });

    await act(async () => {
      findPrimaryTestButton().click();
      await flushMicrotasks();
    });

    expect(findPrimaryTestButton().textContent).toBe('3 tools');

    await act(async () => {
      vi.advanceTimersByTime(2500);
      await flushMicrotasks();
    });

    expect(findPrimaryTestButton().textContent).toBe('3 tools');

    await act(async () => {
      vi.advanceTimersByTime(1500);
      await flushMicrotasks();
    });

    expect(findPrimaryTestButton().textContent).toBe('Test');
  });
});

describe('SettingsPage MCP auth', () => {
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

  it('starts OAuth for streamable-http MCP servers from the catalog', async () => {
    let authStartBody: unknown;
    const popup = {
      closed: false,
      close: vi.fn(),
      location: { href: '' },
      opener: {},
    };
    const openMock = vi.fn(() => popup);
    vi.stubGlobal('open', openMock);
    const fetchMock = vi.fn<typeof fetch>((input, init) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config') {
        return Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: {
              mcpServers: {
                remote: {
                  transport: 'streamable-http',
                  url: 'https://mcp.example/mcp',
                  enabled: true,
                },
              },
            },
          }),
        );
      }
      if (url === '/api/mcp/catalog?session=main') {
        return Promise.resolve(
          jsonResponse({
            session: { id: 'main', name: 'Main' },
            policy: { enabledServers: [], enabledTools: [] },
            servers: [
              {
                id: 'remote',
                name: 'remote',
                transport: 'streamable-http',
                configuredEnabled: true,
                enabled: false,
                authenticated: false,
                toolCount: 0,
                resourceCount: 0,
                promptCount: 0,
              },
            ],
            tools: [],
            resources: [],
            prompts: [],
          }),
        );
      }
      if (url === '/api/mcp/auth/start') {
        authStartBody = JSON.parse(String(init?.body || '{}'));
        return Promise.resolve(
          jsonResponse({
            ok: true,
            authorizationUrl: 'https://auth.example/authorize',
          }),
        );
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
      findButtonByText('Connect').click();
      await flushMicrotasks();
    });

    expect(authStartBody).toEqual({ server: 'remote' });
    expect(openMock).toHaveBeenCalledWith('about:blank', '_blank');
    expect(popup.opener).toBeNull();
    expect(popup.location.href).toBe('https://auth.example/authorize');
  });

  it('disables OAuth connect for globally disabled MCP servers', async () => {
    const fetchMock = vi.fn<typeof fetch>((input) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config') {
        return Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: {
              mcpServers: {
                remote: {
                  transport: 'streamable-http',
                  url: 'https://mcp.example/mcp',
                  enabled: false,
                },
              },
            },
          }),
        );
      }
      if (url === '/api/mcp/catalog?session=main') {
        return Promise.resolve(
          jsonResponse({
            session: { id: 'main', name: 'Main' },
            policy: { enabledServers: [], enabledTools: [] },
            servers: [
              {
                id: 'remote',
                name: 'remote',
                transport: 'streamable-http',
                configuredEnabled: false,
                enabled: false,
                authenticated: false,
                toolCount: 0,
                resourceCount: 0,
                promptCount: 0,
              },
            ],
            tools: [],
            resources: [],
            prompts: [],
          }),
        );
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

    const connect = findButtonByText('Connect');
    expect(connect).toBeInstanceOf(HTMLButtonElement);
    expect((connect as HTMLButtonElement).disabled).toBe(true);
    expect(fetchMock).not.toHaveBeenCalledWith('/api/mcp/auth/start', expect.anything());
  });

  it('does not dirty config when the MCP tab renders omitted default fields', async () => {
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
                },
              },
            },
          }),
        );
      }
      if (url === '/api/mcp/catalog?session=main') {
        return Promise.resolve(
          jsonResponse({
            session: { id: 'main', name: 'Main' },
            policy: { enabledServers: [], enabledTools: [] },
            servers: [
              {
                id: 'demo',
                name: 'demo',
                transport: 'stdio',
                configuredEnabled: true,
                enabled: false,
                toolCount: 0,
                resourceCount: 0,
                promptCount: 0,
              },
            ],
            tools: [],
            resources: [],
            prompts: [],
          }),
        );
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

    const save = document.getElementById('settings-save-btn');
    if (!(save instanceof HTMLButtonElement)) throw new Error('Save button not found');
    expect(save.disabled).toBe(true);
    expect(document.body.textContent).toContain('No unsaved config changes.');
  });

  it('does not let a late MCP catalog refresh overwrite unsaved policy edits', async () => {
    const refreshRequest = deferred<Response>();
    let catalogCalls = 0;
    const fetchMock = vi.fn<typeof fetch>((input) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config') {
        return Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: {
              mcpServers: {
                remote: {
                  transport: 'streamable-http',
                  url: 'https://mcp.example/mcp',
                  enabled: true,
                },
              },
            },
          }),
        );
      }
      if (url === '/api/mcp/catalog?session=main') {
        catalogCalls += 1;
        if (catalogCalls === 2) return refreshRequest.promise;
        return Promise.resolve(
          jsonResponse({
            session: { id: 'main', name: 'Main' },
            policy: { enabledServers: [], enabledTools: [] },
            servers: [
              {
                id: 'remote',
                name: 'remote',
                transport: 'streamable-http',
                configuredEnabled: true,
                enabled: false,
                authenticated: false,
                toolCount: 1,
                resourceCount: 0,
                promptCount: 0,
              },
            ],
            tools: [],
            resources: [],
            prompts: [],
          }),
        );
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
    expect(findCheckboxByLabel('Enabled for session').checked).toBe(false);

    await act(async () => {
      findButtonByText('Refresh Catalog').click();
      await flushMicrotasks();
    });

    await act(async () => {
      findCheckboxByLabel('Enabled for session').click();
      await flushMicrotasks();
    });
    expect(findCheckboxByLabel('Enabled for session').checked).toBe(true);

    await act(async () => {
      refreshRequest.resolve(
        jsonResponse({
          session: { id: 'main', name: 'Main' },
          policy: { enabledServers: [], enabledTools: [] },
          servers: [
            {
              id: 'remote',
              name: 'remote',
              transport: 'streamable-http',
              configuredEnabled: true,
              enabled: false,
              authenticated: false,
              toolCount: 1,
              resourceCount: 0,
              promptCount: 0,
            },
          ],
          tools: [],
          resources: [],
          prompts: [],
        }),
      );
      await flushMicrotasks();
    });

    expect(findCheckboxByLabel('Enabled for session').checked).toBe(true);
    expect(document.body.textContent).toContain('Unsaved');
  });

  it('keeps newer MCP policy edits made while a save is in flight', async () => {
    const saveRequest = deferred<Response>();
    let catalogCalls = 0;
    let savedBody: unknown;
    const fetchMock = vi.fn<typeof fetch>((input, init) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config') {
        return Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: {
              mcpServers: {
                remote: {
                  transport: 'streamable-http',
                  url: 'https://mcp.example/mcp',
                  enabled: true,
                },
              },
            },
          }),
        );
      }
      if (url === '/api/mcp/catalog?session=main') {
        catalogCalls += 1;
        return Promise.resolve(
          jsonResponse({
            session: { id: 'main', name: 'Main' },
            policy: { enabledServers: [], enabledTools: [] },
            servers: [
              {
                id: 'remote',
                name: 'remote',
                transport: 'streamable-http',
                configuredEnabled: true,
                enabled: false,
                authenticated: false,
                toolCount: 0,
                resourceCount: 0,
                promptCount: 0,
              },
            ],
            tools: [],
            resources: [],
            prompts: [],
          }),
        );
      }
      if (url === '/api/mcp/session-policy?session=main' && init?.method === 'PUT') {
        savedBody = JSON.parse(String(init.body || '{}'));
        return saveRequest.promise;
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
    expect(findCheckboxByLabel('Enabled for session').checked).toBe(false);

    await act(async () => {
      findCheckboxByLabel('Enabled for session').click();
      await flushMicrotasks();
    });
    expect(findCheckboxByLabel('Enabled for session').checked).toBe(true);

    await act(async () => {
      findButtonByText('Save MCP Permissions').click();
      await flushMicrotasks();
    });
    expect(savedBody).toEqual({
      enabledServers: ['remote'],
      enabledTools: [],
      confirmMutatingTools: false,
      clientCapabilities: {},
    });
    expect(findButtonByText('Saving...').disabled).toBe(true);

    await act(async () => {
      findCheckboxByLabel('Enabled for session').click();
      await flushMicrotasks();
    });
    expect(findCheckboxByLabel('Enabled for session').checked).toBe(false);

    await act(async () => {
      saveRequest.resolve(jsonResponse({ ok: true, policy: {} }));
      await flushMicrotasks();
    });

    expect(findCheckboxByLabel('Enabled for session').checked).toBe(false);
    expect(document.body.textContent).toContain('Unsaved');
    expect(findButtonByText('Save MCP Permissions').disabled).toBe(false);
    expect(catalogCalls).toBe(1);
  });

  it('drops stale MCP policy tool ids after a successful catalog refresh', async () => {
    let savedBody: unknown;
    const fetchMock = vi.fn<typeof fetch>((input, init) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config') {
        return Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: {
              mcpServers: {
                remote: {
                  transport: 'streamable-http',
                  url: 'https://mcp.example/mcp',
                  enabled: true,
                },
                disabled: {
                  transport: 'streamable-http',
                  url: 'https://disabled.example/mcp',
                  enabled: false,
                },
              },
            },
          }),
        );
      }
      if (url === '/api/mcp/catalog?session=main') {
        return Promise.resolve(
          jsonResponse({
            session: { id: 'main', name: 'Main' },
            policy: {
              enabledServers: ['remote', 'disabled', 'missing'],
              enabledTools: [
                'mcp__remote__cached__99999999',
                'mcp__remote__read__abc12345',
                'mcp__disabled__write__def67890',
                'mcp__missing__tool__bad00000',
              ],
              confirmMutatingTools: false,
              clientCapabilities: { roots: false },
            },
            servers: [
              {
                id: 'remote',
                name: 'remote',
                transport: 'streamable-http',
                configuredEnabled: true,
                enabled: true,
                authenticated: false,
                toolCount: 1,
                resourceCount: 0,
                promptCount: 0,
              },
              {
                id: 'disabled',
                name: 'disabled',
                transport: 'streamable-http',
                configuredEnabled: false,
                enabled: true,
                authenticated: false,
                toolCount: 0,
                resourceCount: 0,
                promptCount: 0,
              },
            ],
            tools: [
              {
                id: 'mcp__remote__read__abc12345',
                server: 'remote',
                rawName: 'read',
                name: 'mcp__remote__read__abc12345',
                readOnly: true,
                enabled: true,
              },
            ],
            resources: [],
            prompts: [],
          }),
        );
      }
      if (url === '/api/mcp/session-policy?session=main' && init?.method === 'PUT') {
        savedBody = JSON.parse(String(init.body || '{}'));
        return Promise.resolve(jsonResponse({ ok: true, policy: {} }));
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
      findCheckboxByLabel('Expose this session workspace root').click();
      await flushMicrotasks();
    });

    await act(async () => {
      findButtonByText('Save MCP Permissions').click();
      await flushMicrotasks();
    });

    expect(savedBody).toEqual({
      enabledServers: ['remote'],
      enabledTools: ['mcp__remote__read__abc12345'],
      confirmMutatingTools: false,
      clientCapabilities: { roots: true },
    });
  });

  it('preserves hidden MCP policy tools for enabled servers that failed to refresh', async () => {
    let savedBody: unknown;
    const fetchMock = vi.fn<typeof fetch>((input, init) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config') {
        return Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: {
              mcpServers: {
                remote: {
                  transport: 'streamable-http',
                  url: 'https://mcp.example/mcp',
                  enabled: true,
                },
              },
            },
          }),
        );
      }
      if (url === '/api/mcp/catalog?session=main') {
        return Promise.resolve(
          jsonResponse({
            session: { id: 'main', name: 'Main' },
            policy: {
              enabledServers: ['remote'],
              enabledTools: ['mcp__remote__cached__99999999'],
              confirmMutatingTools: false,
              clientCapabilities: {},
            },
            servers: [
              {
                id: 'remote',
                name: 'remote',
                transport: 'streamable-http',
                configuredEnabled: true,
                enabled: true,
                authenticated: false,
                toolCount: 0,
                resourceCount: 0,
                promptCount: 0,
                error: 'tools/list failed',
              },
            ],
            tools: [],
            resources: [],
            prompts: [],
          }),
        );
      }
      if (url === '/api/mcp/session-policy?session=main' && init?.method === 'PUT') {
        savedBody = JSON.parse(String(init.body || '{}'));
        return Promise.resolve(jsonResponse({ ok: true, policy: {} }));
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
      findCheckboxByLabel('Expose this session workspace root').click();
      await flushMicrotasks();
    });

    await act(async () => {
      findButtonByText('Save MCP Permissions').click();
      await flushMicrotasks();
    });

    expect(savedBody).toEqual({
      enabledServers: ['remote'],
      enabledTools: ['mcp__remote__cached__99999999'],
      confirmMutatingTools: false,
      clientCapabilities: { roots: true },
    });
  });

  it('clears hidden enabled tool ids when disabling their MCP server', async () => {
    let savedBody: unknown;
    const fetchMock = vi.fn<typeof fetch>((input, init) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config') {
        return Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: {
              mcpServers: {
                remote: {
                  transport: 'streamable-http',
                  url: 'https://mcp.example/mcp',
                  enabled: true,
                },
              },
            },
          }),
        );
      }
      if (url === '/api/mcp/catalog?session=main') {
        return Promise.resolve(
          jsonResponse({
            session: { id: 'main', name: 'Main' },
            policy: {
              enabledServers: ['remote'],
              enabledTools: ['mcp__remote__cached__99999999', 'mcp__remote__read__abc12345'],
              confirmMutatingTools: false,
              clientCapabilities: {},
            },
            servers: [
              {
                id: 'remote',
                name: 'remote',
                transport: 'streamable-http',
                configuredEnabled: true,
                enabled: true,
                authenticated: false,
                toolCount: 1,
                resourceCount: 0,
                promptCount: 0,
              },
            ],
            tools: [
              {
                id: 'mcp__remote__read__abc12345',
                server: 'remote',
                rawName: 'read',
                name: 'mcp__remote__read__abc12345',
                readOnly: true,
                enabled: true,
              },
            ],
            resources: [],
            prompts: [],
          }),
        );
      }
      if (url === '/api/mcp/session-policy?session=main' && init?.method === 'PUT') {
        savedBody = JSON.parse(String(init.body || '{}'));
        return Promise.resolve(jsonResponse({ ok: true, policy: {} }));
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
      findCheckboxByLabel('Enabled for session').click();
      await flushMicrotasks();
    });

    await act(async () => {
      findButtonByText('Save MCP Permissions').click();
      await flushMicrotasks();
    });

    expect(savedBody).toEqual({
      enabledServers: [],
      enabledTools: [],
      confirmMutatingTools: false,
      clientCapabilities: {},
    });
  });

  it('uses exact catalog server ownership when disabling sanitized-colliding MCP servers', async () => {
    let savedBody: unknown;
    const dashTool = 'mcp__github_repo__list_issues__11111111';
    const underscoreTool = 'mcp__github_repo__list_issues__22222222';
    const fetchMock = vi.fn<typeof fetch>((input, init) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config') {
        return Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: {
              mcpServers: {
                'github-repo': {
                  transport: 'streamable-http',
                  url: 'https://dash.example/mcp',
                  enabled: true,
                },
                github_repo: {
                  transport: 'streamable-http',
                  url: 'https://underscore.example/mcp',
                  enabled: true,
                },
              },
            },
          }),
        );
      }
      if (url === '/api/mcp/catalog?session=main') {
        return Promise.resolve(
          jsonResponse({
            session: { id: 'main', name: 'Main' },
            policy: {
              enabledServers: ['github-repo', 'github_repo'],
              enabledTools: [dashTool, underscoreTool],
              confirmMutatingTools: false,
              clientCapabilities: {},
            },
            servers: [
              {
                id: 'github-repo',
                name: 'github-repo',
                transport: 'streamable-http',
                configuredEnabled: true,
                enabled: true,
                authenticated: false,
                toolCount: 1,
                resourceCount: 0,
                promptCount: 0,
              },
              {
                id: 'github_repo',
                name: 'github_repo',
                transport: 'streamable-http',
                configuredEnabled: true,
                enabled: true,
                authenticated: false,
                toolCount: 1,
                resourceCount: 0,
                promptCount: 0,
              },
            ],
            tools: [
              {
                id: dashTool,
                server: 'github-repo',
                rawName: 'list issues',
                name: dashTool,
                readOnly: true,
                enabled: true,
              },
              {
                id: underscoreTool,
                server: 'github_repo',
                rawName: 'list issues',
                name: underscoreTool,
                readOnly: true,
                enabled: true,
              },
            ],
            resources: [],
            prompts: [],
          }),
        );
      }
      if (url === '/api/mcp/session-policy?session=main' && init?.method === 'PUT') {
        savedBody = JSON.parse(String(init.body || '{}'));
        return Promise.resolve(jsonResponse({ ok: true, policy: {} }));
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
      findMcpServerEnabledCheckbox('github-repo').click();
      await flushMicrotasks();
    });

    await act(async () => {
      findButtonByText('Save MCP Permissions').click();
      await flushMicrotasks();
    });

    expect(savedBody).toEqual({
      enabledServers: ['github_repo'],
      enabledTools: [underscoreTool],
      confirmMutatingTools: false,
      clientCapabilities: {},
    });
  });

  it('hides MCP resources and prompts for servers not enabled in the session', async () => {
    const fetchMock = vi.fn<typeof fetch>((input) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config') {
        return Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: {
              mcpServers: {
                docs: {
                  transport: 'streamable-http',
                  url: 'https://docs.example/mcp',
                  enabled: true,
                },
              },
            },
          }),
        );
      }
      if (url === '/api/mcp/catalog?session=main') {
        return Promise.resolve(
          jsonResponse({
            session: { id: 'main', name: 'Main' },
            policy: {
              enabledServers: [],
              enabledTools: [],
              confirmMutatingTools: false,
              clientCapabilities: {},
            },
            servers: [
              {
                id: 'docs',
                name: 'docs',
                transport: 'streamable-http',
                configuredEnabled: true,
                enabled: false,
                authenticated: false,
                toolCount: 0,
                resourceCount: 1,
                promptCount: 1,
              },
            ],
            tools: [],
            resources: [
              {
                server: 'docs',
                uri: 'docs://guide',
                name: 'Guide',
                description: 'Docs guide',
              },
            ],
            prompts: [
              {
                server: 'docs',
                name: 'summarize',
                description: 'Summarize docs',
                arguments: [{ name: 'topic', required: true }],
              },
            ],
          }),
        );
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

    const buttonLabels = Array.from(document.querySelectorAll('button')).map((button) =>
      button.textContent?.trim(),
    );
    expect(buttonLabels).not.toContain('Read');
    expect(buttonLabels).not.toContain('Get');

    await act(async () => {
      findMcpServerEnabledCheckbox('docs').click();
      await flushMicrotasks();
    });

    const afterToggleButtonLabels = Array.from(document.querySelectorAll('button')).map((button) =>
      button.textContent?.trim(),
    );
    expect(afterToggleButtonLabels).not.toContain('Read');
    expect(afterToggleButtonLabels).not.toContain('Get');
    expect(document.body.textContent).toContain('Unsaved');
  });

  it('sends edited MCP prompt arguments when getting a prompt', async () => {
    let promptBody: unknown;
    const fetchMock = vi.fn<typeof fetch>((input, init) => {
      const url = typeof input === 'string' ? input : input.url;
      if (url === '/api/config') {
        return Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: {
              mcpServers: {
                docs: {
                  transport: 'streamable-http',
                  url: 'https://docs.example/mcp',
                  enabled: true,
                },
              },
            },
          }),
        );
      }
      if (url === '/api/mcp/catalog?session=main') {
        return Promise.resolve(
          jsonResponse({
            session: { id: 'main', name: 'Main' },
            policy: {
              enabledServers: ['docs'],
              enabledTools: [],
              confirmMutatingTools: false,
              clientCapabilities: {},
            },
            servers: [
              {
                id: 'docs',
                name: 'docs',
                transport: 'streamable-http',
                configuredEnabled: true,
                enabled: true,
                authenticated: false,
                toolCount: 0,
                resourceCount: 0,
                promptCount: 1,
              },
            ],
            tools: [],
            resources: [],
            prompts: [
              {
                server: 'docs',
                name: 'summarize',
                description: 'Summarize docs',
                arguments: [{ name: 'topic', required: true }],
              },
            ],
          }),
        );
      }
      if (url === '/api/mcp/prompt/get?session=main' && init?.method === 'POST') {
        promptBody = JSON.parse(String(init.body || '{}'));
        return Promise.resolve(jsonResponse({ ok: true, result: { messages: [] } }));
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

    const textarea = document.querySelector('textarea[aria-label="Arguments for summarize"]');
    if (!(textarea instanceof HTMLTextAreaElement)) {
      throw new Error('Prompt arguments textarea not found');
    }

    await act(async () => {
      setTextareaValue(textarea, '{ "topic": "deployment" }');
      await flushMicrotasks();
    });

    await act(async () => {
      findButtonByText('Get').click();
      await flushMicrotasks();
    });

    expect(promptBody).toEqual({
      server: 'docs',
      name: 'summarize',
      arguments: { topic: 'deployment' },
    });
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

  it('adds a sub-agent override and switches every agent route to one model', async () => {
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
                    models: [
                      { id: 'gpt-4o-mini', input: ['text'] },
                      { id: 'gpt-4.1', input: ['text'] },
                    ],
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
      const select = document.querySelector('select[aria-label="Switch all models"]');
      if (!(select instanceof HTMLSelectElement)) {
        throw new Error('Switch all models select not found');
      }
      select.value = 'openai/gpt-4.1';
      select.dispatchEvent(new Event('change', { bubbles: true }));
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
    expect(savedConfig?.agents?.defaults?.model).toEqual({
      primary: 'openai/gpt-4.1',
      fast: 'openai/gpt-4.1',
      'sub-agent': 'openai/gpt-4.1',
      memory: 'openai/gpt-4.1',
      reflection: 'openai/gpt-4.1',
      context: 'openai/gpt-4.1',
      'sub-agent-reviewer': 'openai/gpt-4.1',
    });
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
      const card = document.querySelector('.models-console-card');
      if (!(card instanceof HTMLButtonElement)) throw new Error('Model card not found');
      card.click();
      await flushMicrotasks();
    });

    const input = document.querySelector('input[list="models-console-thinking-formats"]');
    if (!(input instanceof HTMLInputElement)) {
      throw new Error('Thinking Format input not found');
    }
    expect(input.value).toBe('qwen');
    expect(
      Array.from(document.querySelectorAll('#models-console-thinking-formats option')).map(
        (option) => (option as HTMLOptionElement).value,
      ),
    ).toEqual(['openai', 'qwen', 'doubao', 'deepseek-v4', 'ollama', 'gpt-oss', 'ollama-gpt-oss']);

    await act(async () => {
      setInputValue(input, 'deepseek-v4');
      await flushMicrotasks();
    });
    expect(input.value).toBe('deepseek-v4');

    await act(async () => {
      setLanguage('zh-CN');
      await flushMicrotasks();
    });

    const inspector = document.querySelector('.models-console-inspector');
    expect(inspector?.textContent).toContain('推理格式');
    expect(inspector?.textContent).toContain('模型 ID');
    expect(inspector?.textContent).toContain('上下文窗口');
    expect(inspector?.textContent).toContain('最大 Token');
    expect(document.querySelector('input[list="models-console-thinking-formats"]')).toBe(input);
    expect(findPrimaryTestButton().textContent).toBe('测试');
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
      const select = document.querySelector('.models-console-connection-grid select');
      if (!(select instanceof HTMLSelectElement)) throw new Error('API type select not found');
      expect(select.value).toBe('openai-responses');
      expect(Array.from(select.options).some((option) => option.value === 'openai-responses')).toBe(
        true,
      );
    });
  });

  it('treats a newly added blank model card as an unsaved Console draft', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn<typeof fetch>(() =>
        Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: {
              models: {
                providers: {
                  gateway: {
                    api: 'openai-completions',
                    baseUrl: 'https://gateway.example/v1',
                    apiKey: 'secret',
                    models: [{ id: 'model-one', input: ['text'] }],
                  },
                },
              },
            },
          }),
        ),
      ),
    );

    ({ root } = await renderSettingsPage());
    await openAndLoad(undefined, 'tab-models');
    const addModel = Array.from(document.querySelectorAll('button')).find(
      (button) => button.textContent?.trim() === 'Add model',
    );
    if (!(addModel instanceof HTMLButtonElement)) throw new Error('Add model button not found');

    await act(async () => {
      addModel.click();
      await flushMicrotasks();
    });
    expect(document.querySelectorAll('.models-console-card')).toHaveLength(2);

    await act(async () => {
      findCloseButton().click();
      await flushMicrotasks();
    });
    expect(document.querySelector('.settings-discard-dialog')).not.toBeNull();
    expect(document.querySelector('.console-page-surface')).not.toBeNull();
  });

  it('lets a nested provider dialog consume Escape before the Console shell', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn<typeof fetch>(() =>
        Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: {
              models: {
                providers: {
                  gateway: {
                    api: 'openai-completions',
                    models: [{ id: 'model-one' }],
                  },
                },
              },
            },
          }),
        ),
      ),
    );

    ({ root } = await renderSettingsPage());
    await openAndLoad(undefined, 'tab-models');
    const addProvider = Array.from(document.querySelectorAll('button')).find((button) =>
      button.textContent?.includes('Add provider'),
    );
    if (!(addProvider instanceof HTMLButtonElement)) {
      throw new Error('Add provider button not found');
    }
    await act(async () => {
      addProvider.click();
      await flushMicrotasks();
    });
    expect(document.querySelector('[aria-modal="true"]')).not.toBeNull();

    const escapedToDocument = vi.fn();
    document.addEventListener('keydown', escapedToDocument);
    await act(async () => {
      const input = document.querySelector('.models-console-dialog input');
      if (!(input instanceof HTMLInputElement)) throw new Error('Provider name input not found');
      input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
      await flushMicrotasks();
    });
    document.removeEventListener('keydown', escapedToDocument);
    expect(document.querySelector('[aria-modal="true"]')).toBeNull();
    expect(escapedToDocument).not.toHaveBeenCalled();
    expect(document.querySelector('.console-page-surface')).not.toBeNull();
    expect(document.querySelector('.settings-discard-dialog')).toBeNull();
  });

  it('lets a model delete confirmation consume Escape before the Console shell', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn<typeof fetch>(() =>
        Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: {
              models: {
                providers: {
                  gateway: {
                    api: 'openai-completions',
                    models: [{ id: 'model-one' }],
                  },
                },
              },
            },
          }),
        ),
      ),
    );

    ({ root } = await renderSettingsPage());
    await openAndLoad(undefined, 'tab-models');
    await act(async () => {
      const card = document.querySelector<HTMLButtonElement>('.models-console-card');
      card?.click();
      await flushMicrotasks();
      const remove = document.querySelector<HTMLButtonElement>('.models-console-delete-button');
      remove?.click();
      await flushMicrotasks();
    });
    expect(document.querySelector('.models-console-inspector-confirm')).not.toBeNull();

    await act(async () => {
      window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
      await flushMicrotasks();
    });

    expect(document.querySelector('.models-console-inspector-confirm')).toBeNull();
    expect(document.querySelector('.console-page-surface')).not.toBeNull();
    expect(document.querySelector('.settings-discard-dialog')).toBeNull();
  });

  it('ignores hidden persistent modals when Escape dismisses the discard prompt', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn<typeof fetch>(() =>
        Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            config: { settings: { port: 18989 } },
          }),
        ),
      ),
    );

    ({ root } = await renderSettingsPage());
    await openAndLoad();
    await act(async () => {
      setInputValue(findInputByPlaceholder('18989'), '19000');
      await flushMicrotasks();
      findCloseButton().click();
      await flushMicrotasks();
    });
    expect(document.querySelector('.settings-discard-dialog')).not.toBeNull();

    const hiddenModal = document.createElement('div');
    hiddenModal.hidden = true;
    hiddenModal.setAttribute('aria-modal', 'true');
    document.body.append(hiddenModal);
    await act(async () => {
      window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
      await flushMicrotasks();
    });

    expect(document.querySelector('.settings-discard-dialog')).toBeNull();
    expect(document.querySelector('.console-page-surface')).not.toBeNull();
  });

  it('keeps General available as the recovery route for corrupt configuration', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn<typeof fetch>(() =>
        Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            parse_error: 'unexpected token',
            raw: '{',
          }),
        ),
      ),
    );

    ({ root } = await renderSettingsPage());
    await openAndLoad(undefined, 'tab-usage');
    const general = document.querySelector<HTMLButtonElement>('[data-tab="tab-general"]');
    const agents = document.querySelector<HTMLButtonElement>('[data-tab="tab-agents"]');
    expect(general?.disabled).toBe(false);
    expect(agents?.disabled).toBe(true);

    await act(async () => {
      general?.focus();
      general?.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }));
      await flushMicrotasks();
    });
    const usage = document.querySelector<HTMLButtonElement>('[data-tab="tab-usage"]');
    expect(usage?.getAttribute('aria-selected')).toBe('true');

    await act(async () => {
      usage?.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowLeft', bubbles: true }));
      await flushMicrotasks();
    });
    expect(general?.getAttribute('aria-selected')).toBe('true');
    expect(document.body.textContent).toContain('Config file has syntax errors');
  });

  it('opens corrupt configuration directly on the General recovery editor', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn<typeof fetch>(() =>
        Promise.resolve(
          jsonResponse({
            path: '/tmp/config.json',
            parse_error: 'unexpected token',
            raw: '{',
          }),
        ),
      ),
    );

    ({ root } = await renderSettingsPage());
    await openAndLoad(undefined, 'tab-models');

    expect(document.querySelector('[data-tab="tab-general"]')?.getAttribute('aria-selected')).toBe(
      'true',
    );
    expect(document.getElementById('tab-general-panel')?.hasAttribute('hidden')).toBe(false);
    expect(document.querySelector<HTMLTextAreaElement>('.json-editor')?.value).toBe('{');
  });

  it('preserves a corrupt-config recovery draft while visiting Usage', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn<typeof fetch>((input) => {
        const url = typeof input === 'string' ? input : input.url;
        if (url === '/api/config') {
          return Promise.resolve(
            jsonResponse({
              path: '/tmp/config.json',
              parse_error: 'unexpected token',
              raw: '{',
            }),
          );
        }
        if (url.startsWith('/api/usage')) return Promise.resolve(jsonResponse({}));
        throw new Error(`Unexpected fetch URL: ${url}`);
      }),
    );

    ({ root } = await renderSettingsPage());
    await openAndLoad();
    const editor = document.querySelector<HTMLTextAreaElement>('.json-editor');
    if (!editor) throw new Error('Recovery editor not found');
    await act(async () => {
      setTextareaValue(editor, '{"settings":{"port":19000}}');
      findButtonByText('Token Usage').click();
      await flushMicrotasks();
    });

    expect(document.getElementById('tab-general-panel')?.hasAttribute('hidden')).toBe(true);
    await act(async () => {
      findButtonByText('General').click();
      await flushMicrotasks();
    });
    expect(document.querySelector<HTMLTextAreaElement>('.json-editor')?.value).toBe(
      '{"settings":{"port":19000}}',
    );

    await act(async () => {
      findCloseButton().click();
      await flushMicrotasks();
    });
    expect(document.querySelector('.settings-discard-dialog')).not.toBeNull();
  });
});
