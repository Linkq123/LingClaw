import { dom, state } from '../state.js';
import { escHtml, truncateStr, formatToolDuration, hideWelcome } from '../utils.js';
import { scrollDown, syncToolDrawerBounds } from '../scroll.js';
import { animatePanelIn, animateCollapsibleSection } from './timeline.js';
import {
  mountExecutionPanel,
  refreshExecutionStackForPanel,
  resumeExecutionStackAutoCollapse,
} from './execution-stack.js';
import { pinReactStatusToBottom } from './react-status.js';
import { tr } from '../i18n.js';
import { trapDialogFocus } from '../pages/dialogFocus.js';
import { iconMarkup } from '../icons.js';
import type { ImageAttachment } from '../types.js';

const TOOL_LIVE_OUTPUT_MAX_CHARS = 60000;
const TOOL_LIVE_OUTPUT_TRUNCATED_PREFIX = '[live output truncated]\n';
let lastToolDrawerFocus: HTMLElement | null = null;
let toolDrawerFocusRaf = 0;
let toolDrawerModal = false;

export function claimToolImageCompatibilityWarning(): boolean {
  if (state.toolImageCompatibilityWarningShown) return false;
  state.toolImageCompatibilityWarningShown = true;
  return true;
}

export function resetToolImageCompatibilityWarning(): void {
  state.toolImageCompatibilityWarningShown = false;
}

export function normalizeToolImages(images: unknown): ImageAttachment[] {
  if (!Array.isArray(images)) return [];
  return images
    .filter((image): image is ImageAttachment =>
      Boolean(
        image &&
        typeof image === 'object' &&
        typeof image.url === 'string' &&
        isSafeToolImageUrl(image.url),
      ),
    )
    .map((image, index) => ({
      url: image.url,
      name: typeof image.name === 'string' && image.name.trim() ? image.name : `image-${index + 1}`,
      mime_type: typeof image.mime_type === 'string' ? image.mime_type : undefined,
    }));
}

function isSafeToolImageUrl(value: string): boolean {
  try {
    const protocol = new URL(value).protocol.toLowerCase();
    return protocol === 'https:' || protocol === 'http:';
  } catch {
    return false;
  }
}

function panelToolImages(panel: HTMLElement): ImageAttachment[] {
  try {
    return normalizeToolImages(JSON.parse(panel.dataset.toolImages || '[]'));
  } catch {
    return [];
  }
}

function toolImageCountText(count: number): string {
  return tr(count === 1 ? 'tool.imageCountOne' : 'tool.imageCount', { count });
}

function syncPanelImageCount(panel: HTMLElement, images: ImageAttachment[]): void {
  panel.dataset.toolImages = JSON.stringify(images);
  panel.dataset.toolImageCount = String(images.length);
  const count = panel.querySelector<HTMLElement>('.tool-image-count');
  if (!count) return;
  count.hidden = images.length === 0;
  count.textContent = images.length ? toolImageCountText(images.length) : '';
}

function renderToolImageGallery(host: HTMLElement | null, images: ImageAttachment[]): void {
  if (!host) return;
  host.replaceChildren();
  for (const image of images) {
    const name = image.name || 'image';
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'tool-image-preview';
    button.dataset.action = 'preview-tool-image';
    button.dataset.imageUrl = image.url;
    button.setAttribute('aria-label', tr('tool.previewImage', { name }));

    const thumbnail = document.createElement('img');
    thumbnail.src = image.url;
    thumbnail.alt = name;
    thumbnail.loading = 'lazy';
    thumbnail.decoding = 'async';
    thumbnail.setAttribute('loading', 'lazy');
    thumbnail.setAttribute('decoding', 'async');
    const label = document.createElement('span');
    label.className = 'tool-image-name';
    label.textContent = name;
    const error = document.createElement('span');
    error.className = 'tool-image-error';
    error.textContent = tr('tool.imageLoadFailed');
    error.hidden = true;
    thumbnail.addEventListener('error', () => {
      button.classList.add('is-error');
      thumbnail.hidden = true;
      error.hidden = false;
    });
    button.append(thumbnail, error, label);
    host.appendChild(button);
  }
}

export function previewToolImage(button: HTMLElement): void {
  const url = button.dataset.imageUrl;
  if (!url || !isSafeToolImageUrl(url)) return;
  window.open(url, '_blank', 'noopener,noreferrer');
}

function isToolDrawerModalViewport(): boolean {
  if (typeof window.matchMedia === 'function') {
    return window.matchMedia('(max-width: 1279px)').matches;
  }
  return window.innerWidth < 1280;
}

function cancelToolDrawerFocus(): void {
  if (!toolDrawerFocusRaf) return;
  cancelAnimationFrame(toolDrawerFocusRaf);
  toolDrawerFocusRaf = 0;
}

function setToolDrawerBackgroundInert(modal: boolean): void {
  document.body.classList.toggle('tool-drawer-modal-open', modal);
  const subagentModalOpen = document.body.classList.contains('subagent-modal-visible');
  const mobileNavigationViewport =
    typeof window.matchMedia === 'function'
      ? window.matchMedia('(max-width: 768px)').matches
      : window.innerWidth <= 768;
  const mobileNavigationOpen = mobileNavigationViewport && state.mobileNavigationOpen;
  if (dom.sessionDrawer) {
    dom.sessionDrawer.inert =
      modal || subagentModalOpen || (mobileNavigationViewport && !mobileNavigationOpen);
  }
  const conversation = document.querySelector<HTMLElement>('.conversation-column');
  if (conversation) conversation.inert = modal || subagentModalOpen || mobileNavigationOpen;
}

export function syncToolDrawerResponsiveState(): void {
  const drawer = dom.toolDrawer;
  if (!drawer) return;
  const open = drawer.classList.contains('open');
  const modal =
    open &&
    (isToolDrawerModalViewport() || document.body.classList.contains('subagent-modal-visible'));
  const becameModal = modal && !toolDrawerModal;
  toolDrawerModal = modal;
  if (modal) drawer.setAttribute('aria-modal', 'true');
  else drawer.removeAttribute('aria-modal');
  setToolDrawerBackgroundInert(modal);
  if (becameModal && !drawer.contains(document.activeElement)) {
    drawer.querySelector<HTMLButtonElement>('.tool-drawer-close')?.focus();
  }
}

export function trapToolDrawerFocus(event: KeyboardEvent): boolean {
  if (!toolDrawerModal || !dom.toolDrawer?.classList.contains('open')) return false;
  return trapDialogFocus(event, dom.toolDrawer);
}

export function mergeToolLiveOutput(current, stream, chunk, maxChars = TOOL_LIVE_OUTPUT_MAX_CHARS) {
  const prefix = stream === 'stderr' ? '\n[stderr]\n' : '';
  let next = `${current || ''}${prefix}${chunk || ''}`;
  if (next.length <= maxChars) return next;
  next = next.slice(next.length - maxChars);
  return `${TOOL_LIVE_OUTPUT_TRUNCATED_PREFIX}${next}`;
}

function findToolPanel(id) {
  const panels = Array.from(dom.chat.querySelectorAll('.tool-panel'));
  let fallback = null;

  for (let idx = panels.length - 1; idx >= 0; idx -= 1) {
    const panel = panels[idx];
    if (id && panel.dataset.toolId !== id) {
      continue;
    }
    if (!fallback) {
      fallback = panel;
    }
    if (panel.dataset.toolHasResult !== 'true') {
      return panel;
    }
  }

  return fallback;
}

export function addToolCall(name, args, id) {
  let argsDisplay = args;
  try {
    argsDisplay = JSON.stringify(JSON.parse(args), null, 2);
  } catch {}

  const existing = findToolPanel(id);
  if (existing && existing.dataset.toolHasResult !== 'true') {
    existing.dataset.toolId = id;
    existing.dataset.toolName = name;
    existing.dataset.toolArgs = argsDisplay;
    const nameEl = existing.querySelector('.tool-name');
    if (nameEl) nameEl.textContent = name;
    const argsPreviewEl = existing.querySelector('.tool-args-preview');
    if (argsPreviewEl) argsPreviewEl.textContent = truncateStr(args, 80);
    return existing;
  }

  const panel = document.createElement('div');
  panel.className = 'tool-panel';
  panel.dataset.toolId = id;

  panel.dataset.toolName = name;
  panel.dataset.toolArgs = argsDisplay;
  panel.dataset.toolResult = '';
  panel.dataset.toolLiveOutput = '';
  panel.dataset.toolHasResult = 'false';
  panel.dataset.toolImages = '[]';
  panel.dataset.toolImageCount = '0';
  panel.dataset.toolStatus = tr('tool.running');

  panel.innerHTML = `
    <button type="button" class="tool-header" data-action="open-tool-drawer" aria-haspopup="dialog">
      <span class="tool-icon">${iconMarkup('bolt')}</span>
      <span class="tool-name">${escHtml(name)}</span>
      <span class="tool-args-preview">${escHtml(truncateStr(args, 80))}</span>
      <span class="tool-image-count" hidden></span>
      <span class="tool-status">${escHtml(tr('tool.running'))}</span>
    </button>
  `;
  const currentRow = state.currentMsg ? state.currentMsg.closest('.msg-row') : null;
  mountExecutionPanel(panel, 'tool', currentRow);
  pinReactStatusToBottom();
  animatePanelIn(panel);
  hideWelcome();
  scrollDown();
  return panel;
}

export function updateToolProgress(id, elapsedMs) {
  const panel = findToolPanel(id);
  if (!panel || panel.dataset.toolHasResult === 'true') return;
  const seconds = Math.max(1, Math.floor((elapsedMs || 0) / 1000));
  panel.dataset.toolElapsedSeconds = String(seconds);
  const statusText = tr('tool.runningWithSeconds', { seconds });
  panel.dataset.toolStatus = statusText;
  const statusEl = panel.querySelector('.tool-status');
  if (statusEl) {
    statusEl.textContent = statusText;
  }
  if (state.activeToolPanel === panel) {
    syncToolDrawer(panel);
  }
  refreshExecutionStackForPanel(panel);
}

export function appendToolOutput(id, stream, chunk) {
  const panel = findToolPanel(id);
  if (!panel || panel.dataset.toolHasResult === 'true' || !chunk) return;
  panel.dataset.toolLiveOutput = mergeToolLiveOutput(
    panel.dataset.toolLiveOutput || '',
    stream,
    chunk,
  );
  if (state.activeToolPanel === panel) {
    syncToolDrawer(panel);
  }
}

export function addToolResult(
  name,
  result,
  id,
  durationMs = null,
  isError = false,
  images: ImageAttachment[] = [],
) {
  const normalizedImages = normalizeToolImages(images);
  const panel = findToolPanel(id);
  if (panel) {
    panel.dataset.toolResult = result;
    panel.dataset.toolLiveOutput = '';
    panel.dataset.toolHasResult = 'true';
    panel.dataset.toolDurationMs = durationMs == null ? '' : String(durationMs);
    panel.dataset.toolIsError = String(isError);
    syncPanelImageCount(panel, normalizedImages);
    const durationLabel = formatToolDuration(durationMs);
    panel.dataset.toolStatus = isError
      ? durationLabel
        ? tr('tool.failedWithDuration', { duration: durationLabel })
        : tr('tool.failed')
      : durationLabel
        ? tr('tool.resultReturnedWithDuration', { duration: durationLabel })
        : tr('tool.resultReturned');
    const statusEl = panel.querySelector('.tool-status');
    if (statusEl) {
      statusEl.textContent = panel.dataset.toolStatus;
    }
    panel.classList.add('tool-panel-ready');
    panel.classList.toggle('tool-panel-failed', isError);
    if (state.activeToolPanel === panel) {
      syncToolDrawer(panel);
    }
    refreshExecutionStackForPanel(panel);
    return;
  }
  // Fallback: standalone result
  const el = document.createElement('div');
  el.className = 'tool-panel tool-result';
  el.dataset.toolId = id || '';
  el.dataset.toolName = name ? `${name} result` : 'Tool result';
  el.dataset.toolArgs = '';
  el.dataset.toolResult = result;
  el.dataset.toolHasResult = 'true';
  el.dataset.toolDurationMs = durationMs == null ? '' : String(durationMs);
  el.dataset.toolIsError = String(isError);
  el.dataset.toolImages = '[]';
  el.dataset.toolImageCount = '0';
  const durationLabel = formatToolDuration(durationMs);
  el.dataset.toolStatus = isError
    ? durationLabel
      ? tr('tool.failedWithDuration', { duration: durationLabel })
      : tr('tool.failed')
    : durationLabel
      ? tr('tool.resultReturnedWithDuration', { duration: durationLabel })
      : tr('tool.resultReturned');
  el.innerHTML = `
    <button type="button" class="tool-header" data-action="open-tool-drawer" aria-haspopup="dialog">
      <span class="tool-icon">${iconMarkup('clipboard')}</span>
      <span class="tool-name">${escHtml(name)} result</span>
      <span class="tool-image-count" hidden></span>
      <span class="tool-status">${escHtml(el.dataset.toolStatus)}</span>
    </button>
  `;
  el.classList.add('tool-panel-ready');
  syncPanelImageCount(el, normalizedImages);
  el.classList.toggle('tool-panel-failed', isError);
  const currentRow = state.currentMsg ? state.currentMsg.closest('.msg-row') : null;
  mountExecutionPanel(el, 'result', currentRow);
  pinReactStatusToBottom();
  animatePanelIn(el);
  scrollDown();
}

export function syncToolDrawer(panel) {
  if (!panel || !dom.toolDrawer) return;
  const toolName = panel.dataset.toolName || 'Tool';
  const toolArgs = panel.dataset.toolArgs || '';
  const toolResult = panel.dataset.toolResult || '';
  const toolLiveOutput = panel.dataset.toolLiveOutput || '';
  const hasResult = panel.dataset.toolHasResult === 'true';
  const detailText = hasResult ? toolResult : toolLiveOutput;
  const hasDetail = detailText.trim().length > 0;
  const images = panelToolImages(panel);
  const statusText =
    panel.dataset.toolStatus || (hasResult ? tr('tool.resultReturned') : tr('tool.running'));

  if (dom.toolDrawerTitle) dom.toolDrawerTitle.textContent = toolName;
  if (dom.toolDrawerMeta) dom.toolDrawerMeta.textContent = statusText;
  if (dom.toolDrawerArgs) dom.toolDrawerArgs.textContent = toolArgs || tr('tool.argumentsEmpty');
  if (dom.toolDrawerResult) dom.toolDrawerResult.textContent = detailText;
  if (dom.toolDrawerResultSection) dom.toolDrawerResultSection.hidden = !hasDetail;
  renderToolImageGallery(dom.toolDrawerImages, images);
  if (dom.toolDrawerImagesSection) dom.toolDrawerImagesSection.hidden = images.length === 0;
}

export function openToolDrawer(panel, trigger: HTMLElement | null = null) {
  if (!panel || !dom.toolDrawer || !dom.toolDrawerBackdrop) return;
  cancelToolDrawerFocus();
  lastToolDrawerFocus =
    trigger || (document.activeElement instanceof HTMLElement ? document.activeElement : null);
  syncToolDrawerBounds();
  if (state.activeToolPanel && state.activeToolPanel !== panel) {
    state.activeToolPanel.classList.remove('tool-panel-active');
  }
  state.activeToolPanel = panel;
  state.activeToolPanel.classList.add('tool-panel-active');
  syncToolDrawer(panel);
  dom.toolDrawer.classList.add('open');
  dom.toolDrawerBackdrop.classList.add('open');
  dom.toolDrawer.setAttribute('aria-hidden', 'false');
  dom.toolDrawer.setAttribute('role', 'dialog');
  syncToolDrawerResponsiveState();
  if (trigger) {
    toolDrawerFocusRaf = requestAnimationFrame(() => {
      toolDrawerFocusRaf = 0;
      if (!dom.toolDrawer?.classList.contains('open')) return;
      dom.toolDrawer.querySelector<HTMLButtonElement>('.tool-drawer-close')?.focus();
    });
  }
}

export function openToolDrawerFromHeader(header) {
  openToolDrawer(header.closest('.tool-panel'), header);
}

export function closeToolDrawer() {
  cancelToolDrawerFocus();
  if (!dom.toolDrawer || !dom.toolDrawerBackdrop) return;
  const previousFocus = lastToolDrawerFocus;
  const shouldRestoreFocus = dom.toolDrawer.contains(document.activeElement);
  lastToolDrawerFocus = null;
  dom.toolDrawer.classList.remove('open');
  dom.toolDrawerBackdrop.classList.remove('open');
  dom.toolDrawer.setAttribute('aria-hidden', 'true');
  syncToolDrawerResponsiveState();
  const activePanel = state.activeToolPanel;
  if (activePanel) {
    activePanel.classList.remove('tool-panel-active');
    state.activeToolPanel = null;
  }
  const collapsedStackHeader = resumeExecutionStackAutoCollapse(activePanel);
  if (shouldRestoreFocus) {
    if (collapsedStackHeader) collapsedStackHeader.focus();
    else if (previousFocus?.isConnected) previousFocus.focus();
    else dom.input?.focus();
  }
}

export function toggleTool(header) {
  const chevron = header.querySelector('.chevron');
  const body = header.nextElementSibling;
  const nextOpen = !body.classList.contains('show');
  if (chevron) chevron.classList.toggle('open', nextOpen);
  header.setAttribute?.('aria-expanded', String(nextOpen));
  animateCollapsibleSection(body, nextOpen);
}

export function refreshToolPanelsLanguage(): void {
  document
    .querySelectorAll<HTMLElement>('.tool-panel:not([data-task-plan-panel])')
    .forEach((panel) => {
      const hasResult = panel.dataset.toolHasResult === 'true';
      const isError = panel.dataset.toolIsError === 'true';
      const durationMs = panel.dataset.toolDurationMs ? Number(panel.dataset.toolDurationMs) : null;
      const duration = formatToolDuration(durationMs);
      const seconds = Number(panel.dataset.toolElapsedSeconds || 0);
      const status = hasResult
        ? isError
          ? duration
            ? tr('tool.failedWithDuration', { duration })
            : tr('tool.failed')
          : duration
            ? tr('tool.resultReturnedWithDuration', { duration })
            : tr('tool.resultReturned')
        : seconds > 0
          ? tr('tool.runningWithSeconds', { seconds })
          : tr('tool.running');
      panel.dataset.toolStatus = status;
      const statusEl = panel.querySelector<HTMLElement>('.tool-status');
      if (statusEl) statusEl.textContent = status;
      const imageCount = Number(panel.dataset.toolImageCount || 0);
      const imageCountEl = panel.querySelector<HTMLElement>('.tool-image-count');
      if (imageCountEl) {
        imageCountEl.hidden = imageCount === 0;
        imageCountEl.textContent = imageCount ? toolImageCountText(imageCount) : '';
      }
    });
  if (state.activeToolPanel) syncToolDrawer(state.activeToolPanel);
}
