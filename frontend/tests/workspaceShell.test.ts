import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { beforeEach, describe, expect, it, vi } from 'vitest';

type StateModule = typeof import('../src/state.js');
type MobileModule = typeof import('../src/mobile.js');
type SessionsRendererModule = typeof import('../src/renderers/sessions.js');

const indexHtml = readFileSync(resolve(process.cwd(), 'index.html'), 'utf8');
const layoutCss = readFileSync(resolve(process.cwd(), 'src/css/layout.css'), 'utf8');
const chatCss = readFileSync(resolve(process.cwd(), 'src/css/chat.css'), 'utf8');
const responsiveCss = readFileSync(resolve(process.cwd(), 'src/css/responsive.css'), 'utf8');
const workspaceCss = readFileSync(resolve(process.cwd(), 'src/css/workspace.css'), 'utf8');
const mainSource = readFileSync(resolve(process.cwd(), 'src/main.ts'), 'utf8');
const appCssPaths = Array.from(
  mainSource.matchAll(/^import ['"](.\/css\/[^'"]+\.css)['"];?$/gm),
  ([, path]) => path,
);
const appCss = appCssPaths
  .map((path) => readFileSync(resolve(process.cwd(), 'src', path), 'utf8'))
  .join('\n');

type MediaEnvironment = {
  width: number;
  colorScheme: 'light' | 'dark';
  reducedMotion: boolean;
  hover: 'hover' | 'none';
  pointer: 'fine' | 'coarse' | 'none';
};

function mediaFeatureMatches(
  feature: string,
  value: string,
  environment: MediaEnvironment,
): boolean {
  const normalizedFeature = feature.trim().toLowerCase();
  const normalizedValue = value.trim().toLowerCase();
  if (normalizedFeature === 'max-width') {
    const width = Number.parseFloat(normalizedValue);
    return normalizedValue.endsWith('px') && environment.width <= width;
  }
  if (normalizedFeature === 'min-width') {
    const width = Number.parseFloat(normalizedValue);
    return normalizedValue.endsWith('px') && environment.width >= width;
  }
  if (normalizedFeature === 'prefers-color-scheme') {
    return normalizedValue === environment.colorScheme;
  }
  if (normalizedFeature === 'prefers-reduced-motion') {
    return normalizedValue === (environment.reducedMotion ? 'reduce' : 'no-preference');
  }
  if (normalizedFeature === 'hover') return normalizedValue === environment.hover;
  if (normalizedFeature === 'pointer') return normalizedValue === environment.pointer;
  return false;
}

function mediaBranchMatches(branch: string, environment: MediaEnvironment): boolean {
  let normalizedBranch = branch.trim().toLowerCase();
  const negated = normalizedBranch.startsWith('not ');
  if (negated) normalizedBranch = normalizedBranch.slice(4).trim();
  normalizedBranch = normalizedBranch.replace(/^only\s+/, '');

  const mediaType = normalizedBranch.match(/^(screen|print|all)\b/)?.[1];
  const typeMatches = mediaType !== 'print';
  const features = Array.from(normalizedBranch.matchAll(/\(\s*([^:()]+?)\s*:\s*([^()]+?)\s*\)/g));
  const featuresMatch =
    features.length === 0
      ? mediaType != null
      : features.every(([, feature, value]) => mediaFeatureMatches(feature, value, environment));
  const matches = typeMatches && featuresMatch;
  return negated ? !matches : matches;
}

function mediaMatchesEnvironment(mediaText: string, environment: MediaEnvironment): boolean {
  return mediaText.split(',').some((branch) => mediaBranchMatches(branch, environment));
}

function cssForMediaEnvironment(css: string, environment: MediaEnvironment): string {
  const styleElement = document.createElement('style');
  styleElement.textContent = css;
  document.head.appendChild(styleElement);
  const applicableRules: string[] = [];

  function visit(rules: CSSRuleList): void {
    for (const rule of Array.from(rules)) {
      const mediaRule = rule as CSSMediaRule;
      if (mediaRule.media && 'cssRules' in mediaRule) {
        if (mediaMatchesEnvironment(mediaRule.media.mediaText, environment)) {
          visit(mediaRule.cssRules);
        }
        continue;
      }
      applicableRules.push(rule.cssText);
    }
  }

  if (styleElement.sheet) visit(styleElement.sheet.cssRules);
  styleElement.remove();
  return applicableRules.join('\n');
}

function isElementTreeVisible(element: HTMLElement): boolean {
  let current: HTMLElement | null = element;
  while (current) {
    const style = getComputedStyle(current);
    if (
      style.display === 'none' ||
      style.visibility === 'hidden' ||
      style.visibility === 'collapse' ||
      (style.opacity !== '' && Number(style.opacity) === 0)
    ) {
      return false;
    }
    current = current.parentElement;
  }
  return true;
}

describe('workspace shell', () => {
  let stateModule: StateModule;
  let mobileModule: MobileModule;
  let sessionsRendererModule: SessionsRendererModule;
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
        <button id="session-drawer-new-btn">New</button>
        <div id="session-drawer-list"></div>
      </aside>
      <main class="conversation-column"><button id="background-control">Background</button></main>
      <header class="workspace-header">
        <div class="workspace-status-cluster">
          <div class="conn-badge" id="conn-badge"></div>
        </div>
        <div class="actions workspace-actions">
          <button class="workspace-action-btn" id="view-controls-toggle" aria-expanded="false"></button>
          <div id="view-controls-menu" hidden></div>
          <button class="workspace-action-btn workspace-action-icon" id="mobile-menu-toggle" aria-expanded="false"></button>
          <div id="mobile-menu" hidden></div>
        </div>
      </header>
    `;

    stateModule = await import('../src/state.js');
    mobileModule = await import('../src/mobile.js');
    sessionsRendererModule = await import('../src/renderers/sessions.js');
    stateModule.initDomRefs();
    stateModule.state.mobileNavigationOpen = false;
    stateModule.state.activeShellPopover = null;
    stateModule.state.sessionSwitchInFlight = false;
    stateModule.state.sessionIdentityMutationInFlight = false;
    stateModule.state.composerSessionTransitionPending = false;
    stateModule.state.composerSessionIdentityPending = false;
    stateModule.state.imageUploadInFlight = false;
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

  it('keeps desktop focus and closes mobile navigation when the current session row is activated', () => {
    stateModule.state.sessions = [
      { id: 'main', name: 'Main' },
      { id: 'current-session', name: 'Current Session' },
    ];
    stateModule.state.activeSessionId = 'current-session';
    const navigate = vi.fn();
    const onSwitch = mobileModule.createMobileNavigationSelectionHandler(
      (sessionId) => sessionId === stateModule.state.activeSessionId,
      navigate,
    );
    sessionsRendererModule.initSessionDrawer({
      onCreate: vi.fn(),
      onDelete: vi.fn(),
      onSwitch,
    });

    mobileViewport = false;
    mobileModule.syncResponsiveNavigation();
    const desktopCurrentButton =
      stateModule.dom.sessionDrawerList?.querySelector<HTMLButtonElement>(
        '[data-session-id="current-session"] [data-session-action="switch"]',
      );
    desktopCurrentButton?.focus();
    desktopCurrentButton?.click();

    expect(document.activeElement).toBe(desktopCurrentButton);
    expect(navigate).not.toHaveBeenCalled();

    mobileViewport = true;
    const trigger = document.getElementById('mobile-navigation-toggle') as HTMLButtonElement;
    mobileModule.openMobileNavigation(trigger);
    const mobileCurrentButton = stateModule.dom.sessionDrawerList?.querySelector<HTMLButtonElement>(
      '[data-session-id="current-session"] [data-session-action="switch"]',
    );
    mobileCurrentButton?.focus();
    mobileCurrentButton?.click();

    expect(stateModule.state.mobileNavigationOpen).toBe(false);
    expect(document.activeElement).toBe(trigger);
    expect(navigate).not.toHaveBeenCalled();
  });

  it('ignores blank navigation selections without closing the mobile drawer', () => {
    const trigger = document.getElementById('mobile-navigation-toggle') as HTMLButtonElement;
    const navigate = vi.fn();
    const onSwitch = mobileModule.createMobileNavigationSelectionHandler(() => false, navigate);
    mobileModule.openMobileNavigation(trigger);

    onSwitch('   ');

    expect(stateModule.state.mobileNavigationOpen).toBe(true);
    expect(navigate).not.toHaveBeenCalled();
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

  it('restores focus to the command trigger after a menu action', () => {
    const trigger = document.getElementById('mobile-menu-toggle') as HTMLButtonElement;
    const menu = document.getElementById('mobile-menu') as HTMLElement;
    const action = document.createElement('button');
    menu.appendChild(action);

    mobileModule.toggleShellPopover('commands');
    action.focus();
    const executeCommand = vi.fn();
    action.dataset.action = 'cmd-close-menu';
    action.dataset.cmd = '/status';
    const handleCommandMenuAction = mobileModule.createCommandMenuActionHandler(executeCommand);
    handleCommandMenuAction(action);

    expect(menu.hidden).toBe(true);
    expect(document.activeElement).toBe(trigger);
    expect(executeCommand).toHaveBeenCalledWith('/status');
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

  it('keeps connection status accessible without relying only on color on mobile', () => {
    expect(indexHtml).toMatch(
      /id="conn-badge"[\s\S]*?role="status"[\s\S]*?aria-live="polite"[\s\S]*?aria-atomic="true"/,
    );
    expect(workspaceCss).toMatch(/\.conn-label \{[\s\S]*?clip: rect\(0, 0, 0, 0\);/);
    expect(workspaceCss).toContain('.conn-dot.connecting {');
    expect(workspaceCss).toContain('.conn-dot.disconnected {');
    expect(layoutCss).toMatch(
      /\.conn-dot\.connecting\s*\{[^}]*background:\s*var\(--accent-warn\);/,
    );
    expect(layoutCss).toMatch(/\.conn-dot\.connecting\s*\{[^}]*animation:\s*dotPulse/);
    expect(layoutCss).toMatch(/\.conn-dot\.disconnected\s*\{[^}]*animation:\s*dotPulse/);
    const statusCluster = document.querySelector('.workspace-status-cluster') as HTMLElement;
    const connectionBadge = document.getElementById('conn-badge') as HTMLElement;
    const styleElement = document.createElement('style');
    expect(appCssPaths).toContain('./css/responsive.css');
    styleElement.textContent = cssForMediaEnvironment(appCss, {
      width: 390,
      colorScheme: 'light',
      reducedMotion: false,
      hover: 'none',
      pointer: 'coarse',
    });
    document.head.appendChild(styleElement);
    expect(getComputedStyle(statusCluster).display).not.toBe('none');
    expect(getComputedStyle(connectionBadge).display).not.toBe('none');
    expect(isElementTreeVisible(statusCluster)).toBe(true);
    expect(isElementTreeVisible(connectionBadge)).toBe(true);
    styleElement.remove();
  });

  it('evaluates non-width media features and comma-separated alternatives', () => {
    const css = cssForMediaEnvironment(
      `
        @media (prefers-color-scheme: dark) { .dark-only { display: block; } }
        @media (max-width: 300px), (pointer: coarse) { .coarse-or-narrow { display: block; } }
        @media (min-width: 500px), (hover: hover) { .wide-or-hover { display: block; } }
      `,
      {
        width: 390,
        colorScheme: 'light',
        reducedMotion: false,
        hover: 'none',
        pointer: 'coarse',
      },
    );

    expect(css).not.toContain('.dark-only');
    expect(css).toContain('.coarse-or-narrow');
    expect(css).not.toContain('.wide-or-hover');
  });

  it('applies workspace button styles over legacy header rules', () => {
    const styleElement = document.createElement('style');
    styleElement.textContent = `${layoutCss}\n${workspaceCss}`;
    document.head.appendChild(styleElement);
    const button = document.getElementById('view-controls-toggle') as HTMLButtonElement;
    const computed = getComputedStyle(button);

    expect(computed.minHeight).toBe('38px');
    expect(computed.paddingTop).toBe('0px');
    expect(computed.paddingRight).toBe('11px');
    expect(computed.paddingBottom).toBe('0px');
    expect(computed.paddingLeft).toBe('11px');
    expect(computed.fontSize).toBe('11px');
    styleElement.remove();

    expect(workspaceCss).toMatch(
      /\.workspace-header \.shell-menu button \{[\s\S]*?min-height: 44px;/,
    );
  });

  it('uses accessible welcome contrast and collapsed sidebar inspector width', () => {
    expect(workspaceCss).toContain('--welcome-muted: #6c7184;');
    expect(workspaceCss).toMatch(/\.welcome-hint \{[\s\S]*?color: var\(--welcome-muted\);/);
    expect(workspaceCss).toMatch(
      /\.app-shell:has\(\.session-drawer\.is-collapsed\):has\(\.tool-drawer\.open\)[\s\S]*?var\(--workspace-sidebar-collapsed\)/,
    );
  });

  it('keeps short user messages content-sized instead of collapsing percentage widths', () => {
    expect(workspaceCss).toMatch(
      /\.msg-row\.user \{[\s\S]*?align-self: flex-end;[\s\S]*?width: fit-content;[\s\S]*?max-width: min\(72%, 720px\);[\s\S]*?margin-left: auto;/,
    );
    expect(chatCss).toMatch(
      /\.msg\.user \{[\s\S]*?width: fit-content;[\s\S]*?max-width: 100%;[\s\S]*?word-break: normal;/,
    );
  });

  it('does not restore decorative dark-mode gradients over the neutral chat styles', () => {
    expect(responsiveCss).not.toMatch(/:root\[data-theme='dark'\] \.msg\.user\s*\{/);
    expect(responsiveCss).not.toMatch(
      /:root\[data-theme='dark'\] \.msg\.assistant (?:blockquote|pre)\s*\{/,
    );
    expect(responsiveCss).not.toMatch(
      /:root\[data-theme='dark'\] \.msg\.assistant \.markdown-table-wrap\s*\{/,
    );
    expect(responsiveCss).not.toMatch(/:root\[data-theme='dark'\] #send\s*\{/);
  });

  it('keeps the chat edge clear and uses the SVG sprite for jump-to-latest', () => {
    expect(layoutCss).not.toMatch(/#chat\s*\{[^}]*mask-image:/);
    expect(layoutCss).not.toMatch(/#jump-to-latest::before/);
    expect(indexHtml).toMatch(
      /id="jump-to-latest"[\s\S]*?class="icon jump-to-latest-icon"[\s\S]*?href="#icon-chevron-down"/,
    );
  });

  it('keeps execution stacks visible and uses a single scroll container for long details', () => {
    expect(workspaceCss).toMatch(/\.execution-stack \{[\s\S]*?flex: 0 0 auto;/);
    expect(workspaceCss).toMatch(
      /\.execution-stack-body \{[\s\S]*?max-height: min\(62dvh, 680px\);[\s\S]*?overflow-y: auto;/,
    );
    expect(workspaceCss).toMatch(/\.execution-stack-body \{[\s\S]*?overscroll-behavior-y: auto;/);
    expect(workspaceCss).toMatch(
      /\.execution-step \.reasoning-body\.show \{[\s\S]*?max-height: none;[\s\S]*?overflow: visible;/,
    );
    expect(workspaceCss).toMatch(
      /\.execution-stack\.is-complete:not\(\.is-expanded\) \{[\s\S]*?border-color:[\s\S]*?background:/,
    );
  });

  it('keeps Group target controls reachable and isolates the closed member drawer', () => {
    expect(workspaceCss).toMatch(/\.group-target-picker \{[\s\S]*?bottom: calc\(100% \+ 49px\);/);
    expect(workspaceCss).toMatch(
      /@media \(max-width: 768px\)[\s\S]*?\.group-members-toggle,[\s\S]*?min-height: 44px;/,
    );
    expect(workspaceCss).toMatch(
      /@media \(max-width: 768px\)[\s\S]*?\.group-target-picker \{[\s\S]*?position: absolute;[\s\S]*?bottom: calc\(100% \+ 64px\);/,
    );
    expect(mainSource).toContain(
      '.group-target-selection, .group-target-mode[data-mode="selected"]',
    );
    expect(mainSource).toContain('.group-member-row-menu, .group-member-menu-trigger');
    expect(mainSource).toContain("drawer.toggleAttribute('inert', !state.groupMembersDrawerOpen);");
    expect(mainSource).not.toMatch(/window\.(?:prompt|confirm)\s*\(/);
  });

  it('exposes composer suggestions through an accessible combobox contract', () => {
    expect(indexHtml).toMatch(
      /id="composer-availability-status"[^>]*class="composer-availability-status"[^>]*hidden/,
    );
    expect(indexHtml).toMatch(
      /<textarea[\s\S]*?id="input"[\s\S]*?role="combobox"[\s\S]*?aria-autocomplete="list"[\s\S]*?aria-controls="slash-command-menu"[\s\S]*?aria-expanded="false"[\s\S]*?aria-haspopup="listbox"/,
    );
  });

  it('lets a top-level action dialog own the keyboard above mobile navigation', () => {
    const trigger = document.getElementById('mobile-navigation-toggle') as HTMLButtonElement;
    mobileModule.initMobileListeners();
    mobileModule.openMobileNavigation(trigger);
    const overlay = document.createElement('div');
    overlay.className = 'action-dialog-overlay';
    const input = document.createElement('input');
    overlay.appendChild(input);
    document.body.appendChild(overlay);
    const actionDialogKeydown = vi.fn();
    overlay.addEventListener('keydown', actionDialogKeydown);

    input.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }),
    );

    expect(actionDialogKeydown).toHaveBeenCalledOnce();
    expect(stateModule.state.mobileNavigationOpen).toBe(true);
    overlay.remove();
    mobileModule.closeMobileNavigation();
  });
});
