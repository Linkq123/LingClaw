import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  ensureUploadTokenInternal,
  openAttachPopup,
  renderImagePreviews,
  syncPlanModeToggle,
  togglePlanMode,
  updateAttachButton,
} from '../src/images.js';
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
    state.planModeEnabled = false;
    state.pendingImages = [];
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('keeps the newest forced refresh token when an older request resolves later', async () => {
    const first = deferredResponse();
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockReturnValueOnce(first.promise)
      .mockResolvedValueOnce(jsonResponse({ upload_token: 'fresh-token' }));
    vi.stubGlobal('fetch', fetchMock);

    const firstRequest = ensureUploadTokenInternal(false);
    const refreshedRequest = ensureUploadTokenInternal(true);

    expect(fetchMock).toHaveBeenCalledTimes(2);

    await expect(refreshedRequest).resolves.toBe('fresh-token');
    expect(state.uploadToken).toBe('fresh-token');

    first.resolve(jsonResponse({ upload_token: 'stale-token' }));

    await expect(firstRequest).resolves.toBe('stale-token');
    expect(state.uploadToken).toBe('fresh-token');
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
    state.planModeEnabled = false;
    state.pendingImages = [];
  });

  it('keeps the plus menu visible and only shows image upload when uploads are available', () => {
    updateAttachButton();

    expect(dom.attachBtn?.style.display).toBe('');
    expect(dom.attachLocalBtn?.hidden).toBe(true);

    state.imageCapable = true;
    updateAttachButton();
    expect(dom.attachLocalBtn?.hidden).toBe(true);

    state.s3Capable = true;
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
});
