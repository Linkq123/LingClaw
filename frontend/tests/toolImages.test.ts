import { beforeEach, describe, expect, it, vi } from 'vitest';

import { setLanguage } from '../src/i18n.js';
import { dom, state } from '../src/state.js';
import {
  addToolCall,
  addToolResult,
  claimToolImageCompatibilityWarning,
  openToolDrawerFromHeader,
  previewToolImage,
  refreshToolPanelsLanguage,
  resetToolImageCompatibilityWarning,
} from '../src/renderers/tools.js';

describe('tool image results', () => {
  beforeEach(() => {
    document.body.innerHTML = `
      <main class="conversation-column"><div id="chat"></div></main>
      <aside id="tool-drawer" aria-hidden="true">
        <button class="tool-drawer-close">Close</button>
        <h3 id="tool-drawer-title"></h3>
        <div id="tool-drawer-meta"></div>
        <pre id="tool-drawer-args"></pre>
        <section id="tool-drawer-result-section"><pre id="tool-drawer-result"></pre></section>
        <section id="tool-drawer-images-section" hidden><div id="tool-drawer-images"></div></section>
      </aside>
      <div id="tool-drawer-backdrop"></div>
    `;
    dom.chat = document.getElementById('chat');
    dom.toolDrawer = document.getElementById('tool-drawer');
    dom.toolDrawerBackdrop = document.getElementById('tool-drawer-backdrop');
    dom.toolDrawerTitle = document.getElementById('tool-drawer-title');
    dom.toolDrawerMeta = document.getElementById('tool-drawer-meta');
    dom.toolDrawerArgs = document.getElementById('tool-drawer-args');
    dom.toolDrawerResult = document.getElementById('tool-drawer-result');
    dom.toolDrawerResultSection = document.getElementById('tool-drawer-result-section');
    dom.toolDrawerImages = document.getElementById('tool-drawer-images');
    dom.toolDrawerImagesSection = document.getElementById('tool-drawer-images-section');
    state.activeExecutionStack = null;
    state.activeToolPanel = null;
    state.currentMsg = null;
    state.showTools = true;
    state.showReasoning = true;
    state.toolImageCompatibilityWarningShown = false;
    setLanguage('en');
  });

  it('renders lazy accessible thumbnails and includes their count in the execution summary', () => {
    const panel = addToolCall('view_image', '{"path":"chart.png"}', 'tool-image') as HTMLElement;
    addToolResult('view_image', 'Image read successfully.', 'tool-image', 42, false, [
      { url: 'https://example.test/chart.png', name: 'chart.png', mime_type: 'image/png' },
    ]);

    expect(panel.dataset.toolImageCount).toBe('1');
    expect(panel.querySelector('.tool-image-count')?.textContent).toBe('1 image');
    expect(
      panel.closest('.execution-stack')?.querySelector('.execution-stack-meta')?.textContent,
    ).toBe('1 step · 1 image');

    openToolDrawerFromHeader(panel.querySelector('.tool-header'));
    const image = dom.toolDrawerImages?.querySelector('img');
    const preview = dom.toolDrawerImages?.querySelector('button');
    expect(dom.toolDrawerImagesSection?.hidden).toBe(false);
    expect(image?.getAttribute('loading')).toBe('lazy');
    expect(image?.getAttribute('alt')).toBe('chart.png');
    expect(preview?.getAttribute('aria-label')).toBe('Preview chart.png');
  });

  it('shows a localized error state and refreshes an open gallery language', () => {
    const panel = addToolCall('capture', '{}', 'tool-error') as HTMLElement;
    addToolResult('capture', 'done', 'tool-error', 10, false, [
      { url: 'https://example.test/missing.png', name: 'missing.png', mime_type: 'image/png' },
    ]);
    openToolDrawerFromHeader(panel.querySelector('.tool-header'));
    dom.toolDrawerImages?.querySelector('img')?.dispatchEvent(new Event('error'));
    expect(dom.toolDrawerImages?.querySelector('.tool-image-error')?.textContent).toBe(
      'Image unavailable',
    );

    setLanguage('zh-CN');
    refreshToolPanelsLanguage();
    expect(panel.querySelector('.tool-image-count')?.textContent).toBe('1 张图片');
    expect(dom.toolDrawerImages?.querySelector('button')?.getAttribute('aria-label')).toBe(
      '预览 missing.png',
    );
  });

  it('opens a signed image URL only from an explicit preview action', () => {
    const open = vi.spyOn(window, 'open').mockImplementation(() => null);
    const button = document.createElement('button');
    button.dataset.imageUrl = 'https://example.test/signed.png?signature=ok';

    previewToolImage(button);

    expect(open).toHaveBeenCalledWith(
      'https://example.test/signed.png?signature=ok',
      '_blank',
      'noopener,noreferrer',
    );
    open.mockRestore();
  });

  it('rejects non-HTTP image URLs from tool events', () => {
    const open = vi.spyOn(window, 'open').mockImplementation(() => null);
    const panel = addToolCall('capture', '{}', 'unsafe-image') as HTMLElement;
    addToolResult('capture', 'done', 'unsafe-image', 10, false, [
      { url: 'javascript:alert(1)', name: 'unsafe.png', mime_type: 'image/png' },
    ]);

    expect(panel.dataset.toolImageCount).toBe('0');
    openToolDrawerFromHeader(panel.querySelector('.tool-header'));
    expect(dom.toolDrawerImages?.children).toHaveLength(0);
    expect(dom.toolDrawerImagesSection?.hidden).toBe(true);
    const forged = document.createElement('button');
    forged.dataset.imageUrl = 'javascript:alert(1)';
    previewToolImage(forged);
    expect(open).not.toHaveBeenCalled();
    open.mockRestore();
  });

  it('shows the compatibility warning at most once per run', () => {
    expect(claimToolImageCompatibilityWarning()).toBe(true);
    expect(claimToolImageCompatibilityWarning()).toBe(false);

    resetToolImageCompatibilityWarning();
    expect(claimToolImageCompatibilityWarning()).toBe(true);
  });
});
