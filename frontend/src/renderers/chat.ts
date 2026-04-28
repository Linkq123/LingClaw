import { dom, state } from '../state.js';
import { DEFAULT_BRAND_AVATAR, DEFAULT_WELCOME_LOGO } from '../constants.js';
import { formatTime, hideWelcome } from '../utils.js';
import { scrollDown, queueUnreadContent, invalidateChatScrollCache } from '../scroll.js';
import { pinReactStatusToBottom } from './react-status.js';

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
    node.textContent = '🦀';
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
    const time = document.createElement('div');
    time.className = 'msg-time';
    time.textContent = timestamp ? formatTime(new Date(timestamp * 1000)) : formatTime(new Date());
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
  btn.setAttribute('aria-label', 'Dismiss');
  btn.textContent = '×';
  return btn;
}

function buildIconSpan(icon: string): HTMLSpanElement {
  const span = document.createElement('span');
  span.className = 'system-icon';
  span.textContent = icon;
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
  const icon = kind === 'success' ? '✅' : 'ℹ️';
  const isBlock = t.includes('\n') || t.length > 80;
  if (isBlock) {
    const header = document.createElement('div');
    header.className = 'system-header';
    header.appendChild(buildIconSpan('📋'));
    const label = document.createElement('span');
    label.textContent = 'System';
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
  card.appendChild(buildIconSpan('⚠️'));
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

  // welcome-logo
  const logoDiv = document.createElement('div');
  logoDiv.className = 'welcome-logo';
  const logoImg = document.createElement('img');
  logoImg.src = DEFAULT_WELCOME_LOGO;
  logoImg.alt = 'LingClaw';
  logoDiv.appendChild(logoImg);

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

  // welcome-hint
  const hint = document.createElement('div');
  hint.className = 'welcome-hint';
  hint.appendChild(document.createTextNode('你的私人 AI 助手已就绪'));
  hint.appendChild(document.createElement('br'));
  hint.appendChild(document.createTextNode('输入消息开始对话，或使用 '));
  const slash = document.createElement('strong');
  slash.textContent = '/';
  hint.appendChild(slash);
  hint.appendChild(document.createTextNode(' 命令'));

  // welcome-shortcuts
  const shortcuts = document.createElement('div');
  shortcuts.className = 'welcome-shortcuts';
  const shortcutDefs: Array<[string, string]> = [
    ['/clear', 'New Conversation'],
    ['/status', 'Status'],
    ['/help', 'Help'],
  ];
  for (const [cmd, label] of shortcutDefs) {
    const btn = document.createElement('button');
    btn.dataset.action = 'cmd';
    btn.dataset.cmd = cmd;
    btn.textContent = label;
    shortcuts.appendChild(btn);
  }

  w.appendChild(logoDiv);
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
  dom.sendBtn.disabled = false;
  if (b) {
    dom.input.placeholder = 'Message LingClaw... (运行中可发送干预，点击红色按钮停止)';
  } else {
    dom.input.placeholder = 'Message LingClaw... (/ for commands)';
  }
  dom.sendIcon.innerHTML = '↑';
  dom.sendBtn.title = '';
  dom.sendBtn.setAttribute('aria-label', 'Send message');
}
