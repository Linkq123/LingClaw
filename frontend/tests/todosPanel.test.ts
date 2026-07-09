import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { dom, state } from '../src/state.js';
import {
  applyTodosState,
  applyTodosVisibility,
  initTodosPanel,
  resetTodosUiState,
} from '../src/renderers/todos.js';
import { setLanguage } from '../src/i18n.js';

function flushPromises(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

function jsonResponse(payload: unknown, status = 200): Response {
  return new Response(JSON.stringify(payload), {
    status,
    headers: {
      'Content-Type': 'application/json',
    },
  });
}

describe('todos panel', () => {
  beforeEach(() => {
    setLanguage('en');
    document.body.innerHTML = `
      <section id="todos-host"></section>
      <div id="chat"></div>
    `;
    dom.todosHost = document.getElementById('todos-host');
    dom.todosPanel = null;
    dom.chat = document.getElementById('chat');
    state.activeSessionId = 'session-alpha';
    state.showTodos = true;
    state.sessionSwitchInFlight = false;
    state.todoDrafts = new Map();
    initTodosPanel();
    resetTodosUiState();
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    document.body.innerHTML = '';
    dom.todosHost = null;
    dom.todosPanel = null;
    dom.chat = null;
    state.activeSessionId = '';
    state.showTodos = true;
    state.sessionSwitchInFlight = false;
    state.todoDrafts = new Map();
  });

  it('renders empty and non-empty snapshots without depending on chat history nodes', () => {
    expect(dom.todosPanel?.textContent).toContain('No todos yet');

    applyTodosState({
      type: 'todos_state',
      revision: 3,
      items: [
        {
          id: 'todo-1',
          content: 'Inspect runtime loop',
          status: 'in_progress',
        },
      ],
      last_updated_by: 'user',
      updated_at: 1710000000,
    });

    expect(dom.todosPanel?.textContent).toContain('Updated by You');
    expect(dom.todosPanel?.querySelector<HTMLInputElement>('.todo-row-input')?.value).toBe(
      'Inspect runtime loop',
    );
    expect(dom.todosPanel?.textContent).not.toContain('No todos yet');

    dom.chat?.replaceChildren(document.createElement('div'));

    expect(dom.todosPanel?.isConnected).toBe(true);
    expect(dom.todosHost?.contains(dom.todosPanel as HTMLElement)).toBe(true);
  });

  it('hides and shows the todos host based on local visibility state', () => {
    state.showTodos = false;
    applyTodosVisibility();
    expect(dom.todosHost?.hidden).toBe(true);

    state.showTodos = true;
    applyTodosVisibility();
    expect(dom.todosHost?.hidden).toBe(false);
  });

  it('disables todo edits while a session switch is in flight', () => {
    const fetchMock = vi.fn();
    vi.stubGlobal('fetch', fetchMock);

    state.sessionSwitchInFlight = true;
    applyTodosState({
      type: 'todos_state',
      revision: 1,
      items: [
        {
          id: 'todo-1',
          content: 'Pending switch item',
          status: 'pending',
        },
      ],
      last_updated_by: 'user',
      updated_at: 1710000000,
    });

    expect(dom.todosPanel?.querySelector<HTMLInputElement>('.todo-row-input')?.disabled).toBe(true);
    expect(dom.todosPanel?.querySelector<HTMLSelectElement>('.todo-row-status')?.disabled).toBe(
      true,
    );
    const addButton = dom.todosPanel?.querySelector<HTMLButtonElement>('.todos-add-btn');
    expect(addButton?.disabled).toBe(true);

    addButton?.click();
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it('sends PUT requests for status changes, text edits, reorders, deletes, and adds', async () => {
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      const body = JSON.parse(String(init?.body ?? '{}'));
      return jsonResponse({
        ok: true,
        conflict: false,
        revision: Number(body.base_revision ?? 0) + 1,
        items: body.items,
        last_updated_by: 'user',
        updated_at: 1710000100 + Number(body.base_revision ?? 0),
      });
    });
    vi.stubGlobal('fetch', fetchMock);
    vi.stubGlobal('crypto', { randomUUID: () => 'new-item-id' });

    applyTodosState({
      type: 'todos_state',
      revision: 1,
      items: [
        {
          id: 'todo-1',
          content: 'First item',
          status: 'pending',
        },
        {
          id: 'todo-2',
          content: 'Second item',
          status: 'in_progress',
        },
      ],
      last_updated_by: 'assistant',
      updated_at: 1710000000,
    });

    const statusSelect = dom.todosPanel?.querySelectorAll<HTMLSelectElement>('.todo-row-status')[0];
    expect(dom.todosPanel?.textContent).toContain('Updated by Assistant');

    statusSelect!.value = 'completed';
    statusSelect!.dispatchEvent(new Event('change', { bubbles: true }));
    await flushPromises();

    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(String(fetchMock.mock.calls[0]?.[0])).toBe('/api/todos?session=session-alpha');
    expect(JSON.parse(String(fetchMock.mock.calls[0]?.[1]?.body))).toMatchObject({
      base_revision: 1,
      items: [
        { id: 'todo-1', content: 'First item', status: 'completed' },
        { id: 'todo-2', content: 'Second item', status: 'in_progress' },
      ],
    });
    expect(state.todos.revision).toBe(2);

    const textInput = dom.todosPanel?.querySelectorAll<HTMLInputElement>('.todo-row-input')[0];
    textInput!.value = 'First item updated';
    textInput!.dispatchEvent(new Event('input', { bubbles: true }));
    textInput!.dispatchEvent(new Event('blur', { bubbles: true }));
    await flushPromises();

    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(JSON.parse(String(fetchMock.mock.calls[1]?.[1]?.body))).toMatchObject({
      base_revision: 2,
      items: [
        { id: 'todo-1', content: 'First item updated', status: 'completed' },
        { id: 'todo-2', content: 'Second item', status: 'in_progress' },
      ],
    });
    expect(dom.todosPanel?.querySelectorAll<HTMLInputElement>('.todo-row-input')[0]?.value).toBe(
      'First item updated',
    );

    const moveDownButton = Array.from(
      dom.todosPanel?.querySelectorAll<HTMLButtonElement>('.todo-row-btn') || [],
    ).find((button) => button.title === 'Move down');
    moveDownButton!.click();
    await flushPromises();

    expect(fetchMock).toHaveBeenCalledTimes(3);
    expect(JSON.parse(String(fetchMock.mock.calls[2]?.[1]?.body))).toMatchObject({
      base_revision: 3,
      items: [
        { id: 'todo-2', content: 'Second item', status: 'in_progress' },
        { id: 'todo-1', content: 'First item updated', status: 'completed' },
      ],
    });

    const deleteButtons = Array.from(
      dom.todosPanel?.querySelectorAll<HTMLButtonElement>('.todo-row-btn') || [],
    ).filter((button) => button.title === 'Delete todo');
    deleteButtons[0].click();
    await flushPromises();

    expect(fetchMock).toHaveBeenCalledTimes(4);
    expect(JSON.parse(String(fetchMock.mock.calls[3]?.[1]?.body))).toMatchObject({
      base_revision: 4,
      items: [{ id: 'todo-1', content: 'First item updated', status: 'completed' }],
    });

    const addButton = dom.todosPanel?.querySelector<HTMLButtonElement>('.todos-add-btn');
    addButton!.click();
    await flushPromises();

    expect(fetchMock).toHaveBeenCalledTimes(5);
    const addRequest = JSON.parse(String(fetchMock.mock.calls[4]?.[1]?.body));
    expect(addRequest.base_revision).toBe(5);
    expect(addRequest.items).toHaveLength(2);
    expect(addRequest.items[1]).toMatchObject({
      id: 'todo-new-item-id',
      content: 'New todo',
      status: 'pending',
    });
    expect(state.todos.revision).toBe(6);
    const inputs = Array.from(
      dom.todosPanel?.querySelectorAll<HTMLInputElement>('.todo-row-input') || [],
    );
    expect(inputs.map((input) => input.value)).toContain('New todo');
  });

  it('applies the server snapshot and shows a conflict message on 409', async () => {
    const fetchMock = vi.fn(async () =>
      jsonResponse(
        {
          ok: false,
          conflict: true,
          revision: 9,
          items: [
            {
              id: 'todo-1',
              content: 'Server wins',
              status: 'completed',
            },
          ],
          last_updated_by: 'assistant',
          updated_at: 1710000999,
        },
        409,
      ),
    );
    vi.stubGlobal('fetch', fetchMock);

    applyTodosState({
      type: 'todos_state',
      revision: 4,
      items: [
        {
          id: 'todo-1',
          content: 'Local draft',
          status: 'pending',
        },
      ],
      last_updated_by: 'user',
      updated_at: 1710000000,
    });

    const input = dom.todosPanel?.querySelector<HTMLInputElement>('.todo-row-input');
    input!.value = 'Local draft changed';
    input!.dispatchEvent(new Event('input', { bubbles: true }));
    input!.dispatchEvent(new Event('blur', { bubbles: true }));
    await flushPromises();

    expect(state.todos.revision).toBe(9);
    expect(dom.todosPanel?.querySelector<HTMLInputElement>('.todo-row-input')?.value).toBe(
      'Server wins',
    );
    expect(dom.todosPanel?.textContent).toContain('Todo list changed on the server');
  });

  it('clears saving state and shows an error when a failed save has no todos snapshot', async () => {
    const fetchMock = vi.fn(async () => jsonResponse({ error: 'backend failed' }, 500));
    vi.stubGlobal('fetch', fetchMock);

    applyTodosState({
      type: 'todos_state',
      revision: 4,
      items: [
        {
          id: 'todo-1',
          content: 'Local draft',
          status: 'pending',
        },
      ],
      last_updated_by: 'user',
      updated_at: 1710000000,
    });

    const input = dom.todosPanel?.querySelector<HTMLInputElement>('.todo-row-input');
    input!.value = 'Local draft changed';
    input!.dispatchEvent(new Event('input', { bubbles: true }));
    input!.dispatchEvent(new Event('blur', { bubbles: true }));
    await flushPromises();

    expect(state.todoSaving).toBe(false);
    expect(state.todos.revision).toBe(4);
    expect(dom.todosPanel?.querySelector<HTMLInputElement>('.todo-row-input')?.disabled).toBe(
      false,
    );
    expect(dom.todosPanel?.textContent).toContain('backend failed');
  });

  it('ignores stale save responses after switching sessions', async () => {
    let resolveResponse: ((response: Response) => void) | null = null;
    const fetchMock = vi.fn(
      () =>
        new Promise<Response>((resolve) => {
          resolveResponse = resolve;
        }),
    );
    vi.stubGlobal('fetch', fetchMock);

    applyTodosState({
      type: 'todos_state',
      revision: 2,
      items: [
        {
          id: 'todo-1',
          content: 'Session alpha item',
          status: 'pending',
        },
      ],
      last_updated_by: 'user',
      updated_at: 1710000000,
    });

    const input = dom.todosPanel?.querySelector<HTMLInputElement>('.todo-row-input');
    input!.value = 'Session alpha updated';
    input!.dispatchEvent(new Event('input', { bubbles: true }));
    input!.dispatchEvent(new Event('blur', { bubbles: true }));
    await flushPromises();

    state.activeSessionId = 'session-beta';
    resetTodosUiState();

    resolveResponse?.(
      jsonResponse({
        ok: true,
        conflict: false,
        revision: 3,
        items: [
          {
            id: 'todo-1',
            content: 'Stale alpha response',
            status: 'completed',
          },
        ],
        last_updated_by: 'user',
        updated_at: 1710000500,
      }),
    );
    await flushPromises();
    await flushPromises();

    expect(state.activeSessionId).toBe('session-beta');
    expect(state.todos.revision).toBe(0);
    expect(dom.todosPanel?.textContent).toContain('No todos yet');
    expect(dom.todosPanel?.textContent).not.toContain('Stale alpha response');
  });
});
