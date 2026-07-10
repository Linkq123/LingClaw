import { beforeEach, describe, expect, it, vi } from 'vitest';

type StateModule = typeof import('../src/state.js');
type MobileModule = typeof import('../src/mobile.js');

describe('workspace shell', () => {
  let stateModule: StateModule;
  let mobileModule: MobileModule;
  let mobileViewport = true;

  beforeEach(async () => {
    vi.resetModules();
    mobileViewport = true;
    vi.stubGlobal(
      'matchMedia',
      vi.fn(() => ({
        get matches() {
          return mobileViewport;
        },
        media: '(max-width: 768px)',
        onchange: null,
        addEventListener: vi.fn(),
        removeEventListener: vi.fn(),
        addListener: vi.fn(),
        removeListener: vi.fn(),
        dispatchEvent: vi.fn(),
      })),
    );

    document.body.innerHTML = `
      <button id="mobile-navigation-toggle" aria-expanded="false"></button>
      <button id="mobile-navigation-backdrop" hidden></button>
      <aside id="session-drawer">
        <button id="session-drawer-toggle-btn">Close</button>
        <button id="drawer-first">First navigation item</button>
      </aside>
      <main class="conversation-column"><button id="background-control">Background</button></main>
      <button id="view-controls-toggle" aria-expanded="false"></button>
      <div id="view-controls-menu" hidden></div>
      <button id="mobile-menu-toggle" aria-expanded="false"></button>
      <div id="mobile-menu" hidden></div>
    `;

    stateModule = await import('../src/state.js');
    mobileModule = await import('../src/mobile.js');
    stateModule.initDomRefs();
    stateModule.state.mobileNavigationOpen = false;
    stateModule.state.activeShellPopover = null;
  });

  it('opens mobile navigation without changing the persisted desktop drawer state', () => {
    stateModule.state.sessionDrawerExpanded = false;
    const trigger = document.getElementById('mobile-navigation-toggle') as HTMLButtonElement;

    mobileModule.openMobileNavigation(trigger);

    expect(stateModule.state.mobileNavigationOpen).toBe(true);
    expect(stateModule.state.sessionDrawerExpanded).toBe(false);
    expect(stateModule.dom.sessionDrawer?.classList.contains('is-mobile-open')).toBe(true);
    expect(trigger.getAttribute('aria-expanded')).toBe('true');
    expect((document.getElementById('mobile-navigation-backdrop') as HTMLElement).hidden).toBe(
      false,
    );
    expect((stateModule.dom.sessionDrawer as HTMLElement).inert).toBe(false);
    expect((document.querySelector('.conversation-column') as HTMLElement).inert).toBe(true);
    expect(document.activeElement?.id).toBe('session-drawer-toggle-btn');
    expect(stateModule.dom.sessionDrawerToggleBtn?.getAttribute('aria-label')).toBe(
      'Close workspace navigation',
    );
  });

  it('closes mobile navigation and restores focus to its trigger', () => {
    const trigger = document.getElementById('mobile-navigation-toggle') as HTMLButtonElement;
    mobileModule.openMobileNavigation(trigger);

    mobileModule.closeMobileNavigation({ restoreFocus: true });

    expect(stateModule.state.mobileNavigationOpen).toBe(false);
    expect(stateModule.dom.sessionDrawer?.classList.contains('is-mobile-open')).toBe(false);
    expect(trigger.getAttribute('aria-expanded')).toBe('false');
    expect(document.activeElement).toBe(trigger);
    expect((stateModule.dom.sessionDrawer as HTMLElement).inert).toBe(true);
    expect((document.querySelector('.conversation-column') as HTMLElement).inert).toBe(false);
  });

  it('keeps only one workspace popover open and synchronizes aria state', () => {
    mobileModule.toggleShellPopover('view-controls');

    expect(stateModule.state.activeShellPopover).toBe('view-controls');
    expect((document.getElementById('view-controls-menu') as HTMLElement).hidden).toBe(false);
    expect(document.getElementById('view-controls-toggle')?.getAttribute('aria-expanded')).toBe(
      'true',
    );

    mobileModule.toggleShellPopover('commands');

    expect(stateModule.state.activeShellPopover).toBe('commands');
    expect((document.getElementById('view-controls-menu') as HTMLElement).hidden).toBe(true);
    expect((document.getElementById('mobile-menu') as HTMLElement).hidden).toBe(false);
    expect(document.getElementById('mobile-menu-toggle')?.getAttribute('aria-expanded')).toBe(
      'true',
    );
  });

  it('drops transient mobile navigation state when crossing to desktop', () => {
    mobileModule.openMobileNavigation();
    mobileViewport = false;

    mobileModule.syncResponsiveNavigation();

    expect(stateModule.state.mobileNavigationOpen).toBe(false);
    expect(stateModule.dom.sessionDrawer?.classList.contains('is-mobile-open')).toBe(false);
  });

  it('moves focus to the mobile navigation trigger when the desktop drawer becomes hidden', () => {
    mobileViewport = false;
    mobileModule.syncResponsiveNavigation();
    const drawerControl = document.getElementById('drawer-first') as HTMLButtonElement;
    const trigger = document.getElementById('mobile-navigation-toggle') as HTMLButtonElement;
    drawerControl.focus();

    mobileViewport = true;
    mobileModule.syncResponsiveNavigation();

    expect(stateModule.dom.sessionDrawer?.inert).toBe(true);
    expect(document.activeElement).toBe(trigger);
  });
});
