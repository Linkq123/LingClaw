import { describe, expect, it, vi } from 'vitest';

import { closeOverlayById } from '../src/pages/overlay.js';

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
