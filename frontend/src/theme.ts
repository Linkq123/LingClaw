// Theme module: light / dark / auto with localStorage persistence.
//
// - Resolves "auto" against `matchMedia('(prefers-color-scheme: dark)')`.
// - Sets `<html data-theme="light|dark">` (always explicit) so CSS only
//   needs to match `:root[data-theme='dark']`.
// - Swaps highlight.js stylesheet to match the resolved theme by toggling
//   `<link disabled>` on two pre-injected stylesheets. This replaces the
//   previous media-query-based `hljs-themes.css` import so that an explicit
//   user choice overrides the system setting.
// - No-ops gracefully when localStorage / matchMedia are unavailable
//   (private browsing, SSR, etc.).

// Vite `?url` imports resolve to the bundled asset URL at build time.
import githubLightHref from 'highlight.js/styles/github.css?url';
import githubDarkHref from 'highlight.js/styles/github-dark.css?url';

export type ThemeChoice = 'auto' | 'light' | 'dark';
export type ResolvedTheme = 'light' | 'dark';

const STORAGE_KEY = 'lingclaw.theme';

let currentChoice: ThemeChoice = 'auto';
let lightLink: HTMLLinkElement | null = null;
let darkLink: HTMLLinkElement | null = null;
let systemMql: MediaQueryList | null = null;
let systemListener: ((e: MediaQueryListEvent) => void) | null = null;

function readStoredChoice(): ThemeChoice {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    if (v === 'light' || v === 'dark' || v === 'auto') return v;
  } catch {
    /* localStorage blocked (private mode / cookies disabled) — fall through */
  }
  return 'auto';
}

function writeStoredChoice(v: ThemeChoice): void {
  try {
    localStorage.setItem(STORAGE_KEY, v);
  } catch {
    /* localStorage blocked — preference is session-only, acceptable */
  }
}

function resolve(choice: ThemeChoice): ResolvedTheme {
  if (choice === 'light' || choice === 'dark') return choice;
  if (typeof window !== 'undefined' && typeof window.matchMedia === 'function') {
    return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
  }
  return 'light';
}

function ensureHljsLinks(): void {
  if (typeof document === 'undefined') return;
  const head = document.head;
  if (!lightLink) {
    lightLink = document.createElement('link');
    lightLink.rel = 'stylesheet';
    lightLink.href = githubLightHref;
    lightLink.dataset.themeAsset = 'hljs-light';
    head.appendChild(lightLink);
  }
  if (!darkLink) {
    darkLink = document.createElement('link');
    darkLink.rel = 'stylesheet';
    darkLink.href = githubDarkHref;
    darkLink.dataset.themeAsset = 'hljs-dark';
    head.appendChild(darkLink);
  }
}

function applyEffective(effective: ResolvedTheme): void {
  if (typeof document !== 'undefined') {
    document.documentElement.dataset.theme = effective;
  }
  ensureHljsLinks();
  if (lightLink) lightLink.disabled = effective !== 'light';
  if (darkLink) darkLink.disabled = effective !== 'dark';
  updateThemeButton();
}

function updateThemeButton(): void {
  if (typeof document === 'undefined') return;
  const btn = document.getElementById('theme-toggle-btn');
  if (!btn) return;
  const labels: Record<ThemeChoice, string> = { auto: 'Auto', light: 'Light', dark: 'Dark' };
  const icons: Record<ThemeChoice, string> = { auto: '◐', light: '☀', dark: '☾' };
  btn.textContent = `${icons[currentChoice]} ${labels[currentChoice]}`;
  const nextChoice: ThemeChoice =
    currentChoice === 'auto' ? 'light' : currentChoice === 'light' ? 'dark' : 'auto';
  btn.setAttribute(
    'aria-label',
    `Theme: ${labels[currentChoice]}. Click to switch to ${labels[nextChoice]}.`,
  );
  btn.setAttribute('title', `Theme: ${labels[currentChoice]} (click to cycle)`);
  btn.setAttribute('data-theme-choice', currentChoice);
}

export function initTheme(): void {
  currentChoice = readStoredChoice();
  applyEffective(resolve(currentChoice));

  if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return;

  systemMql = window.matchMedia('(prefers-color-scheme: dark)');
  systemListener = () => {
    if (currentChoice === 'auto') applyEffective(resolve('auto'));
  };
  if (typeof systemMql.addEventListener === 'function') {
    systemMql.addEventListener('change', systemListener);
  } else if (
    typeof (systemMql as unknown as { addListener?: unknown }).addListener === 'function'
  ) {
    // Safari < 14 fallback.
    (
      systemMql as unknown as { addListener: (cb: (e: MediaQueryListEvent) => void) => void }
    ).addListener(systemListener);
  }
}

export function setTheme(choice: ThemeChoice): void {
  currentChoice = choice;
  writeStoredChoice(choice);
  applyEffective(resolve(choice));
}

export function cycleTheme(): void {
  // auto → light → dark → auto
  const next: ThemeChoice =
    currentChoice === 'auto' ? 'light' : currentChoice === 'light' ? 'dark' : 'auto';
  setTheme(next);
}

export function getThemeChoice(): ThemeChoice {
  return currentChoice;
}

export function disposeTheme(): void {
  if (systemMql && systemListener) {
    if (typeof systemMql.removeEventListener === 'function') {
      systemMql.removeEventListener('change', systemListener);
    } else if (
      typeof (systemMql as unknown as { removeListener?: unknown }).removeListener === 'function'
    ) {
      (
        systemMql as unknown as {
          removeListener: (cb: (e: MediaQueryListEvent) => void) => void;
        }
      ).removeListener(systemListener);
    }
  }
  systemMql = null;
  systemListener = null;
}
