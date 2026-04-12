// ── State ──
const chat = document.getElementById('chat');
const input = document.getElementById('input');
const inputArea = document.getElementById('input-area');
const jumpToLatestBtn = document.getElementById('jump-to-latest');
const jumpToLatestBadge = document.getElementById('jump-to-latest-badge');
const stopBtn = document.getElementById('stop');
const sendBtn = document.getElementById('send');
const sendIcon = document.getElementById('send-icon');
const connDot = document.getElementById('conn-dot');
const connLabel = document.getElementById('conn-label');
const sessionNameEl = document.getElementById('session-name');
const sessionIdEl = document.getElementById('session-id');
const headerVersionEl = document.getElementById('app-version-header');
const toggleToolsBtn = document.getElementById('toggle-tools-btn');
const toggleReasoningBtn = document.getElementById('toggle-reasoning-btn');
const toolDrawer = document.getElementById('tool-drawer');
const toolDrawerBackdrop = document.getElementById('tool-drawer-backdrop');
const toolDrawerTitle = document.getElementById('tool-drawer-title');
const toolDrawerMeta = document.getElementById('tool-drawer-meta');
const toolDrawerArgs = document.getElementById('tool-drawer-args');
const toolDrawerResult = document.getElementById('tool-drawer-result');
const toolDrawerResultSection = document.getElementById('tool-drawer-result-section');
const DEFAULT_BRAND_AVATAR = 'branding/avatar.png';
const DEFAULT_WELCOME_LOGO = 'branding/logo-wordmark.png';
const AUTO_SCROLL_THRESHOLD = 88;
const SOFT_SPLIT_MIN_CHARS = 72;
const SOFT_SPLIT_MAX_CHARS = 160;
const SOFT_SPLIT_TAIL_MIN_CHARS = 18;

const attachBtn = document.getElementById('attach-btn');
const imagePreviewBar = document.getElementById('image-preview-bar');
const attachPopup = document.getElementById('attach-popup');
const attachMenu = document.getElementById('attach-menu');
const attachLocalBtn = document.getElementById('attach-local-btn');
const attachUrlBtn = document.getElementById('attach-url-btn');
const attachUrlInput = document.getElementById('attach-url-input');
const imageUrlField = document.getElementById('image-url-field');
const imageUrlAddBtn = document.getElementById('image-url-add');
const attachUploadStatus = document.getElementById('attach-upload-status');
const imageFileInput = document.getElementById('image-file-input');

let ws = null;
let currentMsg = null;
let busy = false;
let currentSessionId = '';
let reasoningPanel = null;
let reactStatusRow = null;
let reactStatusPhase = '';
let reactStatusCycle = null;
let reactStatusToolName = '';
let reactStatusElapsedMs = 0;
let reactPhaseShownAt = 0;
let reactPhaseTimer = 0;
let reactPhaseQueue = [];
let reactPendingClear = false;
let reconnectDelay = 1000;
let reconnectAttempts = 0;
const MAX_RECONNECT_ATTEMPTS = 50;
const MIN_REACT_ANALYZE_VISIBLE_MS = 180;
const MIN_REACT_ACT_VISIBLE_MS = 420;
const MIN_REACT_OBSERVE_VISIBLE_MS = 650;
const MAX_REACT_QUEUED_PHASES = 2;
let pendingAssistantText = '';
let pendingReasoningText = '';
let flushHandle = 0;
let _deferredHistory = [];
const HISTORY_RENDER_LIMIT = 50;
let activeToolPanel = null;
let showTools = true;
let showReasoning = true;
let autoFollowChat = true;
let hasBufferedChatUpdates = false;
let unreadMessageCount = 0;
let bulkRenderingChat = false;
let suppressScrollTracking = false;
let currentAppVersion = '';
let imageCapable = false;
let s3Capable = false;
let uploadToken = '';
let uploadTokenPromise = null;
let pendingImages = [];
const inputHistory = [];
const INPUT_HISTORY_MAX = 10;
let inputHistoryIndex = -1;
let inputHistoryDraft = '';
const markdownRenderQueue = [];
let markdownQueueHandle = 0;

function versionBadgeMarkup(id, extraClass = '') {
  const className = ['app-version-badge', extraClass].filter(Boolean).join(' ');
  if (!currentAppVersion) {
    return `<div class="${className}" id="${id}" hidden></div>`;
  }
  return `<div class="${className}" id="${id}">v${currentAppVersion}</div>`;
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

function syncVersionBadges() {
  setVersionBadge(headerVersionEl, currentAppVersion);
  setVersionBadge(document.getElementById('app-version-welcome'), currentAppVersion);
}

async function loadAppVersion() {
  try {
    const response = await fetch('/api/health');
    if (!response.ok) return;
    const data = await response.json();
    if (typeof data.version !== 'string' || !data.version) return;
    currentAppVersion = data.version;
    syncVersionBadges();
  } catch {
    // Version is optional UI metadata; ignore fetch failures.
  }
}

async function ensureUploadToken() {
  return ensureUploadTokenInternal(false);
}

async function ensureUploadTokenInternal(forceRefresh) {
  if (forceRefresh) {
    uploadToken = '';
  }
  if (uploadToken) return uploadToken;
  if (!uploadTokenPromise) {
    uploadTokenPromise = fetch('/api/client-config', { cache: 'no-store' })
      .then(async (response) => {
        if (!response.ok) {
          throw new Error(`client config request failed (${response.status})`);
        }
        const data = await response.json();
        if (typeof data.upload_token !== 'string' || !data.upload_token) {
          throw new Error('upload token missing');
        }
        uploadToken = data.upload_token;
        return uploadToken;
      })
      .finally(() => {
        uploadTokenPromise = null;
      });
  }
  return uploadTokenPromise;
}

function afterNextPaint(callback) {
  requestAnimationFrame(() => requestAnimationFrame(callback));
}

function animatePanelIn(panel) {
  if (!panel) return;
  panel.classList.add('panel-enter');
  afterNextPaint(() => {
    if (!panel.isConnected) return;
    panel.classList.add('panel-enter-active');
  });
}

function wrapInTimeline(panel, variant) {
  const node = document.createElement('div');
  node.className = 'timeline-node' + (variant ? ` timeline-node--${variant}` : '');
  node.appendChild(panel);
  return node;
}

function removeTimelinePanel(panel) {
  if (!panel) return;
  const wrapper = panel.closest('.timeline-node');
  if (wrapper) wrapper.remove(); else panel.remove();
}

function cancelScheduledMarkdownRender(el) {
  if (!el) return;
  if (el._markdownIdleHandle) {
    if (typeof cancelIdleCallback === 'function') {
      cancelIdleCallback(el._markdownIdleHandle);
    } else {
      clearTimeout(el._markdownIdleHandle);
    }
    el._markdownIdleHandle = 0;
  }
}

function scheduleBackgroundTask(callback, timeout = 180) {
  if (typeof requestIdleCallback === 'function') {
    return requestIdleCallback(callback, { timeout });
  }
  return setTimeout(callback, 16);
}

function formatToolDuration(durationMs) {
  if (durationMs == null) return '';
  if (durationMs < 1000) {
    return `${Math.max(1, Math.round(durationMs))}ms`;
  }
  return `${(durationMs / 1000).toFixed(durationMs < 10000 ? 1 : 0)}s`;
}

function cancelBackgroundTask(handle) {
  if (!handle) return;
  if (typeof cancelIdleCallback === 'function') {
    cancelIdleCallback(handle);
  } else {
    clearTimeout(handle);
  }
}

function shouldHighlightBlock(block, index, totalBlocks) {
  const code = block.textContent || '';
  if (code.length > 4000) return false;
  if (totalBlocks > 6 && index >= 4) return false;
  return true;
}

function scheduleCodeHighlight(blocks) {
  const codeBlocks = [...blocks];
  const highlightQueue = codeBlocks.filter((block, index) => {
    if (!block.isConnected || !shouldHighlightBlock(block, index, codeBlocks.length)) {
      block.classList.add('code-highlight-deferred');
      return false;
    }
    return true;
  });

  const highlightChunk = () => {
    let processed = 0;
    while (highlightQueue.length && processed < 2) {
      const block = highlightQueue.shift();
      if (block?.isConnected) {
        hljs.highlightElement(block);
      }
      processed += 1;
    }
    if (highlightQueue.length) {
      scheduleBackgroundTask(highlightChunk, 120);
    }
  };

  if (highlightQueue.length) {
    scheduleBackgroundTask(highlightChunk, 120);
  }
}

function scheduleMarkdownRender(el, options = {}) {
  if (!el) return;
  const { followScroll } = options;
  cancelScheduledMarkdownRender(el);
  const queuedIndex = markdownRenderQueue.indexOf(el);
  if (queuedIndex !== -1) {
    markdownRenderQueue.splice(queuedIndex, 1);
  }
  el.classList.add('markdown-pending');
  el._markdownShouldFollow = typeof followScroll === 'boolean'
    ? followScroll
    : chat.scrollHeight - chat.scrollTop - chat.clientHeight < 80;
  markdownRenderQueue.push(el);
  if (!markdownQueueHandle) {
    markdownQueueHandle = scheduleBackgroundTask(processMarkdownQueue);
  }
}

function processMarkdownQueue() {
  markdownQueueHandle = 0;
  const el = markdownRenderQueue.shift();
  if (!el) return;
  el._markdownIdleHandle = 0;
  if (el.isConnected) {
    renderMarkdown(el);
    el.classList.remove('markdown-pending');
    if (el._markdownShouldFollow) scrollDown();
  }
  el._markdownShouldFollow = false;
  if (markdownRenderQueue.length) {
    markdownQueueHandle = scheduleBackgroundTask(processMarkdownQueue);
  }
}

function animateCollapsibleSection(body, expand) {
  if (!body) return;

  const startHeight = body.getBoundingClientRect().height;
  body.classList.toggle('show', expand);
  const targetHeight = expand ? body.scrollHeight : 0;

  body.style.height = `${startHeight}px`;
  body.getBoundingClientRect();
  body.classList.toggle('is-open', expand);
  body.style.height = `${targetHeight}px`;

  const finalize = (e) => {
    if (e.propertyName !== 'height') return;
    body.style.height = expand ? 'auto' : '0px';
    body.removeEventListener('transitionend', finalize);
  };

  body.addEventListener('transitionend', finalize);
}

function syncToolDrawerBounds() {
  if (!inputArea) return;
  const viewport = window.visualViewport;
  const rect = inputArea.getBoundingClientRect();
  const viewportBottom = viewport
    ? viewport.offsetTop + viewport.height
    : window.innerHeight;
  const bottomInset = Math.max(16, Math.ceil(viewportBottom - rect.top + 8));
  document.documentElement.style.setProperty('--tool-drawer-bottom', `${bottomInset}px`);
  document.documentElement.style.setProperty('--jump-to-latest-bottom', `${bottomInset + 10}px`);
}

function distanceFromBottom() {
  return chat.scrollHeight - chat.scrollTop - chat.clientHeight;
}

function isChatNearBottom(threshold = AUTO_SCROLL_THRESHOLD) {
  return distanceFromBottom() <= threshold;
}

function updateJumpToLatestVisibility() {
  if (!jumpToLatestBtn) return;
  const show = !autoFollowChat && hasBufferedChatUpdates;
  const hasCount = unreadMessageCount > 0;
  jumpToLatestBtn.hidden = !show;
  jumpToLatestBtn.classList.toggle('visible', show);
  jumpToLatestBtn.classList.toggle('has-state-only', show && !hasCount);
  if (jumpToLatestBadge) {
    if (!show) {
      jumpToLatestBadge.hidden = true;
      jumpToLatestBadge.textContent = '';
    } else if (hasCount) {
      jumpToLatestBadge.hidden = false;
      jumpToLatestBadge.textContent = unreadMessageCount > 99 ? '99+' : String(unreadMessageCount);
    } else {
      jumpToLatestBadge.hidden = false;
      jumpToLatestBadge.textContent = '新';
    }
  }
  jumpToLatestBtn.setAttribute(
    'aria-label',
    hasCount ? `Jump to latest messages, ${unreadMessageCount} unread items` : 'Jump to latest messages, new content available'
  );
  jumpToLatestBtn.title = hasCount ? `${unreadMessageCount} 条新内容` : '有新内容';
}

function clearBufferedChatUpdates() {
  hasBufferedChatUpdates = false;
  unreadMessageCount = 0;
  updateJumpToLatestVisibility();
}

function setAutoFollowChat(nextFollow) {
  autoFollowChat = nextFollow;
  if (nextFollow) {
    clearBufferedChatUpdates();
  } else {
    updateJumpToLatestVisibility();
  }
}

function markChatUpdateOffscreen() {
  if (bulkRenderingChat) return;
  hasBufferedChatUpdates = true;
  updateJumpToLatestVisibility();
}

function queueUnreadContent({ countable = false } = {}) {
  if (bulkRenderingChat || autoFollowChat || isChatNearBottom()) {
    return;
  }
  hasBufferedChatUpdates = true;
  if (countable) {
    unreadMessageCount += 1;
  }
  updateJumpToLatestVisibility();
}

function syncChatScrollState() {
  if (suppressScrollTracking) return;
  setAutoFollowChat(isChatNearBottom());
}

function jumpToLatest() {
  setAutoFollowChat(true);
  scrollDown(true);
}

function updateViewToggleButtons() {
  if (toggleToolsBtn) {
    toggleToolsBtn.textContent = `Tools: ${showTools ? 'On' : 'Off'}`;
    toggleToolsBtn.classList.toggle('is-active', showTools);
  }
  if (toggleReasoningBtn) {
    toggleReasoningBtn.textContent = `Reasoning: ${showReasoning ? 'On' : 'Off'}`;
    toggleReasoningBtn.classList.toggle('is-active', showReasoning);
  }
}

function applyViewState(viewState) {
  if (!viewState) return;

  if (typeof viewState.show_tools === 'boolean') {
    showTools = viewState.show_tools;
    if (!showTools) {
      closeToolDrawer();
      activeToolPanel = null;
      for (const panel of chat.querySelectorAll('.tool-panel')) {
        removeTimelinePanel(panel);
      }
    }
  }

  if (typeof viewState.show_reasoning === 'boolean') {
    showReasoning = viewState.show_reasoning;
    if (!showReasoning) {
      finishReasoningStream();
      if (reasoningPanel) removeTimelinePanel(reasoningPanel);
      reasoningPanel = null;
    }
  }

  updateViewToggleButtons();
}

// ── Image Attachment ──

function updateAttachButton() {
  if (attachBtn) attachBtn.style.display = imageCapable ? '' : 'none';
  // Clear pending images when capability is lost
  if (!imageCapable && pendingImages.length > 0) {
    pendingImages = [];
    renderImagePreviews();
  }
}

function isUploadedPendingImage(image) {
  return !!(image && (image.object_key || image.attachment_token));
}

function dropUnavailablePendingUploads(notify = false) {
  if (pendingImages.length === 0) return;
  const keptImages = pendingImages.filter((image) => !isUploadedPendingImage(image));
  if (keptImages.length === pendingImages.length) return;
  pendingImages = keptImages;
  renderImagePreviews();
  closeAttachPopup();
  if (imageFileInput) imageFileInput.value = '';
  if (notify) {
    addSystem('Local uploaded images were cleared because S3 uploads are unavailable. Please re-attach them or use an image URL.');
  }
}

function closeAttachPopup() {
  if (attachPopup) attachPopup.style.display = 'none';
  if (attachUrlInput) attachUrlInput.style.display = 'none';
  if (attachUploadStatus) attachUploadStatus.style.display = 'none';
  if (attachMenu) attachMenu.style.display = 'flex';
}

function openAttachPopup() {
  if (!attachPopup) return;
  // If no S3, go directly to URL input mode
  if (!s3Capable) {
    attachMenu.style.display = 'none';
    attachUrlInput.style.display = 'flex';
    attachUploadStatus.style.display = 'none';
  } else {
    attachMenu.style.display = 'flex';
    attachUrlInput.style.display = 'none';
    attachUploadStatus.style.display = 'none';
  }
  attachPopup.style.display = 'block';
  if (!s3Capable && imageUrlField) {
    setTimeout(() => imageUrlField.focus(), 50);
  }
}

function toggleAttachPopup() {
  if (!attachPopup) return;
  if (attachPopup.style.display === 'none' || !attachPopup.style.display) {
    openAttachPopup();
  } else {
    closeAttachPopup();
  }
}

function addImageUrl(url) {
  if (!url || !url.trim()) return;
  const trimmed = url.trim();
  let parsed;
  try {
    parsed = new URL(trimmed);
  } catch {
    addSystem('Invalid URL format.');
    return;
  }
  if (!trimmed.startsWith('http://') && !trimmed.startsWith('https://')) {
    addSystem('Only http:// and https:// URLs are allowed.');
    return;
  }
  const path = parsed.pathname;
  const rawLastSegment = path.split('/').filter(Boolean).pop() || '';
  let lastSegment = rawLastSegment;
  try {
    lastSegment = decodeURIComponent(rawLastSegment);
  } catch {
    lastSegment = rawLastSegment;
  }
  lastSegment = lastSegment.toLowerCase().replace(/\.+$/, '');
  const dotIndex = lastSegment.lastIndexOf('.');
  const hasExplicitExtension = dotIndex >= 0 && dotIndex < lastSegment.length - 1;
  if (hasExplicitExtension) {
    if (/\.(png|jpe?g)$/.test(lastSegment)) {
      // Accepted explicit image suffix.
    } else if (/\.(gif|webp|svg|bmp|ico|tif|tiff|avif)$/.test(lastSegment)) {
      addSystem('Only PNG and JPEG image URLs are supported.');
      return;
    } else {
      addSystem('URL does not appear to be an image.');
      return;
    }
  }
  pendingImages.push({ url: trimmed });
  renderImagePreviews();
  closeAttachPopup();
}

async function uploadLocalImages(files) {
  if (!files || files.length === 0) return;
  if (attachUploadStatus) {
    attachMenu.style.display = 'none';
    attachUrlInput.style.display = 'none';
    attachUploadStatus.style.display = 'flex';
  }
  let token;
  try {
    token = await ensureUploadToken();
  } catch (e) {
    addSystem('Upload failed: ' + e.message);
    closeAttachPopup();
    if (imageFileInput) imageFileInput.value = '';
    return;
  }
  const formData = new FormData();
  for (const file of files) {
    formData.append('file', file);
  }
  try {
    let resp = await fetch('/api/upload-images', {
      method: 'POST',
      headers: { 'X-LingClaw-Upload-Token': token },
      body: formData
    });
    if (resp.status === 403) {
      token = await ensureUploadTokenInternal(true);
      resp = await fetch('/api/upload-images', {
        method: 'POST',
        headers: { 'X-LingClaw-Upload-Token': token },
        body: formData
      });
    }
    if (!resp.ok) {
      if (resp.status === 403) {
        uploadToken = '';
      }
      const errText = await resp.text().catch(() => resp.statusText);
      addSystem('Upload failed: ' + errText);
      closeAttachPopup();
      if (imageFileInput) imageFileInput.value = '';
      return;
    }
    const data = await resp.json();
    if (data.images && data.images.length > 0) {
      for (const image of data.images) {
        pendingImages.push({
          url: image.url,
          object_key: image.object_key,
          attachment_token: image.attachment_token
        });
      }
      renderImagePreviews();
    } else if (data.urls && data.urls.length > 0) {
      for (const url of data.urls) {
        pendingImages.push({ url });
      }
      renderImagePreviews();
    }
    if (data.errors && data.errors.length > 0) {
      for (const err of data.errors) {
        addSystem('Upload error: ' + err);
      }
    }
    if (!data.urls || data.urls.length === 0) {
      if (!data.errors || data.errors.length === 0) {
        addSystem('No images uploaded.');
      }
    }
  } catch (e) {
    addSystem('Upload failed: ' + e.message);
  }
  closeAttachPopup();
  // Reset file input so same files can be re-selected
  if (imageFileInput) imageFileInput.value = '';
}

function removeImage(index) {
  pendingImages.splice(index, 1);
  renderImagePreviews();
}

function renderImagePreviews() {
  if (!imagePreviewBar) return;
  imagePreviewBar.innerHTML = '';
  if (pendingImages.length === 0) {
    imagePreviewBar.style.display = 'none';
    return;
  }
  imagePreviewBar.style.display = 'flex';
  pendingImages.forEach((img, idx) => {
    const item = document.createElement('div');
    item.className = 'image-preview-item';
    const imgEl = document.createElement('img');
    imgEl.src = img.url;
    imgEl.alt = 'Attached image';
    imgEl.onerror = () => { imgEl.style.display = 'none'; };
    const removeBtn = document.createElement('button');
    removeBtn.className = 'remove-btn';
    removeBtn.textContent = '×';
    removeBtn.onclick = (e) => { e.stopPropagation(); removeImage(idx); };
    item.appendChild(imgEl);
    item.appendChild(removeBtn);
    imagePreviewBar.appendChild(item);
  });
}

if (attachBtn) attachBtn.addEventListener('click', (e) => { e.stopPropagation(); toggleAttachPopup(); });
if (attachUrlBtn) attachUrlBtn.addEventListener('click', () => {
  if (attachMenu) attachMenu.style.display = 'none';
  if (attachUrlInput) attachUrlInput.style.display = 'flex';
  if (imageUrlField) setTimeout(() => imageUrlField.focus(), 50);
});
if (attachLocalBtn) attachLocalBtn.addEventListener('click', () => {
  if (imageFileInput) imageFileInput.click();
});
if (imageUrlAddBtn) imageUrlAddBtn.addEventListener('click', () => {
  if (imageUrlField) { addImageUrl(imageUrlField.value); imageUrlField.value = ''; }
});
if (imageUrlField) imageUrlField.addEventListener('keydown', (e) => {
  if (e.key === 'Enter') { e.preventDefault(); addImageUrl(imageUrlField.value); imageUrlField.value = ''; }
  if (e.key === 'Escape') closeAttachPopup();
});
if (imageFileInput) imageFileInput.addEventListener('change', () => {
  if (imageFileInput.files && imageFileInput.files.length > 0) {
    uploadLocalImages(imageFileInput.files);
  }
});
// Close popup when clicking outside
document.addEventListener('click', (e) => {
  if (attachPopup && attachPopup.style.display !== 'none') {
    const wrapper = attachBtn ? attachBtn.closest('.attach-wrapper') : null;
    if (wrapper && !wrapper.contains(e.target)) {
      closeAttachPopup();
    }
  }
});

function toggleToolsVisibility() {
  if (!ws || ws.readyState !== 1) return;
  const nextShowTools = !showTools;
  applyViewState({ show_tools: nextShowTools });
  sendCmd(`/tool ${nextShowTools ? 'on' : 'off'}`);
}

function toggleReasoningVisibility() {
  if (!ws || ws.readyState !== 1) return;
  const nextShowReasoning = !showReasoning;
  applyViewState({ show_reasoning: nextShowReasoning });
  sendCmd(`/reasoning ${nextShowReasoning ? 'on' : 'off'}`);
}

function canSendWhileBusy(cmd) {
  return /^\/(tool|reasoning|stop)\b/i.test(cmd);
}

// ── Progressive segmented markdown ──

function isSentenceSplitChar(text, index) {
  const ch = text[index];
  if ('。！？；：'.includes(ch)) return true;
  if ('!?;:'.includes(ch)) {
    const next = text[index + 1] || '';
    return !next || /\s/.test(next);
  }
  if (ch === '.') {
    const prev = text[index - 1] || '';
    const next = text[index + 1] || '';
    return /[A-Za-z0-9\)]/.test(prev) && (!next || /\s/.test(next));
  }
  return false;
}

function findProgressiveSplitPoint(text) {
  let inFence = false;
  let lastSplit = -1;
  let lastSoftSplit = -1;
  let charsSinceBoundary = 0;
  let i = 0;
  while (i < text.length) {
    const atLineStart = (i === 0 || text[i - 1] === '\n');
    if (atLineStart && i + 2 < text.length &&
        text[i] === '`' && text[i + 1] === '`' && text[i + 2] === '`') {
      const wasFenced = inFence;
      inFence = !inFence;
      let j = i + 3;
      while (j < text.length && text[j] !== '\n') j++;
      i = j < text.length ? j + 1 : text.length;
      if (wasFenced && !inFence && i < text.length) {
        lastSplit = i;
        lastSoftSplit = -1;
        charsSinceBoundary = 0;
      }
      continue;
    }
    if (!inFence && text[i] === '\n' && i + 1 < text.length && text[i + 1] === '\n') {
      let j = i + 2;
      while (j < text.length && text[j] === '\n') j++;
      lastSplit = j;
      lastSoftSplit = -1;
      charsSinceBoundary = 0;
      i = j;
      continue;
    }
    if (!inFence) {
      charsSinceBoundary += 1;
      if (isSentenceSplitChar(text, i) && charsSinceBoundary >= SOFT_SPLIT_MIN_CHARS) {
        lastSoftSplit = i + 1;
      } else if (/\s/.test(text[i]) && charsSinceBoundary >= SOFT_SPLIT_MAX_CHARS) {
        lastSoftSplit = i + 1;
      }
    }
    i++;
  }
  if (!inFence && lastSoftSplit > lastSplit) {
    const tailLength = text.length - lastSoftSplit;
    if (tailLength === 0 || tailLength >= SOFT_SPLIT_TAIL_MIN_CHARS) {
      return lastSoftSplit;
    }
  }
  return lastSplit;
}

// ── Math (KaTeX) integration ──

function isAsciiDigit(ch) {
  return ch >= '0' && ch <= '9';
}

function extractMath(text) {
  const blocks = [];
  let out = '';
  let i = 0;
  const len = text.length;

  while (i < len) {
    // Escaped dollar sign
    if (text[i] === '\\' && i + 1 < len && text[i + 1] === '$') {
      out += '\\$';
      i += 2;
      continue;
    }

    // Fenced code block
    const atLineStart = i === 0 || text[i - 1] === '\n';
    if (atLineStart && i + 2 < len && text[i] === '`' && text[i + 1] === '`' && text[i + 2] === '`') {
      const endMarker = text.indexOf('\n```', i + 3);
      if (endMarker === -1) {
        out += text.substring(i);
        i = len;
        continue;
      }
      let end = endMarker + 4;
      while (end < len && text[end] !== '\n') end++;
      out += text.substring(i, end);
      i = end;
      continue;
    }

    // Inline code
    if (text[i] === '`') {
      let j = i + 1;
      while (j < len && text[j] !== '`' && text[j] !== '\n') j++;
      if (j < len && text[j] === '`') {
        out += text.substring(i, j + 1);
        i = j + 1;
        continue;
      }
    }

    // Display math $$...$$
    if (text[i] === '$' && i + 1 < len && text[i + 1] === '$') {
      const searchFrom = i + 2;
      const end = text.indexOf('$$', searchFrom);
      if (end !== -1 && end > searchFrom) {
        const formula = text.substring(searchFrom, end);
        const id = blocks.length;
        blocks.push({ formula: formula.trim(), displayMode: true });
        out += `<span data-math="${id}"></span>`;
        i = end + 2;
        continue;
      }
    }

    // Inline math $...$ with a conservative digit heuristic to avoid
    // mis-rendering currency like "$5,$10" as a formula.
    const prev = i > 0 ? text[i - 1] : '';
    if (
      text[i] === '$' &&
      i + 1 < len &&
      text[i + 1] !== '$' &&
      text[i + 1] !== ' ' &&
      text[i + 1] !== '\n' &&
      !isAsciiDigit(prev)
    ) {
      let j = i + 1;
      while (j < len && text[j] !== '$' && text[j] !== '\n') j++;
      const next = j + 1 < len ? text[j + 1] : '';
      if (
        j < len &&
        text[j] === '$' &&
        j > i + 1 &&
        text[j - 1] !== ' ' &&
        !isAsciiDigit(next)
      ) {
        const formula = text.substring(i + 1, j);
        const id = blocks.length;
        blocks.push({ formula, displayMode: false });
        out += `<span data-math="${id}"></span>`;
        i = j + 1;
        continue;
      }
    }

    out += text[i];
    i++;
  }

  return { text: out, blocks };
}

function renderMathPlaceholders(html, mathBlocks) {
  if (!mathBlocks.length) return html;
  return html.replace(/<span data-math="(\d+)"><\/span>/g, (_, idStr) => {
    const id = parseInt(idStr, 10);
    if (id < 0 || id >= mathBlocks.length) return '';
    const { formula, displayMode } = mathBlocks[id];
    if (typeof katex === 'undefined') {
      return displayMode
        ? `<pre><code>${escHtml(formula)}</code></pre>`
        : `<code>${escHtml(formula)}</code>`;
    }
    try {
      return katex.renderToString(formula, {
        displayMode,
        throwOnError: false,
        output: 'htmlAndMathml',
      });
    } catch {
      return `<code>${escHtml(formula)}</code>`;
    }
  });
}

function decorateCodeBlocks(container) {
  container.querySelectorAll('pre').forEach(pre => {
    pre.style.position = 'relative';
    const codeEl = pre.querySelector('code');
    if (codeEl) {
      const cls = [...codeEl.classList].find(c => c.startsWith('language-'));
      if (cls) {
        const label = document.createElement('span');
        label.className = 'code-lang-label';
        label.textContent = cls.replace('language-', '');
        pre.appendChild(label);
      }
    }
    const btn = document.createElement('button');
    btn.className = 'copy-btn';
    btn.textContent = '复制';
    btn.onclick = () => {
      const code = pre.querySelector('code');
      const text = code?.textContent || pre.textContent;
      if (navigator.clipboard) {
        navigator.clipboard.writeText(text).catch(() => fallbackCopy(text));
      } else {
        fallbackCopy(text);
      }
      btn.textContent = '已复制';
      setTimeout(() => btn.textContent = '复制', 1500);
    };
    pre.appendChild(btn);
  });
}

function appendRenderedSegment(el, markdownText) {
  const { text: preprocessed, blocks: mathBlocks } = extractMath(markdownText);
  const html = marked.parse(preprocessed);
  const sanitized = typeof DOMPurify !== 'undefined' ? DOMPurify.sanitize(html) : escHtml(html);
  const temp = document.createElement('div');
  temp.innerHTML = renderMathPlaceholders(sanitized, mathBlocks);
  decorateCodeBlocks(temp);
  const codeBlocks = [...temp.querySelectorAll('pre code')];
  const tail = el._liveTail;
  while (temp.firstChild) {
    if (tail && tail.parentNode === el) {
      el.insertBefore(temp.firstChild, tail);
    } else {
      el.appendChild(temp.firstChild);
    }
  }
  scheduleCodeHighlight(codeBlocks);
}

function parseOpenCodeFence(text) {
  const normalized = text.replace(/^\n+/, '');
  if (!normalized.startsWith('```')) return null;

  const firstNewline = normalized.indexOf('\n');
  if (firstNewline === -1) return null;

  const rest = normalized.slice(firstNewline + 1);
  if (/(^|\n)```/.test(rest)) return null;

  const info = normalized.slice(3, firstNewline).trim();
  const language = info ? info.split(/\s+/, 1)[0] : '';
  return {
    language,
    code: rest
  };
}

function ensureLiveTail(el, mode) {
  if (el._liveTail && el._liveTail.dataset.mode === mode) {
    return el._liveTail;
  }

  removeLiveTail(el);

  if (mode === 'code') {
    const pre = document.createElement('pre');
    pre.className = 'live-tail live-code-tail';
    pre.dataset.mode = 'code';
    const code = document.createElement('code');
    pre.appendChild(code);
    el.appendChild(pre);
    el._liveTail = pre;
    return pre;
  }

  const span = document.createElement('span');
  span.className = 'live-tail';
  span.dataset.mode = 'text';
  el.appendChild(span);
  el._liveTail = span;
  return span;
}

function updateLiveTail(el, text) {
  if (!text) {
    removeLiveTail(el);
    return;
  }

  const codeTail = parseOpenCodeFence(text);
  if (codeTail) {
    const tail = ensureLiveTail(el, 'code');
    let label = tail.querySelector('.code-lang-label');
    if (codeTail.language) {
      if (!label) {
        label = document.createElement('span');
        label.className = 'code-lang-label';
        tail.insertBefore(label, tail.firstChild);
      }
      label.textContent = codeTail.language;
    } else if (label) {
      label.remove();
    }
    const codeEl = tail.querySelector('code');
    if (codeEl) {
      codeEl.textContent = codeTail.code;
    }
    return;
  }

  const tail = ensureLiveTail(el, 'text');
  tail.textContent = text;
}

function removeLiveTail(el) {
  if (el._liveTail) {
    if (el._liveTail.parentNode) el._liveTail.parentNode.removeChild(el._liveTail);
    el._liveTail = null;
  }
}

function flushAssistantText() {
  if (!currentMsg || !pendingAssistantText) return;
  currentMsg._rawText = (currentMsg._rawText || '') + pendingAssistantText;
  pendingAssistantText = '';

  const raw = currentMsg._rawText;
  const offset = currentMsg._renderedOffset || 0;
  const splitAt = findProgressiveSplitPoint(raw);

  if (splitAt > offset) {
    appendRenderedSegment(currentMsg, raw.substring(offset, splitAt));
    currentMsg._renderedOffset = splitAt;
    updateLiveTail(currentMsg, raw.substring(splitAt));
  } else if (offset > 0) {
    updateLiveTail(currentMsg, raw.substring(offset));
  } else {
    updateLiveTail(currentMsg, raw);
  }
  revealCurrentAssistant();
}

function flushReasoningText() {
  if (!reasoningPanel || !pendingReasoningText) return;
  const body = reasoningPanel.querySelector('.reasoning-body');
  if (!body) { pendingReasoningText = ''; return; }
  if (!body._textNode) {
    body._textNode = document.createTextNode(pendingReasoningText);
    body.appendChild(body._textNode);
  } else {
    body._textNode.nodeValue += pendingReasoningText;
  }
  pendingReasoningText = '';
}

function flushStreaming() {
  flushHandle = 0;
  const follow = autoFollowChat || isChatNearBottom();
  flushAssistantText();
  flushReasoningText();
  if (follow) scrollDown();
}

function scheduleFlush() {
  if (!flushHandle) {
    flushHandle = requestAnimationFrame(flushStreaming);
  }
}

function cancelAssistantFlush() {
  pendingAssistantText = '';
  cancelFlushIfIdle();
}

function cancelReasoningFlush() {
  pendingReasoningText = '';
  cancelFlushIfIdle();
}

function cancelFlushIfIdle() {
  if (!pendingAssistantText && !pendingReasoningText && flushHandle) {
    cancelAnimationFrame(flushHandle);
    flushHandle = 0;
  }
}

function currentMsgRow() {
  return currentMsg ? currentMsg.closest('.msg-row') : null;
}

function beginAssistantStream() {
  cancelAssistantFlush();
  const message = addAssistant('', { trackUnread: false });
  const row = message.closest('.msg-row');
  if (row) {
    row.hidden = true;
  }
  message.classList.add('typing');
  message._rawText = '';
  message._renderedOffset = 0;
  currentMsg = message;
}

function revealCurrentAssistant() {
  const row = currentMsgRow();
  if (row) {
    row.hidden = false;
  }
}

function finishAssistantStream({ discardIfEmpty = false } = {}) {
  flushAssistantText();
  if (!currentMsg) {
    return;
  }

  const row = currentMsgRow();
  const rawText = currentMsg._rawText || '';
  const raw = rawText.trim();
  currentMsg.classList.remove('typing');

  if (!raw && discardIfEmpty) {
    row?.remove();
    currentMsg = null;
    return;
  }

  if (!raw) {
    row?.removeAttribute('hidden');
    currentMsg = null;
    return;
  }

  revealCurrentAssistant();

  const offset = currentMsg._renderedOffset || 0;
  if (offset > 0) {
    removeLiveTail(currentMsg);
    const tail = rawText.substring(offset);
    if (tail) {
      appendRenderedSegment(currentMsg, tail);
    }
  } else {
    scheduleMarkdownRender(currentMsg);
  }
  currentMsg = null;
}

function finishReasoningStream() {
  flushReasoningText();
  cancelReasoningFlush();
  if (reasoningPanel) {
    const body = reasoningPanel.querySelector('.reasoning-body');
    if (body && body.classList.contains('show')) {
      body.style.height = 'auto';
    }
  }
  scrollDown();
}

function reactPhaseLabel(phase) {
  return {
    analyze: 'Analyze',
    act: 'Act',
    observe: 'Observe'
  }[phase] || phase || 'Analyze';
}

function pinReactStatusToBottom() {
  if (!reactStatusRow?.isConnected) return;
  if (chat.lastElementChild === reactStatusRow) return;
  chat.appendChild(reactStatusRow);
}

function renderReactStatus() {
  if (!reactStatusRow) return;
  const card = reactStatusRow.querySelector('.react-status-card');
  const phase = reactStatusRow.querySelector('.react-status-phase');
  const cycle = reactStatusRow.querySelector('.react-status-cycle');
  const detail = reactStatusRow.querySelector('.react-status-detail');
  const detailTool = reactStatusRow.querySelector('.react-status-tool');
  const detailTime = reactStatusRow.querySelector('.react-status-time');
  if (!card || !phase || !cycle || !detail || !detailTool || !detailTime) return;
  card.dataset.phase = reactStatusPhase || 'analyze';
  phase.textContent = reactPhaseLabel(reactStatusPhase);
  cycle.textContent = Number.isInteger(reactStatusCycle) ? `cycle ${reactStatusCycle}` : '';
  if (reactStatusPhase === 'act' && reactStatusToolName) {
    const seconds = Math.max(1, Math.floor((reactStatusElapsedMs || 0) / 1000));
    detailTool.textContent = reactStatusToolName;
    detailTime.textContent = `${seconds}s`;
    detail.hidden = false;
  } else {
    detailTool.textContent = '';
    detailTime.textContent = '';
    detail.hidden = true;
  }
}

function clearReactStatus() {
  if (reactPhaseTimer) {
    clearTimeout(reactPhaseTimer);
    reactPhaseTimer = 0;
  }
  reactPhaseQueue = [];
  reactPendingClear = false;
  reactStatusPhase = '';
  reactStatusCycle = null;
  reactStatusToolName = '';
  reactStatusElapsedMs = 0;
  reactPhaseShownAt = 0;
  if (reactStatusRow) {
    reactStatusRow.remove();
    reactStatusRow = null;
  }
}

function reactPhaseMinVisibleMs(phase) {
  switch (phase) {
    case 'act':
      return MIN_REACT_ACT_VISIBLE_MS;
    case 'observe':
      return MIN_REACT_OBSERVE_VISIBLE_MS;
    case 'analyze':
    default:
      return MIN_REACT_ANALYZE_VISIBLE_MS;
  }
}

function requestClearReactStatus() {
  if (!reactStatusPhase && reactPhaseQueue.length === 0) {
    clearReactStatus();
    return;
  }
  reactPendingClear = true;
  scheduleNextReactPhase();
}

function ensureReactStatusRow() {
  if (!reactStatusRow) {
    reactStatusRow = document.createElement('div');
    reactStatusRow.className = 'msg-row system react-status-row';
    reactStatusRow.innerHTML = `
      <div class="system-card system-inline react-status-card">
        <span class="react-status-tag">ReAct</span>
        <span class="react-status-phase"></span>
        <span class="react-status-cycle"></span>
        <span class="react-status-detail" hidden>
          <span class="react-status-tool"></span>
          <span class="react-status-separator">·</span>
          <span class="react-status-time"></span>
        </span>
        <span class="react-status-dots" aria-hidden="true">
          <span></span>
          <span></span>
          <span></span>
        </span>
      </div>
    `;
    chat.appendChild(reactStatusRow);
    hideWelcome();
  }
}

function scheduleNextReactPhase() {
  if (reactPhaseTimer || !reactStatusPhase) {
    return;
  }

  const elapsed = performance.now() - reactPhaseShownAt;
  const delay = Math.max(0, reactPhaseMinVisibleMs(reactStatusPhase) - elapsed);
  reactPhaseTimer = setTimeout(() => {
    reactPhaseTimer = 0;
    const next = reactPhaseQueue.shift();
    if (next) {
      applyReactStatusNow(next.phase, next.cycle);
      return;
    }
    if (reactPendingClear) {
      clearReactStatus();
    }
  }, delay);
}

function applyReactStatusNow(phase, cycle = null) {
  ensureReactStatusRow();
  reactStatusPhase = phase;
  reactStatusCycle = Number.isInteger(cycle) ? cycle : null;
  if (phase !== 'act') {
    reactStatusToolName = '';
    reactStatusElapsedMs = 0;
  }
  reactPhaseShownAt = performance.now();
  renderReactStatus();
  scrollDown();
  scheduleNextReactPhase();
}

function setReactActTool(name, elapsedMs = 0) {
  if (!name) return;
  reactStatusToolName = name;
  reactStatusElapsedMs = elapsedMs;
  if (reactStatusPhase === 'act') {
    renderReactStatus();
  }
}

function showReactStatus(phase, cycle = null) {
  if (!phase) {
    requestClearReactStatus();
    return;
  }

  if (phase === 'finish') {
    requestClearReactStatus();
    return;
  }

  reactPendingClear = false;

  if (!reactStatusPhase && reactPhaseQueue.length === 0 && !reactPhaseTimer) {
    applyReactStatusNow(phase, cycle);
    return;
  }

  if (reactStatusPhase === phase && reactPhaseQueue.length === 0) {
    reactStatusCycle = Number.isInteger(cycle) ? cycle : null;
    renderReactStatus();
    return;
  }

  for (let index = reactPhaseQueue.length - 1; index >= 0; index -= 1) {
    if (reactPhaseQueue[index].phase === phase) {
      reactPhaseQueue[index].cycle = Number.isInteger(cycle) ? cycle : null;
      reactPhaseQueue.splice(index + 1);
      scheduleNextReactPhase();
      return;
    }
  }

  reactPhaseQueue.push({
    phase,
    cycle: Number.isInteger(cycle) ? cycle : null,
  });
  if (reactPhaseQueue.length > MAX_REACT_QUEUED_PHASES) {
    reactPhaseQueue.splice(0, reactPhaseQueue.length - MAX_REACT_QUEUED_PHASES);
  }
  scheduleNextReactPhase();
}

// ── Markdown setup ──
marked.setOptions({
  highlight: (code, lang) => {
    if (lang && hljs.getLanguage(lang)) {
      return hljs.highlight(code, { language: lang }).value;
    }
    return hljs.highlightAuto(code).value;
  },
  breaks: true,
});

// ── WebSocket ──
function connect() {
  const proto = location.protocol === 'https:' ? 'wss' : 'ws';
  ws = new WebSocket(`${proto}://${location.host}/ws`);

  ws.onopen = () => {
    reconnectDelay = 1000;
    reconnectAttempts = 0;
    connDot.className = 'conn-dot connected';
    connLabel.textContent = 'Online';
    addSystem('Connected.');
  };

  ws.onclose = () => {
    connDot.className = 'conn-dot disconnected';
    connLabel.textContent = 'Offline';
    finishAssistantStream({ discardIfEmpty: true });
    finishReasoningStream();
    closeToolDrawer();
    clearReactStatus();
    reasoningPanel = null;
    setBusy(false);
    if (reconnectAttempts < MAX_RECONNECT_ATTEMPTS) {
      addSystem('Disconnected. Reconnecting...');
      setTimeout(connect, reconnectDelay);
      reconnectDelay = Math.min(reconnectDelay * 2, 30000);
      reconnectAttempts++;
    } else {
      addSystem('Connection lost. Please refresh the page.', 'error');
    }
  };

  ws.onerror = () => ws.close();

  ws.onmessage = (e) => {
    let data;
    try { data = JSON.parse(e.data); } catch { console.warn('Invalid JSON from server:', e.data); return; }
    handleMessage(data);
  };
}

function handleMessage(data) {
  switch (data.type) {
    case 'session':
      currentSessionId = data.id;
      sessionNameEl.textContent = data.name || 'Main';
      sessionIdEl.textContent = data.id.slice(0, 12);
      if (data.capabilities && typeof data.capabilities.image === 'boolean') {
        imageCapable = data.capabilities.image;
        updateAttachButton();
      }
      if (data.capabilities && typeof data.capabilities.s3 === 'boolean') {
        const previousS3Capable = s3Capable;
        s3Capable = data.capabilities.s3;
        if (s3Capable) {
          void ensureUploadTokenInternal(true).catch(() => {});
        } else {
          uploadToken = '';
          uploadTokenPromise = null;
          dropUnavailablePendingUploads(previousS3Capable);
        }
      }
      applyViewState(data);
      break;

    case 'history': {
      if (!showTools) {
        data.messages = (data.messages || []).filter(m => m.role !== 'tool_call' && m.role !== 'tool_result');
      }
      closeToolDrawer();
      clearReactStatus();
      clearBufferedChatUpdates();
      setAutoFollowChat(true);
      chat.innerHTML = '';
      _deferredHistory = [];
      const msgs = data.messages || [];
      if (msgs.length === 0) {
        showWelcome();
      } else {
        chat.classList.add('no-animate');
        bulkRenderingChat = true;
        let startIdx = 0;
        if (msgs.length > HISTORY_RENDER_LIMIT) {
          startIdx = findHistoryRenderStart(msgs, msgs.length - HISTORY_RENDER_LIMIT);
          _deferredHistory = msgs.slice(0, startIdx);
          const loadMoreRow = document.createElement('div');
          loadMoreRow.className = 'msg-row system';
          loadMoreRow.id = 'load-more-row';
          loadMoreRow.innerHTML = `<button class="load-more-btn" onclick="loadEarlierMessages()">↑ 加载更早的消息 (${_deferredHistory.length} 条)</button>`;
          chat.appendChild(loadMoreRow);
        }
        for (let i = startIdx; i < msgs.length; i++) {
          renderHistoryMessage(msgs[i]);
        }
        requestAnimationFrame(() => {
          bulkRenderingChat = false;
          chat.classList.remove('no-animate');
          scrollDown(true);
        });
      }
      break;
    }

    case 'view_state':
      applyViewState(data);
      break;

    case 'start':
      setBusy(true);
      finishAssistantStream({ discardIfEmpty: true });
      beginAssistantStream();
      if (data.react_visible && data.phase) {
        showReactStatus(data.phase, data.cycle);
      }
      break;

    case 'delta':
      if (currentMsg) {
        pendingAssistantText += data.content;
        scheduleFlush();
      }
      break;

    case 'done':
      finishAssistantStream({ discardIfEmpty: true });
      finishReasoningStream();
      requestClearReactStatus();
      reasoningPanel = null;
      setBusy(false);
      break;

    case 'react_phase': {
      showReactStatus(data.phase, data.cycle);
      break;
    }

    case 'thinking_start': {
      if (!showReasoning) break;
      const panel = document.createElement('div');
      panel.className = 'reasoning-panel reasoning-active';
      panel.innerHTML = `
        <div class="reasoning-header" onclick="toggleTool(this)">
          <span class="reasoning-icon">💭</span>
          <span class="reasoning-label">Reasoning</span>
          <span class="reasoning-status">推理中</span>
          <span class="chevron open">▸</span>
        </div>
        <div class="reasoning-body show"></div>
      `;
      const currentRow = currentMsg ? currentMsg.closest('.msg-row') : null;
      const wrapper = wrapInTimeline(panel, 'reasoning');
      if (currentRow) {
        chat.insertBefore(wrapper, currentRow);
      } else {
        chat.appendChild(wrapper);
      }
      pinReactStatusToBottom();
      animatePanelIn(panel);
      reasoningPanel = panel;
      hideWelcome();
      scrollDown();
      break;
    }

    case 'thinking_delta':
      if (!showReasoning) break;
      if (reasoningPanel) {
        pendingReasoningText += data.content;
        scheduleFlush();
      }
      break;

    case 'thinking_done':
      if (!showReasoning) {
        finishReasoningStream();
        reasoningPanel = null;
        break;
      }
      if (reasoningPanel) {
        finishReasoningStream();
        reasoningPanel.classList.remove('reasoning-active');
        const status = reasoningPanel.querySelector('.reasoning-status');
        const body = reasoningPanel.querySelector('.reasoning-body');
        const chevron = reasoningPanel.querySelector('.chevron');
        const rawText = body?._textNode?.nodeValue || body?.textContent || '';
        const summaryText = rawText.trim().replace(/\n+/g, ' ');
        const preview = summaryText.substring(0, 60);
        if (status) {
          status.textContent = preview ? preview + (summaryText.length > 60 ? '…' : '') : '完成';
          status.title = summaryText || '完成';
        }
        setTimeout(() => {
          if (body) animateCollapsibleSection(body, false);
          if (chevron) chevron.classList.remove('open');
        }, 600);
        reasoningPanel = null;
      }
      break;

    case 'tool_call':
      setReactActTool(data.name, 0);
      if (!showTools) break;
      addToolCall(data.name, data.arguments, data.id);
      break;

    case 'tool_progress':
      setReactActTool(data.name, data.elapsed_ms || 0);
      if (!showTools) break;
      updateToolProgress(data.id, data.elapsed_ms || 0);
      break;

    case 'tool_result':
      if (reactStatusPhase === 'act' && reactStatusToolName === data.name) {
        reactStatusElapsedMs = data.duration_ms || reactStatusElapsedMs;
        renderReactStatus();
      }
      if (!showTools) break;
      addToolResult(data.name, data.result, data.id, data.duration_ms ?? null);
      break;

    // ── Sub-agent task events ──
    case 'task_started':
      if (showTools) addSystem(`🤖 Sub-agent **${data.agent}** started`);
      break;
    case 'task_progress':
      // Cycle progress — silently consumed (visible via tool_progress on parent)
      break;
    case 'task_tool':
      if (showTools) addSystem(`🔧 **${data.agent}** → \`${data.tool}\``);
      break;
    case 'task_completed':
      if (showTools) addSystem(`✅ Sub-agent **${data.agent}** completed (${data.cycles} cycles, ${data.tool_calls} tools, ${formatToolDuration(data.duration_ms)})`);
      break;
    case 'task_failed':
      if (showTools) addSystem(`❌ Sub-agent **${data.agent}** failed${data.error ? ': ' + data.error : ''} (${data.cycles ?? 0} cycles, ${data.tool_calls ?? 0} tools${data.duration_ms ? ', ' + formatToolDuration(data.duration_ms) : ''})`);
      break;

    case 'context_compressed':
      addSystem(
        `Context auto-compressed: removed ${data.messages_removed || 0} messages, token estimate ${data.before_estimate || 0} -> ${data.after_estimate || 0}`
      );
      break;

    case 'progress':
      addSystem(data.content);
      break;

    case 'success':
      clearReactStatus();
      addSystem(data.content, 'success');
      setBusy(false);
      break;

    case 'system':
      clearReactStatus();
      addSystem(data.content);
      setBusy(false);
      break;

    case 'error':
      finishAssistantStream({ discardIfEmpty: true });
      finishReasoningStream();
      clearReactStatus();
      addError(data.content);
      reasoningPanel = null;
      setBusy(false);
      break;
  }
}

// ── Message rendering ──
function addMsg(cls, text, timestamp, options = {}) {
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

  chat.appendChild(row);
  if (trackUnread) {
    queueUnreadContent({ countable: true });
  }
  pinReactStatusToBottom();
  if (isChat) hideWelcome();
  scrollDown();
  return el;
}

function addAssistant(text, options = {}) { return addMsg('assistant', text, undefined, options); }

function renderUserImageThumbnails(msgEl, images) {
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
  // Insert the thumbnails after the message text bubble
  const row = msgEl.closest('.msg-row');
  if (row) {
    const content = row.querySelector('.msg-content');
    if (content) {
      content.insertBefore(container, content.querySelector('.msg-time'));
    }
  }
}

function addSystem(t, kind = 'info') {
  const row = document.createElement('div');
  row.className = 'msg-row system';
  const card = document.createElement('div');
  card.className = 'system-card';
  const icon = kind === 'success' ? '✅' : 'ℹ️';
  if (kind === 'success') card.classList.add('success-card');
  const isBlock = t.includes('\n') || t.length > 80;
  if (isBlock) {
    card.innerHTML = `<div class="system-header"><span class="system-icon">📋</span> System</div><pre class="system-body">${escHtml(t)}</pre>`;
  } else {
    card.classList.add('system-inline');
    card.innerHTML = `<span class="system-icon">${icon}</span> <span>${escHtml(t)}</span>`;
  }
  row.appendChild(card);
  chat.appendChild(row);
  queueUnreadContent({ countable: true });
  pinReactStatusToBottom();
  scrollDown();
}

function addError(t) {
  const row = document.createElement('div');
  row.className = 'msg-row error';
  const card = document.createElement('div');
  card.className = 'system-card system-inline error-card';
  card.innerHTML = `<span class="system-icon">⚠️</span> <span style="color:var(--accent-error)">${escHtml(t)}</span>`;
  row.appendChild(card);
  chat.appendChild(row);
  queueUnreadContent({ countable: true });
  pinReactStatusToBottom();
  scrollDown();
}

function addToolCall(name, args, id) {
  const panel = document.createElement('div');
  panel.className = 'tool-panel';
  panel.dataset.toolId = id;

  let argsDisplay = args;
  try { argsDisplay = JSON.stringify(JSON.parse(args), null, 2); } catch(e) {}
  panel.dataset.toolName = name;
  panel.dataset.toolArgs = argsDisplay;
  panel.dataset.toolResult = '';
  panel.dataset.toolHasResult = 'false';
  panel.dataset.toolStatus = '执行中';

  panel.innerHTML = `
    <div class="tool-header" onclick="openToolDrawerFromHeader(this)">
      <span class="tool-icon">⚡</span>
      <span class="tool-name">${escHtml(name)}</span>
      <span class="tool-args-preview">${escHtml(truncateStr(args, 80))}</span>
      <span class="tool-status">执行中</span>
    </div>
  `;
  const wrapper = wrapInTimeline(panel, 'tool');
  const currentRow = currentMsg ? currentMsg.closest('.msg-row') : null;
  if (currentRow) {
    chat.insertBefore(wrapper, currentRow);
  } else {
    chat.appendChild(wrapper);
  }
  pinReactStatusToBottom();
  animatePanelIn(panel);
  hideWelcome();
  scrollDown();
}

function updateToolProgress(id, elapsedMs) {
  if (!id) return;
  const seconds = Math.max(1, Math.floor((elapsedMs || 0) / 1000));
  for (const panel of chat.querySelectorAll('.tool-panel')) {
    if (panel.dataset.toolId !== id || panel.dataset.toolHasResult === 'true') {
      continue;
    }
    const statusText = `执行中 ${seconds}s`;
    panel.dataset.toolStatus = statusText;
    const statusEl = panel.querySelector('.tool-status');
    if (statusEl) {
      statusEl.textContent = statusText;
    }
    if (activeToolPanel === panel) {
      syncToolDrawer(panel);
    }
    return;
  }
}

function addToolResult(name, result, id, durationMs = null) {
  const panels = chat.querySelectorAll('.tool-panel');
  for (const p of panels) {
    if (p.dataset.toolId === id) {
      p.dataset.toolResult = result;
      p.dataset.toolHasResult = 'true';
      const durationLabel = formatToolDuration(durationMs);
      p.dataset.toolStatus = durationLabel ? `已返回结果 (${durationLabel})` : '已返回结果';
      const statusEl = p.querySelector('.tool-status');
      if (statusEl) {
        statusEl.textContent = p.dataset.toolStatus;
      }
      p.classList.add('tool-panel-ready');
      if (activeToolPanel === p) {
        syncToolDrawer(p);
      }
      return;
    }
  }
  // Fallback: standalone result
  const el = document.createElement('div');
  el.className = 'tool-panel tool-result';
  el.dataset.toolId = id || '';
  el.dataset.toolName = name ? `${name} result` : 'Tool result';
  el.dataset.toolArgs = '';
  el.dataset.toolResult = result;
  el.dataset.toolHasResult = 'true';
  const durationLabel = formatToolDuration(durationMs);
  el.dataset.toolStatus = durationLabel ? `已返回结果 (${durationLabel})` : '已返回结果';
  el.innerHTML = `
    <div class="tool-header" onclick="openToolDrawerFromHeader(this)">
      <span class="tool-icon">📋</span>
      <span class="tool-name">${escHtml(name)} result</span>
      <span class="tool-status">${escHtml(el.dataset.toolStatus)}</span>
    </div>
  `;
  el.classList.add('tool-panel-ready');
  const wrapper = wrapInTimeline(el, 'result');
  const currentRow = currentMsg ? currentMsg.closest('.msg-row') : null;
  if (currentRow) {
    chat.insertBefore(wrapper, currentRow);
  } else {
    chat.appendChild(wrapper);
  }
  pinReactStatusToBottom();
  animatePanelIn(el);
  scrollDown();
}

function renderMarkdown(el) {
  const raw = el._rawText || el.textContent;
  const { text: preprocessed, blocks: mathBlocks } = extractMath(raw);
  const html = marked.parse(preprocessed);
  const sanitized = typeof DOMPurify !== 'undefined' ? DOMPurify.sanitize(html) : escHtml(html);
  el.innerHTML = renderMathPlaceholders(sanitized, mathBlocks);
  el._markdownIdleHandle = 0;

  decorateCodeBlocks(el);
  scheduleCodeHighlight(el.querySelectorAll('pre code'));
}

// ── Helpers ──
function fallbackCopy(text) {
  const ta = document.createElement('textarea');
  ta.value = text;
  ta.style.cssText = 'position:fixed;left:-9999px';
  document.body.appendChild(ta);
  ta.select();
  document.execCommand('copy');
  document.body.removeChild(ta);
}

function escHtml(s) {
  s = String(s ?? '');
  return s.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;').replace(/'/g,'&#39;');
}
function truncateStr(s, max) {
  return s.length > max ? s.slice(0, max) + '…' : s;
}
function scrollDown(force = false) {
  if (bulkRenderingChat) {
    return false;
  }

  const shouldFollow = force || autoFollowChat || isChatNearBottom();
  if (!shouldFollow) {
    markChatUpdateOffscreen();
    return false;
  }

  suppressScrollTracking = true;
  chat.scrollTop = chat.scrollHeight;
  requestAnimationFrame(() => {
    suppressScrollTracking = false;
    setAutoFollowChat(true);
  });
  return true;
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

function formatTime(d) {
  return d.toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' });
}

function timeAgo(ts) {
  const diff = Date.now() - ts * 1000;
  const sec = Math.floor(diff / 1000);
  if (sec < 60) return '刚刚';
  const min = Math.floor(sec / 60);
  if (min < 60) return `${min}分钟前`;
  const hr = Math.floor(min / 60);
  if (hr < 24) return `${hr}小时前`;
  const day = Math.floor(hr / 24);
  if (day < 30) return `${day}天前`;
  return new Date(ts * 1000).toLocaleDateString('zh-CN');
}

function hideWelcome() {
  const w = document.getElementById('welcome');
  if (w) w.remove();
}

function showWelcome() {
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
      <button onclick="sendCmd('/clear')">New Conversation</button>
      <button onclick="sendCmd('/status')">Status</button>
      <button onclick="sendCmd('/help')">Help</button>
    </div>
  `;
  chat.appendChild(w);
  syncVersionBadges();
}

function setBusy(b) {
  busy = b;
  stopBtn.style.display = b ? 'flex' : 'none';
  stopBtn.disabled = !b;
  sendBtn.disabled = false;
  if (b) {
    input.placeholder = 'Message LingClaw... (运行中可发送干预，点击红色按钮停止)';
  } else {
    input.placeholder = 'Message LingClaw... (/ for commands)';
  }
  sendIcon.innerHTML = '↑';
  sendBtn.title = '';
  sendBtn.setAttribute('aria-label', 'Send message');
}

function syncToolDrawer(panel) {
  if (!panel || !toolDrawer) return;
  const toolName = panel.dataset.toolName || 'Tool';
  const toolArgs = panel.dataset.toolArgs || '';
  const toolResult = panel.dataset.toolResult || '';
  const hasResult = panel.dataset.toolHasResult === 'true';
  const statusText = panel.dataset.toolStatus || (hasResult ? '已返回结果' : '执行中');

  toolDrawerTitle.textContent = toolName;
  toolDrawerMeta.textContent = statusText;
  toolDrawerArgs.textContent = toolArgs || '(empty)';
  toolDrawerResult.textContent = toolResult;
  toolDrawerResultSection.hidden = !hasResult;
}

function openToolDrawer(panel) {
  if (!panel || !toolDrawer || !toolDrawerBackdrop) return;
  syncToolDrawerBounds();
  if (activeToolPanel && activeToolPanel !== panel) {
    activeToolPanel.classList.remove('tool-panel-active');
  }
  activeToolPanel = panel;
  activeToolPanel.classList.add('tool-panel-active');
  syncToolDrawer(panel);
  toolDrawer.classList.add('open');
  toolDrawerBackdrop.classList.add('open');
  toolDrawer.setAttribute('aria-hidden', 'false');
}

function openToolDrawerFromHeader(header) {
  openToolDrawer(header.closest('.tool-panel'));
}

function closeToolDrawer() {
  if (!toolDrawer || !toolDrawerBackdrop) return;
  toolDrawer.classList.remove('open');
  toolDrawerBackdrop.classList.remove('open');
  toolDrawer.setAttribute('aria-hidden', 'true');
  if (activeToolPanel) {
    activeToolPanel.classList.remove('tool-panel-active');
    activeToolPanel = null;
  }
}

function toggleTool(header) {
  const chevron = header.querySelector('.chevron');
  const body = header.nextElementSibling;
  const nextOpen = !body.classList.contains('show');
  if (chevron) chevron.classList.toggle('open', nextOpen);
  animateCollapsibleSection(body, nextOpen);
}

// ── Mobile menu ──
function syncMobileMenuAria(open) {
  const toggle = document.getElementById('mobile-menu-toggle');
  if (toggle) toggle.setAttribute('aria-expanded', String(open));
}
function toggleMobileMenu() {
  const menu = document.getElementById('mobile-menu');
  if (!menu) return;
  const willOpen = !menu.classList.contains('open');
  menu.classList.toggle('open', willOpen);
  syncMobileMenuAria(willOpen);
}
function closeMobileMenu() {
  const menu = document.getElementById('mobile-menu');
  if (menu) menu.classList.remove('open');
  syncMobileMenuAria(false);
}
document.addEventListener('click', (e) => {
  const toggle = document.getElementById('mobile-menu-toggle');
  const menu = document.getElementById('mobile-menu');
  if (menu && toggle && !toggle.contains(e.target) && !menu.contains(e.target)) {
    closeMobileMenu();
  }
});

// ── History lazy-load ──
function findHistoryRenderStart(messages, preferredStart) {
  let startIdx = Math.max(0, preferredStart);
  if (startIdx === 0) {
    return 0;
  }

  const toolCallById = new Map();
  for (let i = 0; i < messages.length; i++) {
    const message = messages[i];
    if (message.role === 'tool_call' && message.id) {
      toolCallById.set(message.id, i);
    }
  }

  let expanded = true;
  while (expanded) {
    expanded = false;
    for (let i = startIdx; i < messages.length; i++) {
      const message = messages[i];
      if (message.role !== 'tool_result' || !message.id) {
        continue;
      }

      const callIdx = toolCallById.get(message.id);
      if (callIdx !== undefined && callIdx < startIdx) {
        startIdx = callIdx;
        expanded = true;
      }
    }
  }

  return startIdx;
}

function renderHistoryMessage(m, options = {}) {
  const { followMarkdown = true } = options;
  switch (m.role) {
    case 'user': {
      const el = addMsg('user', m.content, m.timestamp);
      if (m.images && m.images.length > 0) renderUserImageThumbnails(el, m.images);
      break;
    }
    case 'assistant': {
      const el = addMsg('assistant', m.content, m.timestamp);
      el._rawText = m.content;
      scheduleMarkdownRender(el, { followScroll: followMarkdown });
      break;
    }
    case 'tool_call': if (showTools) addToolCall(m.name, m.arguments, m.id); break;
    case 'tool_result': if (showTools) addToolResult('', m.result, m.id); break;
  }
}

function loadEarlierMessages() {
  const msgs = _deferredHistory;
  _deferredHistory = [];
  const loadMoreRow = document.getElementById('load-more-row');
  // The first child after load-more-row is the anchor we want to scroll to
  const anchor = loadMoreRow ? loadMoreRow.nextElementSibling : chat.firstElementChild;
  if (loadMoreRow) loadMoreRow.remove();
  const existing = [...chat.children];
  chat.replaceChildren();
  chat.classList.add('no-animate');
  bulkRenderingChat = true;
  for (const m of msgs) renderHistoryMessage(m, { followMarkdown: false });
  for (const el of existing) chat.appendChild(el);
  requestAnimationFrame(() => {
    bulkRenderingChat = false;
    chat.classList.remove('no-animate');
    if (anchor) anchor.scrollIntoView({ block: 'start' });
    requestAnimationFrame(syncChatScrollState);
  });
}

// ── Input ──
function send() {
  if (!ws || ws.readyState !== 1) return;

  const text = input.value.trim();
  if (!text && pendingImages.length === 0) return;

  if (text.startsWith('/') && pendingImages.length === 0) {
    if (busy && !canSendWhileBusy(text)) {
      addSystem('Agent 运行中时，只允许 /stop、/tool 和 /reasoning。');
      return;
    }
    sendCmd(text);
    pushInputHistory(text);
    input.value = '';
    input.style.height = 'auto';
    syncToolDrawerBounds();
    return;
  }

  const hasImages = pendingImages.length > 0;

  // When agent is busy, images are dropped server-side; don't render them
  // optimistically — the user would see thumbnails followed by a contradicting
  // "text only" server notice.
  const effectiveImages = busy ? [] : pendingImages.slice();

  // Render user message with images
  const el = addMsg('user', text || '(image)');
  if (effectiveImages.length > 0) {
    renderUserImageThumbnails(el, effectiveImages);
  }
  scrollDown(true);

  if (!busy) {
    setBusy(true);
  }
  // When busy, the server sends its own progress event confirming receipt
  // (with/without "images dropped" context), so no local addSystem here.

  // Send structured JSON when images are attached, plain text otherwise
  if (hasImages) {
    ws.send(JSON.stringify({ text: text || '', images: pendingImages }));
    pendingImages = [];
    renderImagePreviews();
  } else {
    ws.send(text);
  }
  pushInputHistory(text);
  input.value = '';
  input.style.height = 'auto';
  syncToolDrawerBounds();
}

function pushInputHistory(text) {
  if (!text) return;
  // Avoid consecutive duplicates
  if (inputHistory.length > 0 && inputHistory[inputHistory.length - 1] === text) {
    inputHistoryIndex = -1;
    return;
  }
  inputHistory.push(text);
  if (inputHistory.length > INPUT_HISTORY_MAX) inputHistory.shift();
  inputHistoryIndex = -1;
}

function stopAgent() {
  if (!busy || !ws || ws.readyState !== 1) return;
  ws.send('/stop');
}

function sendCmd(cmd) {
  if ((!canSendWhileBusy(cmd) && busy) || !ws || ws.readyState !== 1) return;
  setBusy(true);
  ws.send(cmd);
}

updateViewToggleButtons();
syncToolDrawerBounds();
updateJumpToLatestVisibility();

input.addEventListener('keydown', (e) => {
  if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); send(); return; }
  if ((e.key === 'ArrowUp' || e.key === 'ArrowDown') && !e.shiftKey && inputHistory.length > 0) {
    // Only intercept when cursor is on the first/last line
    const val = input.value;
    const pos = input.selectionStart;
    if (e.key === 'ArrowUp') {
      const textBefore = val.slice(0, pos);
      if (textBefore.includes('\n')) return; // not on first line
      e.preventDefault();
      if (inputHistoryIndex === -1) {
        inputHistoryDraft = val;
        inputHistoryIndex = inputHistory.length - 1;
      } else if (inputHistoryIndex > 0) {
        inputHistoryIndex--;
      }
      input.value = inputHistory[inputHistoryIndex];
      input.setSelectionRange(input.value.length, input.value.length);
    } else {
      const textAfter = val.slice(pos);
      if (textAfter.includes('\n')) return; // not on last line
      e.preventDefault();
      if (inputHistoryIndex === -1) return;
      if (inputHistoryIndex < inputHistory.length - 1) {
        inputHistoryIndex++;
        input.value = inputHistory[inputHistoryIndex];
      } else {
        inputHistoryIndex = -1;
        input.value = inputHistoryDraft;
      }
      input.setSelectionRange(input.value.length, input.value.length);
    }
    input.style.height = 'auto';
    input.style.height = Math.min(input.scrollHeight, 120) + 'px';
  }
});
document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') {
    closeToolDrawer();
    closeMobileMenu();
  }
});
input.addEventListener('input', () => {
  input.style.height = 'auto';
  input.style.height = Math.min(input.scrollHeight, 120) + 'px';
  syncToolDrawerBounds();
});
chat.addEventListener('scroll', () => {
  syncChatScrollState();
}, { passive: true });
window.addEventListener('resize', syncToolDrawerBounds);
window.addEventListener('resize', () => {
  if (window.innerWidth > 768) closeMobileMenu();
});
if (window.visualViewport) {
  window.visualViewport.addEventListener('resize', syncToolDrawerBounds);
  window.visualViewport.addEventListener('scroll', syncToolDrawerBounds);
}
if (jumpToLatestBtn) {
  jumpToLatestBtn.addEventListener('click', () => {
    jumpToLatest();
  });
}
sendBtn.addEventListener('click', () => {
  send();
});
stopBtn.addEventListener('click', () => {
  stopAgent();
});

void loadAppVersion();
connect();
