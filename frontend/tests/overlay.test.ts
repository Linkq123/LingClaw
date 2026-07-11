import { describe, expect, it, vi } from 'vitest';

import { closeOverlayById, matchesOverlayDismissTarget } from '../src/pages/overlay.js';

describe('closeOverlayById', () => {
  it('routes settings overlay closes through the React bridge', () => {
    const closeSettingsPage = vi.fn();
    const closeUsagePage = vi.fn();

    const handled = closeOverlayById('settings-page', closeSettingsPage, closeUsagePage);

    expect(handled).toBe(true);
    expect(closeSettingsPage).toHaveBeenCalledTimes(1);
    expect(closeUsagePage).not.toHaveBeenCalled();
  });

  it('routes usage overlay closes through the React bridge', () => {
    const closeSettingsPage = vi.fn();
    const closeUsagePage = vi.fn();

    const handled = closeOverlayById('usage-page', closeSettingsPage, closeUsagePage);

    expect(handled).toBe(true);
    expect(closeSettingsPage).not.toHaveBeenCalled();
    expect(closeUsagePage).toHaveBeenCalledTimes(1);
  });

  it('returns false for unknown overlays', () => {
    const closeSettingsPage = vi.fn();
    const closeUsagePage = vi.fn();

    const handled = closeOverlayById('tool-drawer', closeSettingsPage, closeUsagePage);

    expect(handled).toBe(false);
    expect(closeSettingsPage).not.toHaveBeenCalled();
    expect(closeUsagePage).not.toHaveBeenCalled();
  });
});

describe('matchesOverlayDismissTarget', () => {
  it('matches an SVG descendant of the close control', () => {
    const overlay = document.createElement('div');
    overlay.innerHTML = `
      <button class="shortcuts-close">
        <svg><use href="#icon-close"></use></svg>
      </button>
    `;
    const iconUse = overlay.querySelector('use');

    expect(matchesOverlayDismissTarget(iconUse, overlay, '.shortcuts-close')).toBe(true);
    expect(matchesOverlayDismissTarget(overlay, overlay, '.shortcuts-close')).toBe(true);
  });

  it('ignores content outside the close control', () => {
    const overlay = document.createElement('div');
    const content = document.createElement('div');
    overlay.appendChild(content);

    expect(matchesOverlayDismissTarget(content, overlay, '.shortcuts-close')).toBe(false);
  });
});
