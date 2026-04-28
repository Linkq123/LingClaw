export interface ModalHostElement extends HTMLElement {
  _modalHostParent?: HTMLElement | null;
  _modalHostNextSibling?: ChildNode | null;
  _modalHostPlaceholder?: HTMLElement | null;
}

type ModalHostConfig = {
  hostClass: string;
  placeholderClass: string;
};

type ModalBackdropConfig = {
  id: string;
  className: string;
  closeAction: string;
};

function stripDuplicateIds(root: HTMLElement) {
  root.removeAttribute('id');
  root.querySelectorAll('[id]').forEach((node) => {
    (node as HTMLElement).removeAttribute('id');
  });
}

function disablePlaceholderInteractivity(root: HTMLElement) {
  root.setAttribute('aria-hidden', 'true');
  root.setAttribute('inert', '');

  root.querySelectorAll('[data-action]').forEach((node) => {
    (node as HTMLElement).removeAttribute('data-action');
  });

  root
    .querySelectorAll('button, [href], input, select, textarea, [tabindex], [contenteditable="true"]')
    .forEach((node) => {
      if (node instanceof HTMLElement) {
        node.setAttribute('tabindex', '-1');
        node.setAttribute('aria-hidden', 'true');
      }
      if (
        node instanceof HTMLButtonElement ||
        node instanceof HTMLInputElement ||
        node instanceof HTMLSelectElement ||
        node instanceof HTMLTextAreaElement
      ) {
        node.disabled = true;
      }
    });
}

function trimPlaceholderContent(root: HTMLElement) {
  root.querySelectorAll('.subagent-body').forEach((node) => node.remove());
  root.querySelectorAll('.subagent-modal-open').forEach((node) => {
    node.classList.remove('subagent-modal-open');
  });
  root.querySelectorAll('.subagent-status, .subagent-modal-close, .chevron').forEach((node) => {
    node.remove();
  });
}

function createModalHostPlaceholder(
  host: ModalHostElement,
  config: ModalHostConfig,
  heightPx: string | null = null,
) {
  const placeholder = host.cloneNode(true) as HTMLElement;
  placeholder.classList.remove(config.hostClass);
  placeholder.classList.remove('subagent-modal-anchor');
  placeholder.classList.add(config.placeholderClass);
  placeholder.style.minHeight =
    heightPx || `${Math.max(host.getBoundingClientRect().height, 1)}px`;
  stripDuplicateIds(placeholder);
  trimPlaceholderContent(placeholder);
  disablePlaceholderInteractivity(placeholder);
  return placeholder;
}

export function syncModalHostPlaceholder(host: ModalHostElement | null, config: ModalHostConfig) {
  if (!host || !host.classList.contains(config.hostClass)) return;

  const currentPlaceholder = host._modalHostPlaceholder;
  const parent = currentPlaceholder?.parentNode;
  if (!currentPlaceholder || !parent) return;

  const preservedHeight =
    currentPlaceholder.style.minHeight ||
    `${Math.max(currentPlaceholder.getBoundingClientRect().height, 1)}px`;
  const nextPlaceholder = createModalHostPlaceholder(host, config, preservedHeight);
  parent.replaceChild(nextPlaceholder, currentPlaceholder);
  host._modalHostPlaceholder = nextPlaceholder;
}

export function moveModalHostToBody(host: ModalHostElement | null, config: ModalHostConfig) {
  if (!host || host.classList.contains(config.hostClass) || host._modalHostPlaceholder) {
    return;
  }

  const parent = host.parentElement;
  if (!parent) return;

  const placeholder = createModalHostPlaceholder(host, config);

  host._modalHostParent = parent;
  host._modalHostNextSibling = host.nextSibling;
  host._modalHostPlaceholder = placeholder;
  parent.replaceChild(placeholder, host);
  host.classList.add(config.hostClass);
  document.body.appendChild(host);
}

export function restoreModalHost(host: ModalHostElement | null, config: Pick<ModalHostConfig, 'hostClass'>) {
  if (!host || !host.classList.contains(config.hostClass)) return;

  const parent = host._modalHostParent;
  const nextSibling = host._modalHostNextSibling;
  const placeholder = host._modalHostPlaceholder;

  if (placeholder?.parentNode) {
    placeholder.parentNode.replaceChild(host, placeholder);
  } else if (parent) {
    if (nextSibling && nextSibling.parentNode === parent) {
      parent.insertBefore(host, nextSibling);
    } else {
      parent.appendChild(host);
    }
  }

  host.classList.remove(config.hostClass);
  host._modalHostParent = null;
  host._modalHostNextSibling = null;
  host._modalHostPlaceholder = null;
}

export function ensureModalBackdrop(config: ModalBackdropConfig) {
  let backdrop = document.getElementById(config.id);
  if (backdrop) return backdrop;

  backdrop = document.createElement('div');
  backdrop.id = config.id;
  backdrop.className = config.className;
  backdrop.dataset.action = config.closeAction;
  backdrop.hidden = true;
  document.body.appendChild(backdrop);
  return backdrop;
}
