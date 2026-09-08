import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

const workspaceCss = readFileSync(resolve(process.cwd(), 'src/css/workspace.css'), 'utf8');
const chatCss = readFileSync(resolve(process.cwd(), 'src/css/chat.css'), 'utf8');
const usageCss = readFileSync(resolve(process.cwd(), 'src/css/usage-console.css'), 'utf8');

type Theme = 'light' | 'dark';

function declarations(body: string): Map<string, string> {
  const result = new Map<string, string>();
  for (const match of body.matchAll(/([\w-]+|--[\w-]+)\s*:\s*([^;]+);/g)) {
    result.set(match[1], match[2].trim());
  }
  return result;
}

function ruleDeclarations(css: string, selector: string): Map<string, string> {
  const result = new Map<string, string>();
  for (const match of css.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
    const selectors = match[1]
      .replace(/@[^;]+$/g, '')
      .split(',')
      .map((value) => value.trim());
    if (selectors.includes(selector)) {
      for (const [name, value] of declarations(match[2])) result.set(name, value);
    }
  }
  if (result.size === 0) throw new Error(`CSS selector not found: ${selector}`);
  return result;
}

function themeTokens(theme: Theme): Map<string, string> {
  const lightBlock = workspaceCss.match(/:root\s*\{([^}]*)\}/)?.[1];
  const darkBlock = workspaceCss.match(/:root\[data-theme='dark'\]\s*\{([^}]*)\}/)?.[1];
  if (!lightBlock || !darkBlock) throw new Error('Theme token blocks not found.');
  const tokens = declarations(lightBlock);
  if (theme === 'dark') {
    for (const [name, value] of declarations(darkBlock)) tokens.set(name, value);
  }
  return tokens;
}

function resolveColor(value: string, theme: Theme): string {
  const tokens = themeTokens(theme);
  let resolved = value.trim();
  const visited = new Set<string>();
  while (/^var\(--[\w-]+\)$/.test(resolved)) {
    const name = resolved.slice(4, -1);
    if (visited.has(name)) throw new Error(`Circular token: ${name}`);
    visited.add(name);
    const next = tokens.get(name);
    if (!next) throw new Error(`Unknown token: ${name}`);
    resolved = next;
  }
  if (!/^#[\da-f]{6}$/i.test(resolved)) throw new Error(`Expected a hex color, got: ${resolved}`);
  return resolved;
}

function channel(hex: string, offset: number): number {
  const value = Number.parseInt(hex.slice(offset, offset + 2), 16) / 255;
  return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
}

function luminance(hex: string): number {
  const value = hex.replace('#', '');
  return 0.2126 * channel(value, 0) + 0.7152 * channel(value, 2) + 0.0722 * channel(value, 4);
}

function contrast(foreground: string, background: string): number {
  const light = Math.max(luminance(foreground), luminance(background));
  const dark = Math.min(luminance(foreground), luminance(background));
  return (light + 0.05) / (dark + 0.05);
}

function actualPair(
  css: string,
  selector: string,
  theme: Theme,
): { foreground: string; background: string } {
  const rule = ruleDeclarations(css, selector);
  const color = rule.get('color');
  const background = rule.get('background');
  if (!color || !background) throw new Error(`Missing color/background for ${selector}`);
  return {
    foreground: resolveColor(color, theme),
    background: resolveColor(background, theme),
  };
}

describe('design token v2 contrast', () => {
  it.each(['light', 'dark'] as const)(
    'keeps actual %s semantic button combinations at WCAG AA contrast',
    (theme) => {
      const combinations = [
        actualPair(chatCss, '.plan-card-actions button.is-primary', theme),
        actualPair(usageCss, '.usage-console-state button', theme),
        actualPair(workspaceCss, '.btn-primary', theme),
        actualPair(workspaceCss, '.btn-primary.btn-danger', theme),
        actualPair(workspaceCss, '.btn-primary:disabled', theme),
        actualPair(chatCss, '.plan-card-actions button:disabled', theme),
        actualPair(usageCss, '.usage-console-state button:disabled', theme),
      ];

      for (const pair of combinations) {
        expect(contrast(pair.foreground, pair.background)).toBeGreaterThanOrEqual(4.5);
      }
    },
  );

  it('binds semantic fills and their on-colors through the actual component rules', () => {
    expect(
      ruleDeclarations(chatCss, '.plan-card-actions button.is-primary').get('background'),
    ).toBe('var(--color-plan-fill)');
    expect(ruleDeclarations(usageCss, '.usage-console-state button').get('background')).toBe(
      'var(--color-info-fill)',
    );
    expect(ruleDeclarations(workspaceCss, '.btn-primary').get('background')).toBe(
      'var(--color-action-fill)',
    );
    expect(ruleDeclarations(workspaceCss, '.btn-primary.btn-danger').get('background')).toBe(
      'var(--color-error-fill)',
    );
    expect(ruleDeclarations(workspaceCss, '.btn-primary:disabled').get('background')).toBe(
      'var(--color-disabled-fill)',
    );
    expect(ruleDeclarations(workspaceCss, '.btn-primary:disabled').get('opacity')).toBe('1');
    expect(ruleDeclarations(chatCss, '.plan-card-actions button:disabled').get('opacity')).toBe(
      '1',
    );
    expect(ruleDeclarations(usageCss, '.usage-console-state button:disabled').get('opacity')).toBe(
      '1',
    );
  });

  it('keeps brand, action, focus, selection, runtime, plan and data roles distinct', () => {
    const tokens = themeTokens('light');
    const roles = [
      '--color-brand-pink',
      '--color-brand-purple',
      '--color-action-fill',
      '--color-selected',
      '--color-focus',
      '--color-running',
      '--color-plan-fill',
      '--color-info-fill',
    ].map((name) => resolveColor(tokens.get(name) || '', 'light'));
    expect(new Set(roles).size).toBe(roles.length);
    expect(workspaceCss).not.toContain('#6554d9');
  });
});
