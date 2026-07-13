import { dom, state } from './state.js';
import { trapDialogFocus } from './pages/dialogFocus.js';
import { closeSessionRowMenu, syncSessionDrawerToggleChrome } from './renderers/sessions.js';

export type ShellPopover = 'view-controls' | 'commands' | null;

const MOBILE_BREAKPOINT = 768;
let lastNavigationTrigger: HTMLElement | null = null;
let lastPopoverTrigger: HTMLElement | null = null;

function mobileMediaQuery(): MediaQueryList | null {
  if (typeof window.matchMedia !== 'function') return null;
  return window.matchMedia(`(max-width: ${MOBILE_BREAKPOINT}px)`);
}

export function isMobileViewport(): boolean {
  const query = mobileMediaQuery();
  return query ? query.matches : window.innerWidth <= MOBILE_BREAKPOINT;
}

function popoverParts(kind: Exclude<ShellPopover, null>): {
  toggle: HTMLButtonElement | null;
  menu: HTMLElement | null;
} {
  const isView = kind === 'view-controls';
  return {
    toggle: document.getElementById(
      isView ? 'view-controls-toggle' : 'mobile-menu-toggle',
    ) as HTMLButtonElement | null,
    menu: document.getElementById(isView ? 'view-controls-menu' : 'mobile-menu'),
  };
}

function syncPopover(kind: Exclude<ShellPopover, null>, open: boolean): void {
  const { toggle, menu } = popoverParts(kind);
  if (menu) menu.hidden = !open;
  if (toggle) toggle.setAttribute('aria-expanded', String(open));
}

export function closeShellPopovers({ restoreFocus = false } = {}): void {
  const previousTrigger = lastPopoverTrigger;
  syncPopover('view-controls', false);
  syncPopover('commands', false);
  state.activeShellPopover = null;
  lastPopoverTrigger = null;
  if (restoreFocus && previousTrigger?.isConnected) previousTrigger.focus();
}

export function toggleShellPopover(kind: Exclude<ShellPopover, null>): void {
  const { toggle } = popoverParts(kind);
  const willOpen = state.activeShellPopover !== kind;
  closeShellPopovers();
  if (!willOpen) return;
  state.activeShellPopover = kind;
  lastPopoverTrigger = toggle;
  syncPopover(kind, true);
}

export function syncMobileMenuAria(open: boolean): void {
  const toggle = document.getElementById('mobile-menu-toggle');
  if (toggle) toggle.setAttribute('aria-expanded', String(open));
}

export function toggleMobileMenu(): void {
  toggleShellPopover('commands');
}

export function closeMobileMenu(): void {
  if (state.activeShellPopover === 'commands') {
    closeShellPopovers();
    return;
  }
  syncPopover('commands', false);
}

export function toggleViewControlsMenu(): void {
  toggleShellPopover('view-controls');
}

function syncMobileNavigation(): void {
  const mobile = isMobileViewport();
  const open = state.mobileNavigationOpen && mobile;
  const toolDrawerModalOpen = document.body.classList.contains('tool-drawer-modal-open');
  const drawer = dom.sessionDrawer;
  drawer?.classList.toggle('is-mobile-open', open);
  if (drawer) {
    drawer.inert = toolDrawerModalOpen || (mobile && !open);
    if (mobile) {
      drawer.setAttribute('role', 'dialog');
      drawer.setAttribute('aria-modal', 'true');
      drawer.setAttribute('aria-hidden', String(!open));
    } else {
      drawer.removeAttribute('role');
      drawer.removeAttribute('aria-modal');
      drawer.removeAttribute('aria-hidden');
    }
  }
  const conversation = document.querySelector<HTMLElement>('.conversation-column');
  if (conversation) conversation.inert = open || toolDrawerModalOpen;
  if (dom.toolDrawer) dom.toolDrawer.inert = open;
  const toggle = document.getElementById('mobile-navigation-toggle');
  toggle?.setAttribute('aria-expanded', String(open));
  syncSessionDrawerToggleChrome();
  const backdrop = document.getElementById('mobile-navigation-backdrop');
  if (backdrop) backdrop.hidden = !open;
  document.body.classList.toggle('mobile-navigation-open', open);
}

export function openMobileNavigation(trigger?: HTMLElement | null): void {
  if (!isMobileViewport()) return;
  closeShellPopovers();
  state.mobileNavigationOpen = true;
  lastNavigationTrigger =
    trigger || (document.getElementById('mobile-navigation-toggle') as HTMLElement | null);
  syncMobileNavigation();
  dom.sessionDrawer
    ?.querySelector<HTMLElement>(
      '#session-drawer-toggle-btn, button:not([disabled]), a[href], [tabindex]:not([tabindex="-1"])',
    )
    ?.focus();
}

export function closeMobileNavigation({ restoreFocus = false } = {}): void {
  const previousTrigger = lastNavigationTrigger;
  closeSessionRowMenu();
  state.mobileNavigationOpen = false;
  lastNavigationTrigger = null;
  syncMobileNavigation();
  if (restoreFocus && previousTrigger?.isConnected) previousTrigger.focus();
}

export function createMobileNavigationSelectionHandler(
  isCurrent: (selectionId: string) => boolean,
  navigate: (selectionId: string) => void,
): (selectionId: string) => void {
  return (selectionId) => {
    const normalizedSelectionId = String(selectionId || '').trim();
    if (!normalizedSelectionId) return;
    closeMobileNavigation({ restoreFocus: true });
    if (isCurrent(normalizedSelectionId)) return;
    navigate(normalizedSelectionId);
  };
}

export function createCommandMenuActionHandler(
  executeCommand: (command: string) => void,
): (element: Element) => void {
  return (element) => {
    const command = element instanceof HTMLElement ? element.dataset.cmd : undefined;
    if (command) executeCommand(command);
    closeShellPopovers({ restoreFocus: true });
  };
}

export function toggleMobileNavigation(trigger?: HTMLElement | null): void {
  if (state.mobileNavigationOpen) {
    closeMobileNavigation({ restoreFocus: true });
  } else {
    openMobileNavigation(trigger);
  }
}

export function syncResponsiveNavigation(): void {
  if (!isMobileViewport()) {
    closeMobileNavigation();
  } else {
    const drawerHadFocus = dom.sessionDrawer?.contains(document.activeElement) === true;
    syncMobileNavigation();
    if (!state.mobileNavigationOpen && drawerHadFocus) {
      document.getElementById('mobile-navigation-toggle')?.focus();
    }
  }
}

// Guard: prevent double-registration on Vite HMR re-execution of main.ts.
let listenerInit = false;

export function initMobileListeners(): void {
  if (listenerInit) return;
  listenerInit = true;

  document.addEventListener('click', (event) => {
    const target = event.target;
    if (!(target instanceof Node) || !state.activeShellPopover) return;
    const { toggle, menu } = popoverParts(state.activeShellPopover);
    if (!toggle?.contains(target) && !menu?.contains(target)) closeShellPopovers();
  });

  document.addEventListener(
    'keydown',
    (event) => {
      if (
        !state.mobileNavigationOpen ||
        !isMobileViewport() ||
        document.querySelector('.action-dialog-overlay')
      ) {
        return;
      }
      if (event.key === 'Escape') {
        event.preventDefault();
        event.stopPropagation();
        closeMobileNavigation({ restoreFocus: true });
        return;
      }
      if (event.key === 'Tab' && trapDialogFocus(event, dom.sessionDrawer)) {
        event.stopPropagation();
      }
    },
    true,
  );

  const query = mobileMediaQuery();
  query?.addEventListener?.('change', syncResponsiveNavigation);
  syncResponsiveNavigation();
}
