import { dom, state } from '../state.js';
import type { TodoItem, TodoStatus, TodosStateEvent, TodosUpdateResponse } from '../types.js';
import { tr } from '../i18n.js';

const MAX_TODO_ITEMS = 12;
const MAX_TODO_CONTENT_CHARS = 200;

function cloneTodoItems(items: TodoItem[]): TodoItem[] {
  return items.map((item) => ({ ...item }));
}

export function createDefaultTodosState(): TodosStateEvent {
  return {
    type: 'todos_state',
    revision: 0,
    items: [],
    last_updated_by: 'assistant',
    updated_at: 0,
  };
}

function normalizeTodosState(snapshot?: Partial<TodosStateEvent> | null): TodosStateEvent {
  return {
    type: 'todos_state',
    revision: Number(snapshot?.revision ?? 0),
    items: Array.isArray(snapshot?.items)
      ? snapshot.items.map((item) => ({
          id: String(item?.id ?? ''),
          content: String(item?.content ?? ''),
          status: normalizeStatus(item?.status),
        }))
      : [],
    last_updated_by: snapshot?.last_updated_by === 'user' ? 'user' : 'assistant',
    updated_at: Number(snapshot?.updated_at ?? 0),
  };
}

function normalizeStatus(status: unknown): TodoStatus {
  return status === 'in_progress' || status === 'completed' ? status : 'pending';
}

function formatUpdatedAt(updatedAt: number): string {
  if (!updatedAt) return tr('todos.notSaved');
  return new Date(updatedAt * 1000).toLocaleString(undefined, {
    hour12: false,
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });
}

function buildTodoId(): string {
  const randomUuid =
    typeof globalThis.crypto?.randomUUID === 'function' ? globalThis.crypto.randomUUID() : null;
  if (randomUuid) {
    return `todo-${randomUuid}`.slice(0, 64);
  }
  return `todo-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`.slice(0, 64);
}

function syncDrafts(items: TodoItem[]): void {
  state.todoDrafts = new Map(items.map((item) => [item.id, item.content]));
}

function setTodoFeedback(message: string, kind: 'conflict' | 'error' | ''): void {
  state.todoFeedbackMessage = message;
  state.todoFeedbackKind = kind;
}

function findTodo(id: string): TodoItem | undefined {
  return state.todos.items.find((item) => item.id === id);
}

function ensureTodosPanel(): HTMLElement | null {
  if (!dom.todosHost) return null;
  if (!dom.todosPanel || !dom.todosPanel.isConnected) {
    const panel = document.createElement('section');
    panel.id = 'todos-panel';
    panel.className = 'todos-panel';
    panel.setAttribute('aria-label', tr('todos.panelAria'));
    dom.todosHost.replaceChildren(panel);
    dom.todosPanel = panel;
  }
  return dom.todosPanel;
}

function currentSessionId(): string {
  return state.activeSessionId || 'main';
}

async function parseJsonResponse(response: Response): Promise<Record<string, unknown>> {
  try {
    return (await response.json()) as Record<string, unknown>;
  } catch {
    return {};
  }
}

function responseToTodosState(
  payload: TodosUpdateResponse | Record<string, unknown>,
): TodosStateEvent {
  return normalizeTodosState({
    revision: Number(payload.revision ?? 0),
    items: payload.items as TodoItem[] | undefined,
    last_updated_by: payload.last_updated_by as 'user' | 'assistant' | undefined,
    updated_at: Number(payload.updated_at ?? 0),
  });
}

function shouldIgnoreResponseForSession(
  requestSessionId: string,
  snapshotRevision?: number,
): boolean {
  if (currentSessionId() !== requestSessionId) {
    return true;
  }
  return snapshotRevision != null && state.todos.revision > snapshotRevision;
}

async function persistTodos(
  nextItems: TodoItem[],
  options: { focusId?: string | null; savingItemId?: string | null } = {},
): Promise<boolean> {
  if (state.todoSaving || state.sessionSwitchInFlight) return false;

  const requestSessionId = currentSessionId();
  const requestBaseRevision = state.todos.revision;
  state.todoSaving = true;
  state.todoSavingItemId = options.savingItemId ?? null;
  if (options.focusId) {
    state.todoPendingFocusId = options.focusId;
  }
  renderTodosPanel();

  try {
    const response = await fetch(`/api/todos?session=${encodeURIComponent(requestSessionId)}`, {
      method: 'PUT',
      headers: {
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({
        base_revision: requestBaseRevision,
        items: nextItems,
      }),
    });
    const payload = await parseJsonResponse(response);

    if (response.status === 409) {
      const nextSnapshot = responseToTodosState(payload);
      if (shouldIgnoreResponseForSession(requestSessionId, nextSnapshot.revision)) {
        return false;
      }
      applyTodosState(nextSnapshot, { preserveFeedback: true });
      setTodoFeedback(tr('todos.changed', { revision: Number(payload.revision ?? 0) }), 'conflict');
      renderTodosPanel();
      return false;
    }

    if (!response.ok) {
      throw new Error(String(payload.error ?? tr('todos.saveFailed')));
    }

    const nextSnapshot = responseToTodosState(payload);
    if (shouldIgnoreResponseForSession(requestSessionId, nextSnapshot.revision)) {
      return false;
    }

    applyTodosState(nextSnapshot);
    return true;
  } catch (error) {
    if (shouldIgnoreResponseForSession(requestSessionId)) {
      return false;
    }
    state.todoSaving = false;
    state.todoSavingItemId = null;
    state.todoPendingFocusId = null;
    setTodoFeedback(error instanceof Error ? error.message : tr('todos.saveFailed'), 'error');
    renderTodosPanel();
    return false;
  }
}

function updateTodoStatus(id: string, status: TodoStatus): void {
  const items: TodoItem[] = cloneTodoItems(state.todos.items).map((item): TodoItem => {
    if (item.id === id) {
      return { ...item, status };
    }
    if (status === 'in_progress' && item.status === 'in_progress') {
      return { ...item, status: 'pending' };
    }
    return item;
  });
  void persistTodos(items, { savingItemId: id });
}

function moveTodo(id: string, direction: -1 | 1): void {
  const items = cloneTodoItems(state.todos.items);
  const index = items.findIndex((item) => item.id === id);
  const nextIndex = index + direction;
  if (index < 0 || nextIndex < 0 || nextIndex >= items.length) return;
  const [item] = items.splice(index, 1);
  items.splice(nextIndex, 0, item);
  void persistTodos(items, { savingItemId: id });
}

function removeTodo(id: string): void {
  const items = cloneTodoItems(state.todos.items).filter((item) => item.id !== id);
  void persistTodos(items, { savingItemId: id });
}

function commitTodoContent(id: string, nextValue: string): void {
  const current = findTodo(id);
  if (!current) return;

  const trimmed = nextValue.trim();
  if (!trimmed) {
    state.todoDrafts.set(id, current.content);
    setTodoFeedback(tr('todos.emptyError'), 'error');
    renderTodosPanel();
    return;
  }

  if (trimmed === current.content) {
    state.todoDrafts.set(id, current.content);
    renderTodosPanel();
    return;
  }

  const items = cloneTodoItems(state.todos.items).map((item) =>
    item.id === id ? { ...item, content: trimmed } : item,
  );
  void persistTodos(items, { focusId: id, savingItemId: id });
}

function addTodo(): void {
  if (state.todos.items.length >= MAX_TODO_ITEMS) return;
  const id = buildTodoId();
  const items = [
    ...cloneTodoItems(state.todos.items),
    {
      id,
      content: tr('todos.defaultNew'),
      status: 'pending' as TodoStatus,
    },
  ];
  state.todoDrafts.set(id, tr('todos.defaultNew'));
  void persistTodos(items, { focusId: id, savingItemId: id });
}

function createIconButton(
  label: string,
  title: string,
  onClick: () => void,
  disabled: boolean,
): HTMLButtonElement {
  const button = document.createElement('button');
  button.type = 'button';
  button.className = 'todo-row-btn';
  button.textContent = label;
  button.title = title;
  button.setAttribute('aria-label', title);
  button.disabled = disabled;
  button.addEventListener('click', onClick);
  return button;
}

function renderTodoRow(
  item: TodoItem,
  index: number,
  total: number,
  disabled: boolean,
): HTMLElement {
  const row = document.createElement('li');
  row.className = 'todo-row';
  if (state.todoSaving && state.todoSavingItemId === item.id) {
    row.classList.add('is-saving');
  }

  const statusSelect = document.createElement('select');
  statusSelect.className = 'todo-row-status';
  statusSelect.disabled = disabled;
  statusSelect.setAttribute('aria-label', tr('todos.statusFor', { content: item.content }));
  for (const status of ['pending', 'in_progress', 'completed'] as TodoStatus[]) {
    const option = document.createElement('option');
    option.value = status;
    option.textContent = tr(`todos.status.${status}`);
    option.selected = item.status === status;
    statusSelect.appendChild(option);
  }
  statusSelect.addEventListener('change', () => {
    updateTodoStatus(item.id, normalizeStatus(statusSelect.value));
  });

  const input = document.createElement('input');
  input.className = 'todo-row-input';
  input.type = 'text';
  input.maxLength = MAX_TODO_CONTENT_CHARS;
  input.value = state.todoDrafts.get(item.id) ?? item.content;
  input.disabled = disabled;
  input.placeholder = tr('todos.todoPlaceholder');
  input.dataset.todoInputId = item.id;
  input.addEventListener('input', () => {
    state.todoDrafts.set(item.id, input.value);
  });
  input.addEventListener('blur', () => {
    commitTodoContent(item.id, input.value);
  });
  input.addEventListener('keydown', (event) => {
    if (event.key !== 'Enter') return;
    event.preventDefault();
    input.blur();
  });

  const actions = document.createElement('div');
  actions.className = 'todo-row-actions';
  actions.appendChild(
    createIconButton('↑', tr('todos.moveUp'), () => moveTodo(item.id, -1), disabled || index === 0),
  );
  actions.appendChild(
    createIconButton(
      '↓',
      tr('todos.moveDown'),
      () => moveTodo(item.id, 1),
      disabled || index === total - 1,
    ),
  );
  actions.appendChild(
    createIconButton('×', tr('todos.delete'), () => removeTodo(item.id), disabled),
  );

  row.appendChild(statusSelect);
  row.appendChild(input);
  row.appendChild(actions);

  return row;
}

export function renderTodosPanel(): void {
  const panel = ensureTodosPanel();
  if (!panel) return;

  const disabled = state.todoSaving || state.sessionSwitchInFlight;
  const snapshot = state.todos;

  const header = document.createElement('div');
  header.className = 'todos-panel-header';

  const heading = document.createElement('div');
  heading.className = 'todos-panel-heading';

  const title = document.createElement('h2');
  title.className = 'todos-panel-title';
  title.textContent = tr('todos.title');

  const meta = document.createElement('div');
  meta.className = 'todos-panel-meta';
  meta.textContent = tr('todos.updatedBy', {
    by: snapshot.last_updated_by === 'user' ? tr('common.you') : tr('common.assistant'),
    time: formatUpdatedAt(snapshot.updated_at),
  });

  heading.appendChild(title);
  heading.appendChild(meta);

  const headerActions = document.createElement('div');
  headerActions.className = 'todos-panel-actions';

  if (state.todoSaving) {
    const saving = document.createElement('span');
    saving.className = 'todos-panel-saving';
    saving.textContent = tr('todos.saving');
    headerActions.appendChild(saving);
  }

  const addButton = document.createElement('button');
  addButton.type = 'button';
  addButton.className = 'todos-add-btn';
  addButton.textContent = '+';
  addButton.title = tr('todos.add');
  addButton.setAttribute('aria-label', tr('todos.add'));
  addButton.disabled = disabled || snapshot.items.length >= MAX_TODO_ITEMS;
  addButton.addEventListener('click', () => addTodo());
  headerActions.appendChild(addButton);

  header.appendChild(heading);
  header.appendChild(headerActions);

  const feedback = document.createElement('div');
  feedback.className = `todos-feedback${state.todoFeedbackKind ? ` is-${state.todoFeedbackKind}` : ''}`;
  feedback.hidden = !state.todoFeedbackMessage;
  feedback.textContent = state.todoFeedbackMessage;

  const body = document.createElement('div');
  body.className = 'todos-panel-body';

  if (snapshot.items.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'todos-empty';

    const emptyText = document.createElement('span');
    emptyText.textContent = tr('todos.empty');

    const emptyAdd = document.createElement('button');
    emptyAdd.type = 'button';
    emptyAdd.className = 'todos-empty-btn';
    emptyAdd.textContent = tr('common.add');
    emptyAdd.disabled = disabled;
    emptyAdd.addEventListener('click', () => addTodo());

    empty.appendChild(emptyText);
    empty.appendChild(emptyAdd);
    body.appendChild(empty);
  } else {
    const list = document.createElement('ol');
    list.className = 'todos-list';
    snapshot.items.forEach((item, index) => {
      list.appendChild(renderTodoRow(item, index, snapshot.items.length, disabled));
    });
    body.appendChild(list);
  }

  panel.replaceChildren(header, feedback, body);

  const focusId = state.todoPendingFocusId;
  if (focusId) {
    const input = panel.querySelector<HTMLInputElement>(`input[data-todo-input-id="${focusId}"]`);
    if (input) {
      input.focus();
      input.select();
    }
    state.todoPendingFocusId = null;
  }
}

export function initTodosPanel(): void {
  ensureTodosPanel();
  renderTodosPanel();
}

export function applyTodosVisibility(): void {
  if (!dom.todosHost) return;
  dom.todosHost.hidden = !state.showTodos;
}

export function resetTodosUiState(): void {
  state.todos = createDefaultTodosState();
  state.todoDrafts = new Map();
  state.todoSaving = false;
  state.todoSavingItemId = null;
  state.todoPendingFocusId = null;
  setTodoFeedback('', '');
  renderTodosPanel();
}

export function applyTodosState(
  snapshot: Partial<TodosStateEvent> | TodosStateEvent,
  options: { preserveFeedback?: boolean } = {},
): void {
  state.todos = normalizeTodosState(snapshot);
  syncDrafts(state.todos.items);
  state.todoSaving = false;
  state.todoSavingItemId = null;
  if (!options.preserveFeedback) {
    setTodoFeedback('', '');
  }
  renderTodosPanel();
}
