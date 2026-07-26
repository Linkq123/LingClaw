import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  addImageUrl,
  ensureUploadTokenInternal,
  openAttachPopup,
  removeImage,
  renderImagePreviews,
  syncPlanModeToggle,
  togglePlanMode,
  updateAttachButton,
  updateS3ConfigIdentity,
  uploadLocalImages,
} from '../src/images.js';
import {
  beginComposerSessionTransition,
  restoreComposerSessionTransition,
} from '../src/composerAvailability.js';
import { dom, initDomRefs, state } from '../src/state.js';
import { setLanguage, translateDom } from '../src/i18n.js';

function jsonResponse(payload: unknown): Response {
  return new Response(JSON.stringify(payload), {
    status: 200,
    headers: { 'Content-Type': 'application/json' },
  });
}

function deferredResponse() {
  let resolve!: (response: Response) => void;
  const promise = new Promise<Response>((resolveResponse) => {
    resolve = resolveResponse;
  });
  return { promise, resolve };
}

describe('ensureUploadTokenInternal', () => {
  beforeEach(() => {
    setLanguage('en');
    document.body.innerHTML = `
      <div class="attach-wrapper">
        <button id="attach-btn"></button>
        <div id="attach-popup" style="display: none">
          <div id="attach-menu">
            <button id="attach-local-btn"></button>
            <button id="plan-mode-toggle" class="attach-menu-toggle" aria-checked="false"></button>
          </div>
          <div id="attach-upload-status" style="display: none"></div>
        </div>
      </div>
      <div id="image-preview-bar"></div>
    `;
    initDomRefs();
    state.uploadToken = '';
    state.uploadTokenPromise = null;
    state.uploadTokenRequestSeq = 0;
    state.imageCapable = false;
    state.s3Capable = false;
    state.s3ConfigId = '';
    state.planModeEnabled = false;
    state.pendingImages = [];
    state.imageUploadInFlight = false;
    state.sessionSwitchInFlight = false;
    state.sessionIdentityMutationInFlight = false;
    state.composerSessionIdentityPending = false;
    state.composerSessionTransitionPending = false;
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('keeps the newest forced refresh token when an older request resolves later', async () => {
    const first = deferredResponse();
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockReturnValueOnce(first.promise)
      .mockResolvedValueOnce(
        jsonResponse({ upload_token: 'fresh-token', s3_config_id: 'fresh-s3' }),
      );
    vi.stubGlobal('fetch', fetchMock);

    const firstRequest = ensureUploadTokenInternal(false);
    const refreshedRequest = ensureUploadTokenInternal(true);

    expect(fetchMock).toHaveBeenCalledTimes(2);

    await expect(refreshedRequest).resolves.toBe('fresh-token');
    expect(state.uploadToken).toBe('fresh-token');
    expect(state.s3ConfigId).toBe('fresh-s3');

    first.resolve(jsonResponse({ upload_token: 'stale-token', s3_config_id: 'stale-s3' }));

    await expect(firstRequest).resolves.toBe('stale-token');
    expect(state.uploadToken).toBe('fresh-token');
    expect(state.s3ConfigId).toBe('fresh-s3');
    expect(state.uploadTokenPromise).toBeNull();
  });
});

describe('attachment menu', () => {
  beforeEach(() => {
    setLanguage('en');
    document.body.innerHTML = `
      <div class="attach-wrapper">
        <button id="attach-btn"></button>
        <div id="attach-popup" style="display: none">
          <div id="attach-menu">
            <button id="attach-local-btn"></button>
            <button id="plan-mode-toggle" class="attach-menu-toggle" aria-checked="false"></button>
          </div>
          <div id="attach-upload-status" style="display: none"></div>
        </div>
      </div>
      <div id="image-preview-bar"></div>
    `;
    initDomRefs();
    state.imageCapable = false;
    state.s3Capable = false;
    state.s3ConfigId = '';
    state.planModeEnabled = false;
    state.pendingImages = [];
    state.imageUploadInFlight = false;
    state.sessionSwitchInFlight = false;
    state.sessionIdentityMutationInFlight = false;
    state.composerSessionIdentityPending = false;
    state.composerSessionTransitionPending = false;
  });

  it('keeps the plus menu visible and only shows image upload when uploads are available', () => {
    updateAttachButton();

    expect(dom.attachBtn?.style.display).toBe('');
    expect(dom.attachLocalBtn?.hidden).toBe(true);

    state.imageCapable = true;
    updateAttachButton();
    expect(dom.attachLocalBtn?.hidden).toBe(true);

    state.s3Capable = true;
    state.s3ConfigId = 's3-a';
    updateAttachButton();
    expect(dom.attachLocalBtn?.hidden).toBe(false);
  });

  it('opens the compact menu instead of the legacy URL input', () => {
    openAttachPopup();

    expect(dom.attachPopup?.style.display).toBe('block');
    expect(dom.attachMenu?.style.display).toBe('flex');
    expect(dom.attachUrlInput).toBeNull();
  });

  it('syncs plan mode switch state', () => {
    syncPlanModeToggle();
    expect(dom.planModeToggle?.getAttribute('aria-checked')).toBe('false');
    expect(dom.planModeToggle?.classList.contains('is-on')).toBe(false);

    togglePlanMode();

    expect(state.planModeEnabled).toBe(true);
    expect(dom.planModeToggle?.getAttribute('aria-checked')).toBe('true');
    expect(dom.planModeToggle?.classList.contains('is-on')).toBe(true);
  });

  it('uses the shared close icon for pending image removal', () => {
    state.pendingImages = [{ url: 'https://example.com/image.png' }];

    renderImagePreviews();

    const removeButton = dom.imagePreviewBar?.querySelector<HTMLButtonElement>('.remove-btn');
    expect(removeButton?.querySelector('use')?.getAttribute('href')).toBe('#icon-close');
    expect(removeButton?.getAttribute('aria-label')).toBe('Remove image');

    setLanguage('zh-CN');
    translateDom();

    expect(removeButton?.getAttribute('aria-label')).toBe('移除图片');
  });

  it('preserves the source draft by blocking attachment changes during a Session switch', () => {
    state.imageCapable = true;
    state.pendingImages = [{ url: 'https://example.com/source.png' }];
    beginComposerSessionTransition(true, 'missing-session');
    updateAttachButton();

    expect(dom.attachBtn?.disabled).toBe(true);
    addImageUrl('https://example.com/new.png');
    removeImage(0);
    expect(state.pendingImages).toEqual([{ url: 'https://example.com/source.png' }]);

    restoreComposerSessionTransition();
    expect(state.pendingImages).toEqual([{ url: 'https://example.com/source.png' }]);
  });
});

describe('local image upload lifecycle', () => {
  beforeEach(() => {
    setLanguage('en');
    document.body.innerHTML = `
      <div id="chat"></div>
      <div class="attach-wrapper">
        <button id="attach-btn"></button>
        <div id="attach-popup" style="display: none">
          <div id="attach-menu">
            <button id="attach-local-btn"></button>
          </div>
          <div id="attach-upload-status" style="display: none"></div>
        </div>
        <input id="image-file-input" type="file" />
      </div>
      <div id="image-preview-bar"></div>
      <textarea id="input">describe the image</textarea>
      <button id="send"></button>
      <p id="composer-availability-status">
        <span id="composer-availability-message"></span>
        <button id="composer-availability-retry"></button>
      </p>
    `;
    initDomRefs();
    state.ws = null;
    state.activeSessionId = 'main';
    state.activeGroupId = '';
    state.sessionSwitchInFlight = false;
    state.sessionIdentityMutationInFlight = false;
    state.composerSessionIdentityPending = false;
    state.composerSessionTransitionPending = false;
    state.storageMode = 'healthy';
    state.composerModelAvailability = 'ready';
    state.imageCapable = true;
    state.s3Capable = true;
    state.s3ConfigId = 's3-a';
    state.pendingImages = [];
    state.imageUploadInFlight = false;
    state.uploadToken = 'upload-token';
    state.uploadTokenPromise = null;
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('keeps ordinary sending disabled until a successful upload is applied', async () => {
    const upload = deferredResponse();
    vi.stubGlobal('fetch', vi.fn<typeof fetch>().mockReturnValue(upload.promise));

    const pending = uploadLocalImages([new File(['image'], 'image.png', { type: 'image/png' })]);

    expect(state.imageUploadInFlight).toBe(true);
    expect(dom.sendBtn?.disabled).toBe(true);
    expect(dom.input?.placeholder).toContain('image upload');

    upload.resolve(
      jsonResponse({
        s3_config_id: 's3-a',
        images: [
          {
            url: 'https://images.example/uploaded.png',
            object_key: 'uploads/uploaded.png',
            attachment_token: 'attachment-token',
            s3_config_id: 's3-a',
          },
        ],
        urls: ['https://images.example/uploaded.png'],
      }),
    );
    await pending;

    expect(state.imageUploadInFlight).toBe(false);
    expect(state.pendingImages).toEqual([
      {
        url: 'https://images.example/uploaded.png',
        object_key: 'uploads/uploaded.png',
        attachment_token: 'attachment-token',
        s3_config_id: 's3-a',
      },
    ]);
    expect(dom.sendBtn?.disabled).toBe(false);
  });

  it('clears a rejected file selection so the same file can be selected again', async () => {
    const input = dom.imageFileInput!;
    Object.defineProperty(input, 'value', {
      configurable: true,
      writable: true,
      value: 'C:\\fakepath\\image.png',
    });
    state.sessionSwitchInFlight = true;

    await uploadLocalImages([new File(['image'], 'image.png', { type: 'image/png' })]);

    expect(input.value).toBe('');
    expect(state.imageUploadInFlight).toBe(false);
  });

  it('restores composer availability when an upload request fails', async () => {
    vi.stubGlobal('fetch', vi.fn<typeof fetch>().mockRejectedValue(new Error('network down')));

    await uploadLocalImages([new File(['image'], 'image.png', { type: 'image/png' })]);

    expect(state.imageUploadInFlight).toBe(false);
    expect(state.pendingImages).toEqual([]);
    expect(dom.sendBtn?.disabled).toBe(false);
    expect(dom.chat?.textContent).toContain('network down');
  });

  it('discards a completed upload after the active capability is revoked', async () => {
    const upload = deferredResponse();
    vi.stubGlobal('fetch', vi.fn<typeof fetch>().mockReturnValue(upload.promise));
    const pending = uploadLocalImages([new File(['image'], 'image.png', { type: 'image/png' })]);

    state.s3Capable = false;
    upload.resolve(
      jsonResponse({
        s3_config_id: 's3-a',
        images: [
          {
            url: 'https://images.example/stale.png',
            object_key: 'uploads/stale.png',
            attachment_token: 'attachment-token',
            s3_config_id: 's3-a',
          },
        ],
        urls: ['https://images.example/stale.png'],
      }),
    );
    await pending;

    expect(state.pendingImages).toEqual([]);
    expect(dom.chat?.textContent).toContain('upload was discarded');
  });

  it('discards an in-flight upload when storage enters protected mode', async () => {
    const upload = deferredResponse();
    vi.stubGlobal('fetch', vi.fn<typeof fetch>().mockReturnValue(upload.promise));
    const pending = uploadLocalImages([new File(['image'], 'image.png', { type: 'image/png' })]);

    state.storageMode = 'protected';
    upload.resolve(
      jsonResponse({
        s3_config_id: 's3-a',
        images: [
          {
            url: 'https://images.example/stale.png',
            object_key: 'uploads/stale.png',
            attachment_token: 'attachment-token',
            s3_config_id: 's3-a',
          },
        ],
        urls: ['https://images.example/stale.png'],
      }),
    );
    await pending;

    expect(state.pendingImages).toEqual([]);
    expect(dom.chat?.textContent).toContain('upload was discarded');
  });

  it('discards a completed upload after its source socket disconnects', async () => {
    const upload = deferredResponse();
    vi.stubGlobal('fetch', vi.fn<typeof fetch>().mockReturnValue(upload.promise));
    const sourceSocket = { readyState: 1 } as WebSocket;
    state.ws = sourceSocket;
    const pending = uploadLocalImages([new File(['image'], 'image.png', { type: 'image/png' })]);

    Object.assign(sourceSocket, { readyState: 3 });
    state.activeSessionId = 'main-fallback';
    upload.resolve(
      jsonResponse({
        s3_config_id: 's3-a',
        images: [
          {
            url: 'https://images.example/stale.png',
            object_key: 'uploads/stale.png',
            attachment_token: 'attachment-token',
            s3_config_id: 's3-a',
          },
        ],
        urls: ['https://images.example/stale.png'],
      }),
    );
    await pending;

    expect(state.pendingImages).toEqual([]);
    expect(dom.chat?.textContent).toContain('upload was discarded');
  });

  it('drops only trusted pending uploads when the S3 configuration changes', () => {
    state.pendingImages = [
      { url: 'https://example.com/remote.png' },
      {
        url: 'https://images.example/uploaded.png',
        object_key: 'uploads/uploaded.png',
        attachment_token: 'attachment-token',
        s3_config_id: 's3-a',
      },
    ];
    renderImagePreviews();

    expect(updateS3ConfigIdentity('s3-b', true)).toBe(true);

    expect(state.pendingImages).toEqual([{ url: 'https://example.com/remote.png' }]);
    expect(dom.chat?.textContent).toContain('storage settings changed');
  });

  it('discards an in-flight upload when the S3 configuration changes', async () => {
    const upload = deferredResponse();
    vi.stubGlobal('fetch', vi.fn<typeof fetch>().mockReturnValue(upload.promise));
    const pending = uploadLocalImages([new File(['image'], 'image.png', { type: 'image/png' })]);

    updateS3ConfigIdentity('s3-b');
    upload.resolve(
      jsonResponse({
        s3_config_id: 's3-a',
        images: [
          {
            url: 'https://images.example/stale.png',
            object_key: 'uploads/stale.png',
            attachment_token: 'attachment-token',
            s3_config_id: 's3-a',
          },
        ],
        urls: ['https://images.example/stale.png'],
      }),
    );
    await pending;

    expect(state.pendingImages).toEqual([]);
    expect(dom.chat?.textContent).toContain('upload was discarded');
  });

  it('refreshes the client configuration before unlocking after an upload identity mismatch', async () => {
    const upload = deferredResponse();
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockReturnValueOnce(upload.promise)
      .mockResolvedValueOnce(
        jsonResponse({ upload_token: 'replacement-token', s3_config_id: 's3-b' }),
      );
    vi.stubGlobal('fetch', fetchMock);
    const pending = uploadLocalImages([new File(['image'], 'image.png', { type: 'image/png' })]);

    upload.resolve(
      jsonResponse({
        s3_config_id: 's3-b',
        images: [
          {
            url: 'https://images.example/new-storage.png',
            object_key: 'uploads/new-storage.png',
            attachment_token: 'replacement-token',
            s3_config_id: 's3-b',
          },
        ],
        urls: ['https://images.example/new-storage.png'],
      }),
    );
    await pending;

    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(state.s3ConfigId).toBe('s3-b');
    expect(state.uploadToken).toBe('replacement-token');
    expect(state.pendingImages).toEqual([]);
    expect(state.imageUploadInFlight).toBe(false);
    expect(dom.chat?.textContent).toContain('upload was discarded');
  });
});
