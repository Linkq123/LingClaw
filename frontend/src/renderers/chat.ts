import { dom, state } from '../state.js';
import { DEFAULT_BRAND_AVATAR } from '../constants.js';
import { formatTime, hideWelcome } from '../utils.js';
import { scrollDown, queueUnreadContent, invalidateChatScrollCache } from '../scroll.js';
import { pinReactStatusToBottom } from './react-status.js';
import { tr } from '../i18n.js';
import { createIcon, iconMarkup } from '../icons.js';
import type { IconName } from '../icons.js';
import { syncComposerAvailability } from '../composerAvailability.js';

function setVersionBadge(el, version) {
  if (!el) return;
  if (!version) {
    el.hidden = true;
    el.textContent = '';
    return;
  }
  el.textContent = `v${version}`;
  el.hidden = false;
}

export function syncVersionBadges() {
  setVersionBadge(dom.headerVersionEl, state.currentAppVersion);
  setVersionBadge(document.getElementById('app-version-welcome'), state.currentAppVersion);
}

export async function loadAppVersion() {
  try {
    const response = await fetch('/api/health');
    if (!response.ok) return;
    const data = await response.json();
    if (typeof data.version !== 'string' || !data.version) return;
    state.currentAppVersion = data.version;
    syncVersionBadges();
  } catch {
    // Version is optional UI metadata; ignore fetch failures.
  }
}

function setAssistantAvatar(node) {
  node.replaceChildren();
  const img = document.createElement('img');
  img.src = DEFAULT_BRAND_AVATAR;
  img.alt = 'LingClaw avatar';
  img.style.cssText = 'width:100%;height:100%;border-radius:50%;object-fit:cover';
  img.onerror = () => {
    node.replaceChildren();
    node.textContent = 'LC';
    node.classList.add('is-text-fallback');
  };
  node.appendChild(img);
}

export function addMsg(cls, text, timestamp = undefined, options: { trackUnread?: boolean } = {}) {
  const { trackUnread = cls === 'assistant' } = options;
  const isChat = cls === 'user' || cls === 'assistant';
  const hasAvatar = cls === 'assistant';
  const row = document.createElement('div');
  row.className = `msg-row ${cls}`;

  if (hasAvatar) {
    const avatar = document.createElement('div');
    avatar.className = 'msg-avatar';
    setAssistantAvatar(avatar);
    row.appendChild(avatar);
  }

  const el = document.createElement('div');
  el.className = `msg ${cls}`;
  el.textContent = text;

  if (isChat) {
    const content = document.createElement('div');
    content.className = 'msg-content';
    content.appendChild(el);
    const timestampValue = timestamp === undefined ? Number.NaN : Number(timestamp);
    const timestampDate = new Date(timestampValue * 1000);
    const messageDate = Number.isFinite(timestampDate.getTime()) ? timestampDate : new Date();
    const time = document.createElement('time');
    time.className = 'msg-time';
    time.dateTime = messageDate.toISOString();
    time.textContent = formatTime(messageDate);
    content.appendChild(time);
    row.appendChild(content);
  } else {
    row.appendChild(el);
  }

  dom.chat.appendChild(row);
  invalidateChatScrollCache();
  if (trackUnread) {
    queueUnreadContent({ countable: true });
  }
  pinReactStatusToBottom();
  if (isChat) hideWelcome();
  scrollDown();
  return el;
}

export function addAssistant(text, options = {}) {
  return addMsg('assistant', text, undefined, options);
}

export function renderUserImageThumbnails(msgEl, images) {
  if (!images || images.length === 0) return;
  const container = document.createElement('div');
  container.className = 'user-images';
  for (const img of images) {
    const imgEl = document.createElement('img');
    // Defer decoding/fetching of off-screen user image thumbnails so long
    // scrollback doesn't eagerly load every historical attachment on page
    // load. `lazy` is a hint; browsers may still fetch when close to viewport.
    imgEl.loading = 'lazy';
    imgEl.decoding = 'async';
    imgEl.src = img.url;
    imgEl.alt = 'Attached image';
    imgEl.title = img.url;
    imgEl.onerror = () => {
      imgEl.style.display = 'none';
    };
    imgEl.onclick = () => window.open(img.url, '_blank', 'noopener');
    container.appendChild(imgEl);
  }
  const row = msgEl.closest('.msg-row');
  if (row) {
    const content = row.querySelector('.msg-content');
    if (content) {
      content.insertBefore(container, content.querySelector('.msg-time'));
      invalidateChatScrollCache();
    }
  }
}

function buildDismissButton(): HTMLButtonElement {
  const btn = document.createElement('button');
  btn.className = 'system-dismiss';
  btn.type = 'button';
  btn.dataset.action = 'dismiss-system-card';
  btn.setAttribute('aria-label', tr('common.dismiss'));
  btn.appendChild(createIcon('close'));
  return btn;
}

function buildIconSpan(icon: IconName): HTMLSpanElement {
  const span = document.createElement('span');
  span.className = 'system-icon';
  span.appendChild(createIcon(icon));
  return span;
}

function buildInlineText(text: string): HTMLSpanElement {
  const span = document.createElement('span');
  span.className = 'system-inline-text';
  span.textContent = text;
  return span;
}

export function addSystem(t, kind = 'info', options: { dismissible?: boolean } = {}) {
  const { dismissible = false } = options;
  const row = document.createElement('div');
  row.className = 'msg-row system';
  const card = document.createElement('div');
  card.className = 'system-card';
  if (dismissible) card.classList.add('is-dismissible');
  if (kind === 'success') card.classList.add('success-card');
  const icon: IconName = kind === 'success' ? 'check-circle' : 'info';
  const isBlock = t.includes('\n') || t.length > 80;
  if (isBlock) {
    const header = document.createElement('div');
    header.className = 'system-header';
    header.appendChild(buildIconSpan('clipboard'));
    const label = document.createElement('span');
    label.textContent = tr('common.system');
    header.appendChild(label);
    if (dismissible) header.appendChild(buildDismissButton());
    const body = document.createElement('pre');
    body.className = 'system-body';
    body.textContent = t;
    card.appendChild(header);
    card.appendChild(body);
  } else {
    card.classList.add('system-inline');
    card.appendChild(buildIconSpan(icon));
    card.appendChild(buildInlineText(t));
    if (dismissible) card.appendChild(buildDismissButton());
  }
  row.appendChild(card);
  dom.chat.appendChild(row);
  invalidateChatScrollCache();
  queueUnreadContent({ countable: true });
  pinReactStatusToBottom();
  scrollDown();
}

export function addError(t, options: { dismissible?: boolean } = {}) {
  const { dismissible = false } = options;
  const row = document.createElement('div');
  row.className = 'msg-row error';
  const card = document.createElement('div');
  card.className = 'system-card system-inline error-card';
  if (dismissible) card.classList.add('is-dismissible');
  card.appendChild(buildIconSpan('alert-triangle'));
  card.appendChild(buildInlineText(t));
  if (dismissible) card.appendChild(buildDismissButton());
  row.appendChild(card);
  dom.chat.appendChild(row);
  invalidateChatScrollCache();
  queueUnreadContent({ countable: true });
  pinReactStatusToBottom();
  scrollDown();
}

export function showWelcome() {
  if (document.getElementById('welcome')) return;
  const w = document.createElement('div');
  w.className = 'welcome';
  w.id = 'welcome';

  const logoDiv = document.createElement('div');
  logoDiv.className = 'welcome-brand-mark';
  const logoImg = document.createElement('img');
  logoImg.src = DEFAULT_BRAND_AVATAR;
  logoImg.alt = '';
  logoDiv.appendChild(logoImg);

  const title = document.createElement('h1');
  title.className = 'welcome-title';
  title.dataset.i18n = 'welcome.title';
  title.textContent = tr('welcome.title');

  // version badge (dynamic — uses textContent, no innerHTML)
  const versionBadgeClass = ['app-version-badge', 'welcome-version'].join(' ');
  const versionBadge = document.createElement('div');
  versionBadge.className = versionBadgeClass;
  versionBadge.id = 'app-version-welcome';
  if (state.currentAppVersion) {
    versionBadge.textContent = `v${state.currentAppVersion}`;
  } else {
    versionBadge.hidden = true;
  }

  const hint = document.createElement('p');
  hint.className = 'welcome-hint';
  const ready = document.createElement('span');
  ready.dataset.i18n = 'welcome.ready';
  ready.textContent = tr('welcome.ready');
  hint.append(ready);

  // welcome-shortcuts
  const shortcuts = document.createElement('div');
  shortcuts.className = 'welcome-shortcuts';
  const shortcutDefs: Array<[string, string, IconName]> = [
    ['/clear', 'common.newConversation', 'message'],
    ['/status', 'common.status', 'activity'],
    ['/help', 'common.help', 'help'],
  ];
  for (const [cmd, labelKey, icon] of shortcutDefs) {
    const btn = document.createElement('button');
    btn.dataset.action = 'cmd';
    btn.dataset.cmd = cmd;
    btn.disabled = state.storageMode === 'protected' && cmd === '/clear';
    if (btn.disabled) btn.dataset.storageProtectedDisabled = 'true';
    btn.appendChild(createIcon(icon));
    const text = document.createElement('span');
    text.dataset.i18n = labelKey;
    text.textContent = tr(labelKey);
    btn.appendChild(text);
    shortcuts.appendChild(btn);
  }

  w.appendChild(logoDiv);
  w.appendChild(title);
  w.appendChild(versionBadge);
  w.appendChild(hint);
  w.appendChild(shortcuts);

  dom.chat.appendChild(w);
  invalidateChatScrollCache();
  syncVersionBadges();
}

export function setBusy(b) {
  if (state.busy === b) return;
  state.busy = b;
  dom.stopBtn.style.display = b ? 'flex' : 'none';
  dom.stopBtn.disabled = !b;
  syncComposerAvailability();
  dom.sendIcon.innerHTML = iconMarkup('send');
  dom.sendBtn.setAttribute('aria-label', tr('composer.send'));
}
