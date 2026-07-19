export type ConsoleSurface = 'workspace' | 'console';

export const CONSOLE_TRANSITION_CLASSES = {
  active: 'is-console-active',
  transitioning: 'is-console-transitioning',
  fallback: 'is-console-transition-fallback',
  entering: 'is-console-entering',
  leaving: 'is-console-leaving',
} as const;

interface ViewTransitionLike {
  finished?: Promise<unknown>;
  updateCallbackDone?: Promise<unknown>;
}

type StartConsoleViewTransition = (update: () => void | Promise<void>) => ViewTransitionLike;

type DocumentWithOptionalViewTransition = {
  startViewTransition?: StartConsoleViewTransition;
};

export interface ConsoleTransitionElements {
  workspace: HTMLElement;
  consolePage: HTMLElement;
  /** Body-level workspace overlays isolated together with the workspace surface. */
  workspacePortalRoot?: HTMLElement | null;
  classTarget?: HTMLElement;
  /** Workspace-owned scroll containers whose position must survive the full-screen swap. */
  scrollTargets?: Iterable<HTMLElement>;
  /** Runs immediately before the workspace becomes unavailable for layout reads. */
  onBeforeWorkspaceHide?: () => void;
  /** Runs after the workspace is measurable again and generic scroll positions are restored. */
  onAfterWorkspaceShow?: () => void;
}

export interface ConsoleTransitionControllerOptions {
  document?: Document;
  window?: Window;
  fallbackDurationMs?: number;
  initialSurface?: ConsoleSurface;
}

export interface ConsoleTransitionRequest {
  /** The page heading (or another programmatically focusable element) to focus after entering. */
  focusTarget?: HTMLElement | null;
  /** Override the captured workspace control when returning from the console. */
  restoreTarget?: HTMLElement | null;
}

export interface ConsoleTransitionController {
  readonly surface: ConsoleSurface;
  readonly desiredSurface: ConsoleSurface;
  transitionTo(surface: ConsoleSurface, request?: ConsoleTransitionRequest): Promise<boolean>;
  showConsole(request?: ConsoleTransitionRequest): Promise<boolean>;
  showWorkspace(request?: ConsoleTransitionRequest): Promise<boolean>;
  dispose(): void;
}

const REDUCED_MOTION_QUERY = '(prefers-reduced-motion: reduce)';
const DEFAULT_FALLBACK_DURATION_MS = 220;

interface WorkspaceScrollPosition {
  top: number;
  left: number;
}

function inferInitialSurface(elements: ConsoleTransitionElements): ConsoleSurface {
  const { workspace, consolePage } = elements;
  const workspaceUnavailable =
    workspace.hidden || workspace.inert || workspace.getAttribute('aria-hidden') === 'true';
  return !consolePage.hidden && workspaceUnavailable ? 'console' : 'workspace';
}

function setAvailable(element: HTMLElement, available: boolean): void {
  element.hidden = !available;
  element.inert = !available;
  if (available) {
    element.removeAttribute('aria-hidden');
  } else {
    element.setAttribute('aria-hidden', 'true');
  }
}

function isAvailableForFocus(element: HTMLElement): boolean {
  if (!element.isConnected) return false;

  let current: HTMLElement | null = element;
  while (current) {
    if (current.hidden || current.inert || current.getAttribute('aria-hidden') === 'true') {
      return false;
    }
    current = current.parentElement;
  }
  return true;
}

function focusIfAvailable(
  element: HTMLElement | null | undefined,
  ensureProgrammaticFocus = false,
): boolean {
  if (!element || !isAvailableForFocus(element)) return false;
  if (
    element instanceof HTMLButtonElement ||
    element instanceof HTMLInputElement ||
    element instanceof HTMLSelectElement ||
    element instanceof HTMLTextAreaElement
  ) {
    if (element.disabled) return false;
  }
  if (ensureProgrammaticFocus && !element.hasAttribute('tabindex') && element.tabIndex < 0) {
    element.setAttribute('tabindex', '-1');
  }
  element.focus({ preventScroll: true });
  return element.ownerDocument.activeElement === element;
}

export function prefersReducedConsoleMotion(windowRef: Window | null | undefined): boolean {
  if (!windowRef || typeof windowRef.matchMedia !== 'function') return false;
  return windowRef.matchMedia(REDUCED_MOTION_QUERY).matches;
}

export function supportsConsoleViewTransition(documentRef: Document): boolean {
  return (
    typeof (documentRef as unknown as DocumentWithOptionalViewTransition).startViewTransition ===
    'function'
  );
}

class ConsoleTransitionControllerImpl implements ConsoleTransitionController {
  private readonly documentRef: Document;
  private readonly windowRef: Window | null;
  private readonly workspace: HTMLElement;
  private readonly consolePage: HTMLElement;
  private readonly workspacePortalRoot: HTMLElement | null;
  private readonly classTarget: HTMLElement;
  private readonly scrollTargets: HTMLElement[];
  private readonly onBeforeWorkspaceHide: (() => void) | undefined;
  private readonly onAfterWorkspaceShow: (() => void) | undefined;
  private readonly fallbackDurationMs: number;
  private generation = 0;
  private disposed = false;
  private capturedWorkspaceFocus: HTMLElement | null = null;
  private capturedWorkspaceScroll: Map<HTMLElement, WorkspaceScrollPosition> | null = null;
  private currentSurface: ConsoleSurface;
  private requestedSurface: ConsoleSurface;

  constructor(
    elements: ConsoleTransitionElements,
    options: ConsoleTransitionControllerOptions = {},
  ) {
    this.documentRef = options.document ?? elements.workspace.ownerDocument;
    this.windowRef = options.window ?? this.documentRef.defaultView;
    this.workspace = elements.workspace;
    this.consolePage = elements.consolePage;
    this.workspacePortalRoot = elements.workspacePortalRoot ?? null;
    this.classTarget = elements.classTarget ?? this.documentRef.documentElement;
    this.scrollTargets = Array.from(
      new Set([this.workspace, ...(elements.scrollTargets ?? [])]),
    ).filter((target) => target === this.workspace || this.workspace.contains(target));
    this.onBeforeWorkspaceHide = elements.onBeforeWorkspaceHide;
    this.onAfterWorkspaceShow = elements.onAfterWorkspaceShow;
    this.fallbackDurationMs = Math.max(
      0,
      options.fallbackDurationMs ?? DEFAULT_FALLBACK_DURATION_MS,
    );
    this.currentSurface = options.initialSurface ?? inferInitialSurface(elements);
    this.requestedSurface = this.currentSurface;
    this.applySurface(this.currentSurface);
  }

  get surface(): ConsoleSurface {
    return this.currentSurface;
  }

  get desiredSurface(): ConsoleSurface {
    return this.requestedSurface;
  }

  showConsole(request?: ConsoleTransitionRequest): Promise<boolean> {
    return this.transitionTo('console', request);
  }

  showWorkspace(request?: ConsoleTransitionRequest): Promise<boolean> {
    return this.transitionTo('workspace', request);
  }

  async transitionTo(
    surface: ConsoleSurface,
    request: ConsoleTransitionRequest = {},
  ): Promise<boolean> {
    if (this.disposed) return false;

    const generation = ++this.generation;
    const previousIntent = this.requestedSurface;
    this.requestedSurface = surface;

    if (
      surface === 'console' &&
      previousIntent === 'workspace' &&
      this.currentSurface === 'workspace'
    ) {
      const active = this.documentRef.activeElement;
      this.capturedWorkspaceFocus =
        active instanceof HTMLElement &&
        (this.workspace.contains(active) || this.workspacePortalRoot?.contains(active))
          ? active
          : null;
      this.captureWorkspaceScroll();
    }

    if (surface === this.currentSurface) {
      this.clearTransitionClasses();
      this.applySurface(surface);
      this.focusForSurface(surface, request);
      return this.isCurrent(generation, surface);
    }

    if (prefersReducedConsoleMotion(this.windowRef)) {
      this.clearTransitionClasses();
      this.applySurface(surface);
      this.focusForSurface(surface, request);
      return this.isCurrent(generation, surface);
    }

    const startViewTransition = (this.documentRef as unknown as DocumentWithOptionalViewTransition)
      .startViewTransition;
    if (typeof startViewTransition === 'function') {
      try {
        return await this.runNativeTransition(startViewTransition, generation, surface, request);
      } catch {
        if (!this.isGenerationCurrent(generation)) return false;
      }
    }

    return this.runFallbackTransition(generation, surface, request);
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.generation += 1;
    if (this.currentSurface === 'console') {
      setAvailable(this.consolePage, false);
      setAvailable(this.workspace, true);
      if (this.workspacePortalRoot) setAvailable(this.workspacePortalRoot, true);
      this.restoreWorkspaceScroll();
      this.onAfterWorkspaceShow?.();
      this.currentSurface = 'workspace';
      this.requestedSurface = 'workspace';
    }
    this.capturedWorkspaceFocus = null;
    this.capturedWorkspaceScroll = null;
    this.clearTransitionClasses();
    this.classTarget.classList.remove(CONSOLE_TRANSITION_CLASSES.active);
  }

  private async runNativeTransition(
    startViewTransition: StartConsoleViewTransition,
    generation: number,
    surface: ConsoleSurface,
    request: ConsoleTransitionRequest,
  ): Promise<boolean> {
    this.beginTransitionClasses(surface, false);
    let applied = false;
    const transition = startViewTransition.call(this.documentRef, () => {
      if (!this.isGenerationCurrent(generation)) return;
      this.applySurface(surface);
      applied = true;
    });

    await (transition.updateCallbackDone ?? Promise.resolve());
    if (applied && this.isGenerationCurrent(generation)) {
      this.focusForSurface(surface, request);
    }

    try {
      await (transition.finished ?? Promise.resolve());
    } finally {
      if (this.isGenerationCurrent(generation)) this.clearTransitionClasses();
    }
    return applied && this.isCurrent(generation, surface);
  }

  private async runFallbackTransition(
    generation: number,
    surface: ConsoleSurface,
    request: ConsoleTransitionRequest,
  ): Promise<boolean> {
    if (!this.isGenerationCurrent(generation)) return false;
    this.beginTransitionClasses(surface, true);
    this.applySurface(surface);
    this.focusForSurface(surface, request);

    await this.delay(this.fallbackDurationMs);
    if (this.isGenerationCurrent(generation)) this.clearTransitionClasses();
    return this.isCurrent(generation, surface);
  }

  private applySurface(surface: ConsoleSurface): void {
    if (surface === 'console') {
      this.onBeforeWorkspaceHide?.();
      setAvailable(this.workspace, false);
      if (this.workspacePortalRoot) setAvailable(this.workspacePortalRoot, false);
      setAvailable(this.consolePage, true);
      this.classTarget.classList.add(CONSOLE_TRANSITION_CLASSES.active);
    } else {
      setAvailable(this.consolePage, false);
      setAvailable(this.workspace, true);
      if (this.workspacePortalRoot) setAvailable(this.workspacePortalRoot, true);
      this.restoreWorkspaceScroll();
      this.onAfterWorkspaceShow?.();
      this.classTarget.classList.remove(CONSOLE_TRANSITION_CLASSES.active);
    }
    this.currentSurface = surface;
  }

  private focusForSurface(surface: ConsoleSurface, request: ConsoleTransitionRequest): void {
    if (surface === 'console') {
      const focusTarget =
        request.focusTarget ??
        this.consolePage.querySelector<HTMLElement>('[data-console-focus], h1, [role="heading"]') ??
        this.consolePage;
      focusIfAvailable(focusTarget, true);
      return;
    }

    const restoreTarget = request.restoreTarget ?? this.capturedWorkspaceFocus;
    if (!focusIfAvailable(restoreTarget)) {
      const fallbackTarget = this.workspace.querySelector<HTMLElement>(
        '#input:not(:disabled), [data-action="nav-settings"]:not(:disabled), [data-action="nav-usage"]:not(:disabled), button:not(:disabled), input:not(:disabled), textarea:not(:disabled), select:not(:disabled), a[href]',
      );
      if (!focusIfAvailable(fallbackTarget)) focusIfAvailable(this.workspace, true);
    }
    this.capturedWorkspaceFocus = null;
  }

  private captureWorkspaceScroll(): void {
    this.capturedWorkspaceScroll = new Map(
      this.scrollTargets.map((target) => [
        target,
        { top: target.scrollTop, left: target.scrollLeft },
      ]),
    );
  }

  private restoreWorkspaceScroll(): void {
    const positions = this.capturedWorkspaceScroll;
    if (!positions) return;
    this.capturedWorkspaceScroll = null;
    for (const [target, position] of positions) {
      if (!target.isConnected || (target !== this.workspace && !this.workspace.contains(target))) {
        continue;
      }
      target.scrollTop = position.top;
      target.scrollLeft = position.left;
    }
  }

  private beginTransitionClasses(surface: ConsoleSurface, fallback: boolean): void {
    this.clearTransitionClasses();
    this.classTarget.classList.add(CONSOLE_TRANSITION_CLASSES.transitioning);
    this.classTarget.classList.toggle(CONSOLE_TRANSITION_CLASSES.fallback, fallback);
    this.classTarget.classList.toggle(CONSOLE_TRANSITION_CLASSES.entering, surface === 'console');
    this.classTarget.classList.toggle(CONSOLE_TRANSITION_CLASSES.leaving, surface === 'workspace');
  }

  private clearTransitionClasses(): void {
    this.classTarget.classList.remove(
      CONSOLE_TRANSITION_CLASSES.transitioning,
      CONSOLE_TRANSITION_CLASSES.fallback,
      CONSOLE_TRANSITION_CLASSES.entering,
      CONSOLE_TRANSITION_CLASSES.leaving,
    );
  }

  private isGenerationCurrent(generation: number): boolean {
    return !this.disposed && generation === this.generation;
  }

  private isCurrent(generation: number, surface: ConsoleSurface): boolean {
    return (
      this.isGenerationCurrent(generation) &&
      this.requestedSurface === surface &&
      this.currentSurface === surface
    );
  }

  private delay(durationMs: number): Promise<void> {
    if (!this.windowRef || durationMs === 0) return Promise.resolve();
    return new Promise((resolve) => {
      this.windowRef?.setTimeout(resolve, durationMs);
    });
  }
}

export function createConsoleTransitionController(
  elements: ConsoleTransitionElements,
  options: ConsoleTransitionControllerOptions = {},
): ConsoleTransitionController {
  return new ConsoleTransitionControllerImpl(elements, options);
}
