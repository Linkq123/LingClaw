import { createIcon } from './icons.js';
import { tr } from './i18n.js';
import { trapDialogFocus } from './pages/dialogFocus.js';
import { appendWorkspacePortal } from './workspacePortal.js';

export interface ActionDialogSessionOption {
  id: string;
  name: string;
}

export type ActionDialogWorkspace = { kind: 'managed' } | { kind: 'directory'; path: string };

export interface ActionDialogBrowseResult {
  current: string;
  parent: string | null;
  home: string | null;
  roots: string[];
  directories: Array<{ name: string; path: string }>;
}

type MaybePromise<T> = T | Promise<T>;

type EntityRequestBase = {
  entityId: string;
  entityName: string;
};

export type ActionDialogRequest =
  | {
      kind: 'create-session' | 'edit-session';
      sessionId?: string;
      initialName: string;
      initialWorkspace: ActionDialogWorkspace;
      browse: (path?: string) => MaybePromise<ActionDialogBrowseResult>;
      submit: (value: { name: string; workspace: ActionDialogWorkspace }) => MaybePromise<void>;
    }
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
  | {
      kind: 'create-session' | 'edit-session';
      name: string;
      workspace: ActionDialogWorkspace;
    }
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
  workspaceKind: 'managed' | 'directory';
  workspacePath: string;
  workspaceBrowse: ActionDialogBrowseResult | null;
  workspaceBrowseBusy: boolean;
  workspaceBrowseFailed: boolean;
  workspaceBroadConfirmed: boolean;
  error: string;
  errorKind: 'name' | 'members' | 'workspace' | 'submit' | '';
  errorDetail: string;
};

let activeDialog: ActiveDialog | null = null;

function isGroupRequest(
  request: ActionDialogRequest,
): request is Extract<ActionDialogRequest, { kind: 'create-group' | 'edit-group' }> {
  return request.kind === 'create-group' || request.kind === 'edit-group';
}

function isSessionEditorRequest(
  request: ActionDialogRequest,
): request is Extract<ActionDialogRequest, { kind: 'create-session' | 'edit-session' }> {
  return request.kind === 'create-session' || request.kind === 'edit-session';
}

function titleKey(request: ActionDialogRequest): string {
  switch (request.kind) {
    case 'create-session':
      return 'dialog.createSessionTitle';
    case 'edit-session':
      return 'dialog.editSessionTitle';
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
    case 'create-session':
      return 'dialog.createSessionAction';
    default:
      return 'common.save';
  }
}

function normalizeComparablePath(value: string): string {
  const slashed = value.trim().replace(/\\/g, '/');
  if (slashed === '/') return '/';
  if (/^[a-zA-Z]:\/+$/u.test(slashed)) return `${slashed.slice(0, 2)}/`;
  return slashed.replace(/\/+$/u, '');
}

function workspacePathsMatch(left: string, right: string): boolean {
  const normalizedLeft = normalizeComparablePath(left);
  const normalizedRight = normalizeComparablePath(right);
  const windowsLike = (value: string): boolean =>
    /^[a-zA-Z]:\//u.test(value) || value.startsWith('//');
  if (windowsLike(normalizedLeft) && windowsLike(normalizedRight)) {
    return normalizedLeft.toLocaleLowerCase() === normalizedRight.toLocaleLowerCase();
  }
  return normalizedLeft === normalizedRight;
}

function workspaceBindingChanged(dialog: ActiveDialog): boolean {
  if (!isSessionEditorRequest(dialog.request) || dialog.request.kind === 'create-session') {
    return true;
  }
  const initial = dialog.request.initialWorkspace;
  if (initial.kind !== dialog.workspaceKind) return true;
  return initial.kind === 'directory' && !workspacePathsMatch(initial.path, dialog.workspacePath);
}

function workspaceIsBroad(dialog: ActiveDialog): boolean {
  if (dialog.workspaceKind !== 'directory' || !dialog.workspacePath.trim()) return false;
  const normalized = normalizeComparablePath(dialog.workspacePath);
  if (normalized === '/' || /^[a-zA-Z]:\/$/.test(normalized)) return true;
  const home = dialog.workspaceBrowse?.home;
  if (!home) return false;
  return workspacePathsMatch(normalized, home);
}

function workspaceMetadataUnavailable(dialog: ActiveDialog): boolean {
  if (
    !isSessionEditorRequest(dialog.request) ||
    dialog.workspaceKind !== 'directory' ||
    workspaceIsBroad(dialog)
  ) {
    return false;
  }
  if (dialog.workspaceBrowseBusy) return true;
  if (dialog.workspaceBrowse) return false;
  return workspaceBindingChanged(dialog) || !dialog.workspaceBrowseFailed;
}

async function browseWorkspace(dialog: ActiveDialog, path?: string): Promise<void> {
  if (!isSessionEditorRequest(dialog.request) || dialog.workspaceBrowseBusy) return;
  dialog.workspaceBrowseBusy = true;
  dialog.workspaceBrowseFailed = false;
  clearDialogError(dialog);
  renderDialog(dialog, 'workspace-path');
  try {
    const result = await dialog.request.browse(path || dialog.workspacePath || undefined);
    if (activeDialog !== dialog) return;
    dialog.workspaceBrowse = result;
    dialog.workspaceBrowseFailed = false;
    dialog.workspacePath = result.current;
    dialog.workspaceBroadConfirmed = false;
  } catch (error) {
    if (activeDialog !== dialog) return;
    dialog.errorKind = 'workspace';
    dialog.workspaceBrowseFailed = true;
    dialog.errorDetail = errorDetail(error);
    dialog.error = tr('dialog.workspaceBrowseError', { error: dialog.errorDetail });
  } finally {
    if (activeDialog === dialog) {
      dialog.workspaceBrowseBusy = false;
      renderDialog(dialog, 'workspace-path');
    }
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

function renderSessionEditorFields(dialog: ActiveDialog, form: HTMLFormElement): void {
  if (!isSessionEditorRequest(dialog.request)) return;

  const nameField = document.createElement('label');
  nameField.className = 'action-dialog-field';
  const nameLabel = document.createElement('span');
  nameLabel.textContent = tr('dialog.sessionNameLabel');
  const nameInput = document.createElement('input');
  nameInput.type = 'text';
  nameInput.required = true;
  nameInput.autocomplete = 'off';
  nameInput.value = dialog.name;
  nameInput.disabled = dialog.busy;
  nameInput.dataset.focusRole = 'primary';
  if (dialog.errorKind === 'name') nameInput.setAttribute('aria-invalid', 'true');
  nameInput.addEventListener('input', () => {
    dialog.name = nameInput.value;
    clearDialogError(dialog);
  });
  nameField.append(nameLabel, nameInput);

  const mode = document.createElement('fieldset');
  mode.className = 'action-dialog-workspace-mode';
  const legend = document.createElement('legend');
  legend.textContent = tr('dialog.workspaceLabel');
  mode.appendChild(legend);
  for (const kind of ['managed', 'directory'] as const) {
    const label = document.createElement('label');
    const input = document.createElement('input');
    input.type = 'radio';
    input.name = 'action-dialog-workspace-kind';
    input.value = kind;
    input.checked = dialog.workspaceKind === kind;
    input.disabled = dialog.busy;
    input.addEventListener('change', () => {
      if (!input.checked) return;
      dialog.workspaceKind = kind;
      dialog.workspaceBrowseFailed = false;
      dialog.workspaceBroadConfirmed = false;
      clearDialogError(dialog);
      renderDialog(dialog, kind === 'directory' ? 'workspace-path' : 'primary');
      if (kind === 'directory' && !dialog.workspaceBrowse) {
        void browseWorkspace(dialog);
      }
    });
    const copy = document.createElement('span');
    const title = document.createElement('strong');
    title.textContent = tr(
      kind === 'managed' ? 'dialog.workspaceManaged' : 'dialog.workspaceDirectory',
    );
    const hint = document.createElement('small');
    hint.textContent = tr(
      kind === 'managed' ? 'dialog.workspaceManagedHint' : 'dialog.workspaceDirectoryHint',
    );
    copy.append(title, hint);
    label.append(input, copy);
    mode.appendChild(label);
  }
  form.append(nameField, mode);

  if (dialog.workspaceKind !== 'directory') return;

  const pathField = document.createElement('div');
  pathField.className = 'action-dialog-workspace-path';
  const pathLabel = document.createElement('label');
  pathLabel.htmlFor = 'action-dialog-workspace-path';
  pathLabel.textContent = tr('dialog.workspacePath');
  const pathRow = document.createElement('div');
  const pathInput = document.createElement('input');
  pathInput.id = 'action-dialog-workspace-path';
  pathInput.type = 'text';
  pathInput.value = dialog.workspacePath;
  pathInput.placeholder = tr('dialog.workspacePathPlaceholder');
  pathInput.disabled = dialog.busy || dialog.workspaceBrowseBusy;
  pathInput.dataset.focusRole = 'workspace-path';
  if (dialog.errorKind === 'workspace') pathInput.setAttribute('aria-invalid', 'true');
  pathInput.addEventListener('input', () => {
    dialog.workspacePath = pathInput.value;
    if (
      dialog.workspaceBrowse &&
      !workspacePathsMatch(dialog.workspacePath, dialog.workspaceBrowse.current)
    ) {
      dialog.workspaceBrowse = null;
    }
    dialog.workspaceBrowseFailed = false;
    dialog.workspaceBroadConfirmed = false;
    clearDialogError(dialog);
  });
  const browse = createButton(
    tr(dialog.workspaceBrowseBusy ? 'dialog.workspaceBrowsing' : 'dialog.workspaceBrowse'),
    'action-dialog-workspace-browse',
  );
  browse.disabled = dialog.busy || dialog.workspaceBrowseBusy;
  browse.addEventListener('click', () => void browseWorkspace(dialog));
  pathRow.append(pathInput, browse);
  pathField.append(pathLabel, pathRow);

  const browser = document.createElement('div');
  browser.className = 'action-dialog-workspace-browser';
  const browseResult = dialog.workspaceBrowse;
  if (browseResult) {
    const nav = document.createElement('div');
    nav.className = 'action-dialog-workspace-nav';
    if (browseResult.parent) {
      const parent = createButton(
        tr('dialog.workspaceParent'),
        'action-dialog-workspace-nav-button',
      );
      parent.disabled = dialog.busy || dialog.workspaceBrowseBusy;
      parent.addEventListener('click', () => void browseWorkspace(dialog, browseResult.parent!));
      nav.appendChild(parent);
    }
    if (browseResult.home && browseResult.home !== browseResult.current) {
      const home = createButton(tr('dialog.workspaceHome'), 'action-dialog-workspace-nav-button');
      home.disabled = dialog.busy || dialog.workspaceBrowseBusy;
      home.title = browseResult.home;
      home.addEventListener('click', () => void browseWorkspace(dialog, browseResult.home!));
      nav.appendChild(home);
    }
    for (const root of browseResult.roots) {
      const rootButton = createButton(root, 'action-dialog-workspace-nav-button');
      rootButton.disabled = dialog.busy || dialog.workspaceBrowseBusy;
      rootButton.title = root;
      rootButton.addEventListener('click', () => void browseWorkspace(dialog, root));
      nav.appendChild(rootButton);
    }
    browser.appendChild(nav);
    const list = document.createElement('div');
    list.className = 'action-dialog-workspace-list';
    if (browseResult.directories.length === 0) {
      const empty = document.createElement('p');
      empty.textContent = tr('dialog.workspaceNoDirectories');
      list.appendChild(empty);
    } else {
      for (const directory of browseResult.directories) {
        const item = createButton(directory.name, 'action-dialog-workspace-directory');
        item.disabled = dialog.busy || dialog.workspaceBrowseBusy;
        item.title = directory.path;
        item.addEventListener('click', () => void browseWorkspace(dialog, directory.path));
        list.appendChild(item);
      }
    }
    browser.appendChild(list);
  }

  form.append(pathField, browser);
  if (workspaceIsBroad(dialog)) {
    const confirm = document.createElement('label');
    confirm.className = 'action-dialog-workspace-warning';
    const checkbox = document.createElement('input');
    checkbox.type = 'checkbox';
    checkbox.checked = dialog.workspaceBroadConfirmed;
    checkbox.disabled = dialog.busy;
    checkbox.addEventListener('change', () => {
      dialog.workspaceBroadConfirmed = checkbox.checked;
      clearDialogError(dialog);
    });
    confirm.append(checkbox, document.createTextNode(tr('dialog.workspaceBroadConfirm')));
    form.appendChild(confirm);
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

  if (isSessionEditorRequest(request)) {
    renderSessionEditorFields(dialog, form);
  } else if (request.kind === 'rename-session') {
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
  submit.disabled = dialog.busy || workspaceMetadataUnavailable(dialog);
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
  if (isSessionEditorRequest(request)) {
    const name = dialog.name.trim();
    if (!name) {
      dialog.error = tr('dialog.nameRequired');
      dialog.errorKind = 'name';
      return null;
    }
    if (dialog.workspaceKind === 'directory') {
      const path = dialog.workspacePath.trim();
      if (!path) {
        dialog.error = tr('dialog.workspacePathRequired');
        dialog.errorKind = 'workspace';
        return null;
      }
      if (workspaceIsBroad(dialog) && !dialog.workspaceBroadConfirmed) {
        dialog.error = tr('dialog.workspaceBroadRequired');
        dialog.errorKind = 'workspace';
        return null;
      }
      return { kind: request.kind, name, workspace: { kind: 'directory', path } };
    }
    return { kind: request.kind, name, workspace: { kind: 'managed' } };
  }
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
    case 'create-session':
    case 'edit-session':
      if (result.kind === request.kind) {
        await request.submit({ name: result.name, workspace: result.workspace });
      }
      break;
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
  if (workspaceMetadataUnavailable(dialog)) {
    await browseWorkspace(dialog, dialog.workspacePath);
    if (activeDialog !== dialog || workspaceMetadataUnavailable(dialog)) return;
  }
  const result = buildResult(dialog);
  if (!result) {
    renderDialog(
      dialog,
      dialog.errorKind === 'members'
        ? 'search'
        : dialog.errorKind === 'workspace'
          ? 'workspace-path'
          : 'primary',
    );
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
    const initialWorkspace = isSessionEditorRequest(request)
      ? request.initialWorkspace
      : ({ kind: 'managed' } as const);
    const dialog: ActiveDialog = {
      request,
      overlay,
      panel,
      restoreFocus: document.activeElement instanceof HTMLElement ? document.activeElement : null,
      resolve,
      busy: false,
      name: isSessionEditorRequest(request)
        ? request.initialName
        : isGroupRequest(request)
          ? request.initialName
          : request.entityName,
      search: '',
      selectedMembers: initialMembers,
      workspaceKind: initialWorkspace.kind,
      workspacePath: initialWorkspace.kind === 'directory' ? initialWorkspace.path : '',
      workspaceBrowse: null,
      workspaceBrowseBusy: false,
      workspaceBrowseFailed: false,
      workspaceBroadConfirmed: false,
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
    if (isSessionEditorRequest(request) && initialWorkspace.kind === 'directory') {
      void browseWorkspace(dialog, initialWorkspace.path);
    }
  });
}

export function refreshActionDialog(): void {
  if (!activeDialog) return;
  if (activeDialog.errorKind === 'name') {
    activeDialog.error = tr('dialog.nameRequired');
  } else if (activeDialog.errorKind === 'members') {
    activeDialog.error = tr('dialog.memberRequired');
  } else if (activeDialog.errorKind === 'workspace') {
    activeDialog.error = activeDialog.errorDetail
      ? tr('dialog.workspaceBrowseError', { error: activeDialog.errorDetail })
      : workspaceIsBroad(activeDialog) && !activeDialog.workspaceBroadConfirmed
        ? tr('dialog.workspaceBroadRequired')
        : tr('dialog.workspacePathRequired');
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
