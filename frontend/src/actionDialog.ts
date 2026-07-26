import { createIcon } from './icons.js';
import { tr } from './i18n.js';
import { trapDialogFocus } from './pages/dialogFocus.js';
import { appendWorkspacePortal } from './workspacePortal.js';

export interface ActionDialogSessionOption {
  id: string;
  name: string;
}

type MaybePromise<T> = T | Promise<T>;

type EntityRequestBase = {
  entityId: string;
  entityName: string;
};

export type ActionDialogRequest =
  | (EntityRequestBase & {
      kind: 'rename-session';
      submit: (name: string) => MaybePromise<void>;
    })
  | (EntityRequestBase & {
      kind: 'delete-session' | 'delete-group' | 'remove-group-member';
      submit: () => MaybePromise<void>;
    })
  | {
      kind: 'create-group' | 'edit-group';
      groupId?: string;
      initialName: string;
      sessions: ActionDialogSessionOption[];
      selectedMembers: string[];
      submit: (value: { name: string; members: string[] }) => MaybePromise<void>;
    };

export type ActionDialogResult =
  | { kind: 'rename-session'; name: string }
  | { kind: 'delete-session' | 'delete-group' | 'remove-group-member' }
  | { kind: 'create-group' | 'edit-group'; name: string; members: string[] };

type ActiveDialog = {
  request: ActionDialogRequest;
  overlay: HTMLElement;
  panel: HTMLElement;
  restoreFocus: HTMLElement | null;
  resolve: (result: ActionDialogResult | null) => void;
  busy: boolean;
  name: string;
  search: string;
  selectedMembers: Set<string>;
  error: string;
  errorKind: 'name' | 'members' | 'submit' | '';
  errorDetail: string;
};

let activeDialog: ActiveDialog | null = null;

function isGroupRequest(
  request: ActionDialogRequest,
): request is Extract<ActionDialogRequest, { kind: 'create-group' | 'edit-group' }> {
  return request.kind === 'create-group' || request.kind === 'edit-group';
}

function titleKey(request: ActionDialogRequest): string {
  switch (request.kind) {
    case 'rename-session':
      return 'dialog.renameSessionTitle';
    case 'delete-session':
      return 'dialog.deleteSessionTitle';
    case 'delete-group':
      return 'dialog.deleteGroupTitle';
    case 'remove-group-member':
      return 'dialog.removeMemberTitle';
    case 'create-group':
      return 'dialog.createGroupTitle';
    case 'edit-group':
      return 'dialog.editGroupTitle';
  }
}

function submitLabelKey(request: ActionDialogRequest): string {
  switch (request.kind) {
    case 'delete-session':
    case 'delete-group':
      return 'common.delete';
    case 'remove-group-member':
      return 'common.remove';
    case 'create-group':
      return 'dialog.createGroupAction';
    default:
      return 'common.save';
  }
}

function isDangerRequest(request: ActionDialogRequest): boolean {
  return (
    request.kind === 'delete-session' ||
    request.kind === 'delete-group' ||
    request.kind === 'remove-group-member'
  );
}

function errorDetail(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function createButton(label: string, className: string): HTMLButtonElement {
  const button = document.createElement('button');
  button.type = 'button';
  button.className = className;
  button.textContent = label;
  return button;
}

function createEntitySummary(
  request: Extract<ActionDialogRequest, EntityRequestBase>,
): HTMLElement {
  const summary = document.createElement('div');
  summary.className = 'action-dialog-entity';
  const name = document.createElement('strong');
  name.textContent = request.entityName || request.entityId;
  const id = document.createElement('code');
  id.textContent = request.entityId;
  summary.append(name, id);
  return summary;
}

function syncSelectedMemberCount(dialog: ActiveDialog): void {
  const count = dialog.overlay.querySelector<HTMLElement>('.action-dialog-members-header span');
  if (count) {
    count.textContent = tr('dialog.membersSelected', { count: dialog.selectedMembers.size });
  }
}

function clearDialogError(dialog: ActiveDialog): void {
  dialog.error = '';
  dialog.errorKind = '';
  dialog.errorDetail = '';
  dialog.panel.removeAttribute('aria-describedby');
  dialog.panel.querySelectorAll<HTMLElement>('[aria-invalid="true"]').forEach((element) => {
    element.removeAttribute('aria-invalid');
    element.removeAttribute('aria-describedby');
  });
  const error = dialog.overlay.querySelector<HTMLElement>('.action-dialog-error');
  if (error) {
    error.hidden = true;
    error.textContent = '';
  }
}

function renderMemberOptions(dialog: ActiveDialog, container: HTMLElement): void {
  container.replaceChildren();
  if (!isGroupRequest(dialog.request)) return;
  const query = dialog.search.trim().toLocaleLowerCase();
  const sessions = dialog.request.sessions.filter((session) => {
    if (!query) return true;
    return `${session.name}\n${session.id}`.toLocaleLowerCase().includes(query);
  });
  if (sessions.length === 0) {
    const empty = document.createElement('p');
    empty.className = 'action-dialog-members-empty';
    empty.textContent = tr(
      dialog.request.sessions.length === 0 ? 'dialog.noAvailableMembers' : 'dialog.noMemberMatches',
    );
    container.appendChild(empty);
    return;
  }
  for (const session of sessions) {
    const label = document.createElement('label');
    label.className = 'action-dialog-member-option';
    const checkbox = document.createElement('input');
    checkbox.type = 'checkbox';
    checkbox.value = session.id;
    checkbox.checked = dialog.selectedMembers.has(session.id);
    checkbox.disabled = dialog.busy;
    checkbox.addEventListener('change', () => {
      if (checkbox.checked) dialog.selectedMembers.add(session.id);
      else dialog.selectedMembers.delete(session.id);
      clearDialogError(dialog);
      syncSelectedMemberCount(dialog);
    });
    const copy = document.createElement('span');
    copy.className = 'action-dialog-member-copy';
    const name = document.createElement('strong');
    name.textContent = session.name || session.id;
    const id = document.createElement('code');
    id.textContent = session.id;
    copy.append(name, id);
    label.append(checkbox, copy);
    container.appendChild(label);
  }
}

function renderDialog(dialog: ActiveDialog, focusRole = ''): void {
  const { request, panel } = dialog;
  panel.replaceChildren();
  panel.setAttribute('aria-busy', String(dialog.busy));
  if (dialog.error) panel.setAttribute('aria-describedby', 'action-dialog-error');
  else panel.removeAttribute('aria-describedby');

  const header = document.createElement('header');
  header.className = 'action-dialog-header';
  const heading = document.createElement('div');
  const eyebrow = document.createElement('span');
  eyebrow.className = 'action-dialog-eyebrow';
  eyebrow.textContent = tr('dialog.eyebrow');
  const title = document.createElement('h2');
  title.id = 'action-dialog-title';
  title.textContent = tr(titleKey(request));
  heading.append(eyebrow, title);
  const close = createButton('', 'action-dialog-close');
  close.dataset.focusRole = 'close';
  close.appendChild(createIcon('close'));
  close.setAttribute('aria-label', tr('common.close'));
  close.disabled = dialog.busy;
  close.addEventListener('click', () => closeActionDialog(null));
  header.append(heading, close);

  const form = document.createElement('form');
  form.className = 'action-dialog-form';
  form.noValidate = true;

  if (request.kind === 'rename-session') {
    const field = document.createElement('label');
    field.className = 'action-dialog-field';
    const fieldLabel = document.createElement('span');
    fieldLabel.textContent = tr('dialog.sessionNameLabel');
    const input = document.createElement('input');
    input.type = 'text';
    input.required = true;
    input.autocomplete = 'off';
    input.value = dialog.name;
    input.disabled = dialog.busy;
    input.dataset.focusRole = 'primary';
    if (dialog.errorKind === 'name') {
      input.setAttribute('aria-invalid', 'true');
      input.setAttribute('aria-describedby', 'action-dialog-error');
    }
    input.addEventListener('input', () => {
      dialog.name = input.value;
      clearDialogError(dialog);
    });
    field.append(fieldLabel, input);
    form.append(field, createEntitySummary(request));
  } else if (isGroupRequest(request)) {
    const nameField = document.createElement('label');
    nameField.className = 'action-dialog-field';
    const nameLabel = document.createElement('span');
    nameLabel.textContent = tr('dialog.groupNameLabel');
    const nameInput = document.createElement('input');
    nameInput.type = 'text';
    nameInput.required = true;
    nameInput.autocomplete = 'off';
    nameInput.value = dialog.name;
    nameInput.disabled = dialog.busy;
    nameInput.dataset.focusRole = 'primary';
    if (dialog.errorKind === 'name') {
      nameInput.setAttribute('aria-invalid', 'true');
      nameInput.setAttribute('aria-describedby', 'action-dialog-error');
    }
    nameInput.addEventListener('input', () => {
      dialog.name = nameInput.value;
      clearDialogError(dialog);
    });
    nameField.append(nameLabel, nameInput);

    const owner = document.createElement('div');
    owner.className = 'action-dialog-owner';
    owner.appendChild(createIcon('user-node'));
    const ownerCopy = document.createElement('span');
    const ownerTitle = document.createElement('strong');
    ownerTitle.textContent = tr('dialog.mainOwner');
    const ownerHint = document.createElement('small');
    ownerHint.textContent = tr('dialog.mainOwnerHint');
    ownerCopy.append(ownerTitle, ownerHint);
    owner.appendChild(ownerCopy);

    const membersHeader = document.createElement('div');
    membersHeader.className = 'action-dialog-members-header';
    const membersLabel = document.createElement('strong');
    membersLabel.textContent = tr('dialog.membersLabel');
    const selectedCount = document.createElement('span');
    selectedCount.textContent = tr('dialog.membersSelected', {
      count: dialog.selectedMembers.size,
    });
    membersHeader.append(membersLabel, selectedCount);

    const searchWrap = document.createElement('label');
    searchWrap.className = 'action-dialog-search';
    searchWrap.appendChild(createIcon('search'));
    const search = document.createElement('input');
    search.type = 'search';
    search.autocomplete = 'off';
    search.placeholder = tr('dialog.searchMembers');
    search.setAttribute('aria-label', tr('dialog.searchMembers'));
    search.value = dialog.search;
    search.disabled = dialog.busy;
    search.dataset.focusRole = 'search';
    const members = document.createElement('div');
    members.className = 'action-dialog-members';
    members.setAttribute('role', 'group');
    members.setAttribute('aria-label', tr('dialog.membersLabel'));
    members.setAttribute('aria-required', 'true');
    if (dialog.errorKind === 'members') {
      members.setAttribute('aria-invalid', 'true');
      members.setAttribute('aria-describedby', 'action-dialog-error');
    }
    search.addEventListener('input', () => {
      dialog.search = search.value;
      renderMemberOptions(dialog, members);
    });
    searchWrap.appendChild(search);
    renderMemberOptions(dialog, members);
    form.append(nameField, owner, membersHeader, searchWrap, members);
  } else {
    const description = document.createElement('p');
    description.className = 'action-dialog-description';
    const key =
      request.kind === 'delete-session'
        ? 'dialog.deleteSessionDescription'
        : request.kind === 'delete-group'
          ? 'dialog.deleteGroupDescription'
          : 'dialog.removeMemberDescription';
    description.textContent = tr(key, { name: request.entityName || request.entityId });
    form.append(description, createEntitySummary(request));
    if (request.kind !== 'remove-group-member') {
      const warning = document.createElement('p');
      warning.className = 'action-dialog-warning';
      warning.append(
        createIcon('alert-triangle'),
        document.createTextNode(tr('dialog.irreversible')),
      );
      form.appendChild(warning);
    }
  }

  const error = document.createElement('p');
  error.className = 'action-dialog-error';
  error.id = 'action-dialog-error';
  error.setAttribute('role', 'alert');
  error.hidden = !dialog.error;
  error.textContent = dialog.error;
  form.appendChild(error);

  const footer = document.createElement('footer');
  footer.className = 'action-dialog-footer';
  const cancel = createButton(tr('common.cancel'), 'action-dialog-button action-dialog-cancel');
  cancel.dataset.focusRole = 'cancel';
  cancel.disabled = dialog.busy;
  cancel.addEventListener('click', () => closeActionDialog(null));
  const submit = document.createElement('button');
  submit.type = 'submit';
  submit.className = `action-dialog-button action-dialog-submit${isDangerRequest(request) ? ' danger' : ''}`;
  submit.dataset.focusRole = 'submit';
  submit.disabled = dialog.busy;
  submit.textContent = tr(dialog.busy ? 'dialog.working' : submitLabelKey(request));
  footer.append(cancel, submit);
  form.appendChild(footer);
  form.addEventListener('submit', (event) => {
    event.preventDefault();
    void submitActionDialog();
  });

  panel.append(header, form);
  const nextFocus = panel.querySelector<HTMLElement>(
    `[data-focus-role="${focusRole || 'primary'}"]`,
  );
  queueMicrotask(() => {
    if (activeDialog !== dialog) return;
    if (nextFocus && !nextFocus.matches(':disabled')) {
      nextFocus.focus();
    } else {
      panel.focus();
    }
  });
}

function buildResult(dialog: ActiveDialog): ActionDialogResult | null {
  const { request } = dialog;
  if (request.kind === 'rename-session') {
    const name = dialog.name.trim();
    if (!name) {
      dialog.error = tr('dialog.nameRequired');
      dialog.errorKind = 'name';
      return null;
    }
    return { kind: request.kind, name };
  }
  if (isGroupRequest(request)) {
    const name = dialog.name.trim();
    if (!name) {
      dialog.error = tr('dialog.nameRequired');
      dialog.errorKind = 'name';
      return null;
    }
    const validIds = new Set(request.sessions.map((session) => session.id));
    const members = [...dialog.selectedMembers].filter((member) => validIds.has(member));
    if (members.length === 0) {
      dialog.error = tr('dialog.memberRequired');
      dialog.errorKind = 'members';
      return null;
    }
    return { kind: request.kind, name, members };
  }
  return { kind: request.kind };
}

async function runSubmit(request: ActionDialogRequest, result: ActionDialogResult): Promise<void> {
  switch (request.kind) {
    case 'rename-session':
      if (result.kind === request.kind) await request.submit(result.name);
      break;
    case 'create-group':
    case 'edit-group':
      if (result.kind === request.kind) {
        await request.submit({ name: result.name, members: result.members });
      }
      break;
    case 'delete-session':
    case 'delete-group':
    case 'remove-group-member':
      await request.submit();
      break;
  }
}

async function submitActionDialog(): Promise<void> {
  const dialog = activeDialog;
  if (!dialog || dialog.busy) return;
  const result = buildResult(dialog);
  if (!result) {
    renderDialog(dialog, dialog.errorKind === 'members' ? 'search' : 'primary');
    return;
  }
  dialog.busy = true;
  dialog.error = '';
  dialog.errorKind = '';
  dialog.errorDetail = '';
  renderDialog(dialog, 'submit');
  try {
    await runSubmit(dialog.request, result);
    if (activeDialog === dialog) {
      dialog.busy = false;
      closeActionDialog(result);
    }
  } catch (error) {
    if (activeDialog !== dialog) return;
    dialog.busy = false;
    dialog.errorKind = 'submit';
    dialog.errorDetail = errorDetail(error);
    dialog.error = tr('dialog.submitError', { error: dialog.errorDetail });
    renderDialog(dialog, 'submit');
  }
}

function closeActionDialog(result: ActionDialogResult | null, force = false): void {
  const dialog = activeDialog;
  if (!dialog || (dialog.busy && !force)) return;
  activeDialog = null;
  dialog.overlay.remove();
  document.body.classList.remove('action-dialog-open');
  dialog.resolve(result);
  queueMicrotask(() => {
    if (dialog.restoreFocus?.isConnected) {
      dialog.restoreFocus.focus();
      return;
    }
    document.querySelector<HTMLElement>('.group-members-toggle, #input')?.focus();
  });
}

export function openActionDialog(request: ActionDialogRequest): Promise<ActionDialogResult | null> {
  if (activeDialog) {
    activeDialog.panel.focus();
    return Promise.resolve(null);
  }
  const overlay = document.createElement('div');
  overlay.className = 'action-dialog-overlay';
  const panel = document.createElement('section');
  panel.className = 'action-dialog-panel';
  panel.setAttribute('role', 'dialog');
  panel.setAttribute('aria-modal', 'true');
  panel.setAttribute('aria-labelledby', 'action-dialog-title');
  panel.tabIndex = -1;
  overlay.appendChild(panel);
  appendWorkspacePortal(overlay);
  document.body.classList.add('action-dialog-open');

  return new Promise((resolve) => {
    const validMemberIds = isGroupRequest(request)
      ? new Set(request.sessions.map((session) => session.id))
      : new Set<string>();
    const initialMembers = isGroupRequest(request)
      ? new Set(
          request.selectedMembers.filter(
            (member) => member !== 'main' && validMemberIds.has(member),
          ),
        )
      : new Set<string>();
    const dialog: ActiveDialog = {
      request,
      overlay,
      panel,
      restoreFocus: document.activeElement instanceof HTMLElement ? document.activeElement : null,
      resolve,
      busy: false,
      name: isGroupRequest(request) ? request.initialName : request.entityName,
      search: '',
      selectedMembers: initialMembers,
      error: '',
      errorKind: '',
      errorDetail: '',
    };
    activeDialog = dialog;
    overlay.addEventListener('click', (event) => {
      if (event.target === overlay && !dialog.busy) closeActionDialog(null);
    });
    overlay.addEventListener('keydown', (event) => {
      event.stopPropagation();
      if (event.key === 'Escape' && !dialog.busy) {
        event.preventDefault();
        closeActionDialog(null);
        return;
      }
      if (event.key === 'Enter' && event.target === panel && !dialog.busy) {
        event.preventDefault();
        void submitActionDialog();
        return;
      }
      trapDialogFocus(event, panel);
    });
    renderDialog(dialog);
  });
}

export function refreshActionDialog(): void {
  if (!activeDialog) return;
  if (activeDialog.errorKind === 'name') {
    activeDialog.error = tr('dialog.nameRequired');
  } else if (activeDialog.errorKind === 'members') {
    activeDialog.error = tr('dialog.memberRequired');
  } else if (activeDialog.errorKind === 'submit') {
    activeDialog.error = tr('dialog.submitError', { error: activeDialog.errorDetail });
  }
  const focused = document.activeElement;
  const focusRole =
    focused instanceof HTMLElement && activeDialog.panel.contains(focused)
      ? focused.dataset.focusRole || ''
      : '';
  renderDialog(activeDialog, focusRole);
}

export function hasOpenActionDialog(): boolean {
  return activeDialog !== null;
}

export function dismissActionDialog(): boolean {
  if (!activeDialog || activeDialog.busy) return false;
  closeActionDialog(null);
  return true;
}

export function forceDismissActionDialog(): boolean {
  if (!activeDialog) return false;
  closeActionDialog(null, true);
  return true;
}
