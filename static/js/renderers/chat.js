import { dom, state } from '../state.js';
import { DEFAULT_BRAND_AVATAR, DEFAULT_WELCOME_LOGO } from '../constants.js';
import { escHtml, formatTime, hideWelcome } from '../utils.js';
import { scrollDown, queueUnreadContent } from '../scroll.js';
import { pinReactStatusToBottom } from './react-status.js';

function versionBadgeMarkup(id, extraClass = '') {
  const className = ['app-version-badge', extraClass].filter(Boolean).join(' ');
  if (!state.currentAppVersion) {
    return `<div class="${className}" id="${id}" hidden></div>`;
  }
  return `<div class="${className}" id="${id}">v${state.currentAppVersion}</div>`;
}

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

export function addMsg(cls, text, timestamp, options = {}) {
  const { trackUnread = cls === 'assistant' } = options;
  const isChat = (cls === 'user' || cls === 'assistant');
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
    imgEl.src = img.url;
    imgEl.alt = 'Attached image';
    imgEl.title = img.url;
    imgEl.onerror = () => { imgEl.style.display = 'none'; };
    imgEl.onclick = () => window.open(img.url, '_blank', 'noopener');
    container.appendChild(imgEl);
  }
  const row = msgEl.closest('.msg-row');
  if (row) {
    const content = row.querySelector('.msg-content');
    if (content) {
      content.insertBefore(container, content.querySelector('.msg-time'));
    }
  }
}

function buildDismissButton() {
  return '<button class="system-dismiss" type="button" data-action="dismiss-system-card" aria-label="Dismiss">×</button>';
}

export function addSystem(t, kind = 'info', options = {}) {
  const { dismissible = false } = options;
  const row = document.createElement('div');
  row.className = 'msg-row system';
  const card = document.createElement('div');
  card.className = 'system-card';
  if (dismissible) card.classList.add('is-dismissible');
  const icon = kind === 'success' ? '✅' : 'ℹ️';
  if (kind === 'success') card.classList.add('success-card');
  const isBlock = t.includes('\n') || t.length > 80;
  if (isBlock) {
    card.innerHTML = `<div class="system-header"><span class="system-icon">📋</span><span>System</span>${dismissible ? buildDismissButton() : ''}</div><pre class="system-body">${escHtml(t)}</pre>`;
  } else {
    card.classList.add('system-inline');
    card.innerHTML = `<span class="system-icon">${icon}</span><span class="system-inline-text">${escHtml(t)}</span>${dismissible ? buildDismissButton() : ''}`;
  }
  row.appendChild(card);
  dom.chat.appendChild(row);
  queueUnreadContent({ countable: true });
  pinReactStatusToBottom();
  scrollDown();
}

export function addError(t, options = {}) {
  const { dismissible = false } = options;
  const row = document.createElement('div');
  row.className = 'msg-row error';
  const card = document.createElement('div');
  card.className = 'system-card system-inline error-card';
  if (dismissible) card.classList.add('is-dismissible');
  card.innerHTML = `<span class="system-icon">⚠️</span><span class="system-inline-text">${escHtml(t)}</span>${dismissible ? buildDismissButton() : ''}`;
  row.appendChild(card);
  dom.chat.appendChild(row);
  queueUnreadContent({ countable: true });
  pinReactStatusToBottom();
  scrollDown();
}

export function showWelcome() {
  if (document.getElementById('welcome')) return;
  const w = document.createElement('div');
  w.className = 'welcome';
  w.id = 'welcome';
  w.innerHTML = `
    <div class="welcome-logo"><img src="${DEFAULT_WELCOME_LOGO}" alt="LingClaw"></div>
    ${versionBadgeMarkup('app-version-welcome', 'welcome-version')}
    <div class="welcome-hint">
      你的私人 AI 助手已就绪<br>
      输入消息开始对话，或使用 <strong>/</strong> 命令
    </div>
    <div class="welcome-shortcuts">
      <button data-action="cmd" data-cmd="/clear">New Conversation</button>
      <button data-action="cmd" data-cmd="/status">Status</button>
      <button data-action="cmd" data-cmd="/help">Help</button>
    </div>
  `;
  dom.chat.appendChild(w);
  syncVersionBadges();
}

export function setBusy(b) {
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
