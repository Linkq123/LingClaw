import { dom, state } from './state.js';
import { addSystem } from './renderers/chat.js';
import { tr } from './i18n.js';

// Guard: prevent double-registration on Vite HMR re-execution of main.ts.
let _listenerInit = false;

// ── Upload token ──

export async function ensureUploadToken() {
  return ensureUploadTokenInternal(false);
}

export async function ensureUploadTokenInternal(forceRefresh: boolean): Promise<string> {
  if (forceRefresh) {
    state.uploadToken = '';
    state.uploadTokenPromise = null;
  }
  if (state.uploadToken) return state.uploadToken;
  if (!state.uploadTokenPromise) {
    const requestSeq = state.uploadTokenRequestSeq + 1;
    state.uploadTokenRequestSeq = requestSeq;
    state.uploadTokenPromise = fetch('/api/client-config', { cache: 'no-store' })
      .then(async (response) => {
        if (!response.ok) {
          throw new Error(`client config request failed (${response.status})`);
        }
        const data = await response.json();
        if (typeof data.upload_token !== 'string' || !data.upload_token) {
          throw new Error('upload token missing');
        }
        if (requestSeq === state.uploadTokenRequestSeq) {
          state.uploadToken = data.upload_token;
        }
        return data.upload_token;
      })
      .finally(() => {
        if (requestSeq === state.uploadTokenRequestSeq) {
          state.uploadTokenPromise = null;
        }
      });
  }
  return state.uploadTokenPromise;
}

// ── Image Attachment ──

export function updateAttachButton() {
  if (dom.attachBtn) dom.attachBtn.style.display = '';
  if (dom.attachLocalBtn) dom.attachLocalBtn.hidden = !(state.imageCapable && state.s3Capable);
  syncPlanModeToggle();
  if (!state.imageCapable && state.pendingImages.length > 0) {
    state.pendingImages = [];
    renderImagePreviews();
  }
}

export function syncPlanModeToggle() {
  if (!dom.planModeToggle) return;
  dom.planModeToggle.setAttribute('aria-checked', state.planModeEnabled ? 'true' : 'false');
  dom.planModeToggle.classList.toggle('is-on', state.planModeEnabled);
}

export function togglePlanMode() {
  state.planModeEnabled = !state.planModeEnabled;
  syncPlanModeToggle();
}

function isUploadedPendingImage(image) {
  return !!(image && (image.object_key || image.attachment_token));
}

export function dropUnavailablePendingUploads(notify = false) {
  if (state.pendingImages.length === 0) return;
  const keptImages = state.pendingImages.filter((image) => !isUploadedPendingImage(image));
  if (keptImages.length === state.pendingImages.length) return;
  state.pendingImages = keptImages;
  renderImagePreviews();
  closeAttachPopup();
  if (dom.imageFileInput) dom.imageFileInput.value = '';
  if (notify) {
    addSystem(tr('composer.uploadUnavailable'));
  }
}

export function closeAttachPopup() {
  if (dom.attachPopup) dom.attachPopup.style.display = 'none';
  if (dom.attachUrlInput) dom.attachUrlInput.style.display = 'none';
  if (dom.attachUploadStatus) dom.attachUploadStatus.style.display = 'none';
  if (dom.attachMenu) dom.attachMenu.style.display = 'flex';
}

export function openAttachPopup() {
  if (!dom.attachPopup) return;
  updateAttachButton();
  if (dom.attachMenu) dom.attachMenu.style.display = 'flex';
  if (dom.attachUrlInput) dom.attachUrlInput.style.display = 'none';
  if (dom.attachUploadStatus) dom.attachUploadStatus.style.display = 'none';
  dom.attachPopup.style.display = 'block';
}

export function toggleAttachPopup() {
  if (!dom.attachPopup) return;
  if (dom.attachPopup.style.display === 'none' || !dom.attachPopup.style.display) {
    openAttachPopup();
  } else {
    closeAttachPopup();
  }
}

export function addImageUrl(url) {
  if (!url || !url.trim()) return;
  const trimmed = url.trim();
  let parsed;
  try {
    parsed = new URL(trimmed);
  } catch {
    addSystem(tr('composer.invalidUrl'));
    return;
  }
  if (!trimmed.startsWith('http://') && !trimmed.startsWith('https://')) {
    addSystem(tr('composer.httpOnly'));
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
      addSystem(tr('composer.imageFormats'));
      return;
    } else {
      addSystem(tr('composer.notImageUrl'));
      return;
    }
  }
  state.pendingImages.push({ url: trimmed });
  renderImagePreviews();
  closeAttachPopup();
}

export async function uploadLocalImages(files) {
  if (!files || files.length === 0) return;
  if (dom.attachUploadStatus) {
    if (dom.attachMenu) dom.attachMenu.style.display = 'none';
    if (dom.attachUrlInput) dom.attachUrlInput.style.display = 'none';
    dom.attachUploadStatus.style.display = 'flex';
  }
  let token;
  try {
    token = await ensureUploadToken();
  } catch (e) {
    addSystem(tr('composer.uploadFailed', { error: e.message }));
    closeAttachPopup();
    if (dom.imageFileInput) dom.imageFileInput.value = '';
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
      body: formData,
    });
    if (resp.status === 403) {
      token = await ensureUploadTokenInternal(true);
      resp = await fetch('/api/upload-images', {
        method: 'POST',
        headers: { 'X-LingClaw-Upload-Token': token },
        body: formData,
      });
    }
    if (!resp.ok) {
      if (resp.status === 403) {
        state.uploadToken = '';
      }
      const errText = await resp.text().catch(() => resp.statusText);
      addSystem(tr('composer.uploadFailed', { error: errText }));
      closeAttachPopup();
      if (dom.imageFileInput) dom.imageFileInput.value = '';
      return;
    }
    const data = await resp.json();
    if (data.images && data.images.length > 0) {
      for (const image of data.images) {
        state.pendingImages.push({
          url: image.url,
          object_key: image.object_key,
          attachment_token: image.attachment_token,
        });
      }
      renderImagePreviews();
    } else if (data.urls && data.urls.length > 0) {
      for (const url of data.urls) {
        state.pendingImages.push({ url });
      }
      renderImagePreviews();
    }
    if (data.errors && data.errors.length > 0) {
      for (const err of data.errors) {
        addSystem(tr('composer.uploadError', { error: err }));
      }
    }
    if (!data.urls || data.urls.length === 0) {
      if (!data.errors || data.errors.length === 0) {
        addSystem(tr('composer.noImagesUploaded'));
      }
    }
  } catch (e) {
    addSystem(tr('composer.uploadFailed', { error: e.message }));
  }
  closeAttachPopup();
  if (dom.imageFileInput) dom.imageFileInput.value = '';
}

export function removeImage(index) {
  state.pendingImages.splice(index, 1);
  renderImagePreviews();
}

export function renderImagePreviews() {
  if (!dom.imagePreviewBar) return;
  dom.imagePreviewBar.innerHTML = '';
  if (state.pendingImages.length === 0) {
    dom.imagePreviewBar.style.display = 'none';
    return;
  }
  dom.imagePreviewBar.style.display = 'flex';
  state.pendingImages.forEach((img, idx) => {
    const item = document.createElement('div');
    item.className = 'image-preview-item';
    const imgEl = document.createElement('img');
    // Pending-upload previews stay above the composer so `lazy` rarely helps
    // visually, but costs nothing and avoids a cold decode on slow devices
    // when the user stages many images.
    imgEl.loading = 'lazy';
    imgEl.decoding = 'async';
    imgEl.src = img.url;
    imgEl.alt = 'Attached image';
    imgEl.onerror = () => {
      imgEl.style.display = 'none';
    };
    const removeBtn = document.createElement('button');
    removeBtn.className = 'remove-btn';
    removeBtn.textContent = '\u00d7';
    removeBtn.addEventListener('click', (e) => {
      e.stopPropagation();
      removeImage(idx);
    });
    item.appendChild(imgEl);
    item.appendChild(removeBtn);
    dom.imagePreviewBar.appendChild(item);
  });
}

export function initImageListeners() {
  if (_listenerInit) return;
  _listenerInit = true;
  if (dom.attachBtn)
    dom.attachBtn.addEventListener('click', (e) => {
      e.stopPropagation();
      toggleAttachPopup();
    });
  if (dom.attachUrlBtn)
    dom.attachUrlBtn.addEventListener('click', () => {
      if (dom.attachMenu) dom.attachMenu.style.display = 'none';
      if (dom.attachUrlInput) dom.attachUrlInput.style.display = 'flex';
      if (dom.imageUrlField) setTimeout(() => dom.imageUrlField.focus(), 50);
    });
  if (dom.attachLocalBtn)
    dom.attachLocalBtn.addEventListener('click', () => {
      if (dom.imageFileInput) dom.imageFileInput.click();
    });
  if (dom.planModeToggle)
    dom.planModeToggle.addEventListener('click', (e) => {
      e.stopPropagation();
      togglePlanMode();
    });
  if (dom.imageUrlAddBtn)
    dom.imageUrlAddBtn.addEventListener('click', () => {
      if (dom.imageUrlField) {
        addImageUrl(dom.imageUrlField.value);
        dom.imageUrlField.value = '';
      }
    });
  if (dom.imageUrlField)
    dom.imageUrlField.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') {
        e.preventDefault();
        addImageUrl(dom.imageUrlField.value);
        dom.imageUrlField.value = '';
      }
      if (e.key === 'Escape') closeAttachPopup();
    });
  if (dom.imageFileInput)
    dom.imageFileInput.addEventListener('change', () => {
      if (dom.imageFileInput.files && dom.imageFileInput.files.length > 0) {
        uploadLocalImages(dom.imageFileInput.files);
      }
    });
  document.addEventListener('click', (e) => {
    const target = e.target;
    if (!(target instanceof Node)) return;
    if (dom.attachPopup && dom.attachPopup.style.display !== 'none') {
      const wrapper = dom.attachBtn ? dom.attachBtn.closest('.attach-wrapper') : null;
      if (wrapper && !wrapper.contains(target)) {
        closeAttachPopup();
      }
    }
  });
  updateAttachButton();
}
