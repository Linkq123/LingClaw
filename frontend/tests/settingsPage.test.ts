import React from 'react';
import { act } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createRoot, type Root } from 'react-dom/client';

import { SettingsPage, closeSettingsPage, openSettingsPage } from '../src/pages/SettingsPage.js';
import {
  CONFIG_SAVED_EVENT,
  acceptComposerSocketModelPayloadRevision,
  beginComposerRevisionHandshake,
} from '../src/composerAvailability.js';
import { state } from '../src/state.js';

beforeEach(() => {
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

async function openAndLoad(sessionId?: string): Promise<void> {
  await act(async () => {
    openSettingsPage(sessionId);
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
  const button = document.querySelector('button[aria-label="Close"]');
  if (!(button instanceof HTMLButtonElement)) {
    throw new Error('Close button not found');
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
    expect(tabs).toHaveLength(6);

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

    await act(async () => {
      findButtonByText('Reload latest').click();
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
