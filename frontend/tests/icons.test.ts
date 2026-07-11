import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

import { ICON_NAMES, createIcon, iconHref, iconMarkup } from '../src/icons.js';

const indexHtml = readFileSync(resolve(process.cwd(), 'index.html'), 'utf8');

describe('local SVG icon system', () => {
  it('defines every typed icon exactly once in the sprite', () => {
    const symbolNames = Array.from(
      indexHtml.matchAll(/<symbol id="icon-([^"]+)"/g),
      (match) => match[1],
    );

    expect(new Set(symbolNames).size).toBe(symbolNames.length);
    for (const name of ICON_NAMES) {
      expect(symbolNames).toContain(name);
    }
  });

  it('creates decorative current-document SVG references', () => {
    expect(iconHref('trash')).toBe('#icon-trash');
    expect(iconMarkup('trash')).toContain('href="#icon-trash"');

    const icon = createIcon('trash');
    expect(icon.getAttribute('aria-hidden')).toBe('true');
    expect(icon.getAttribute('focusable')).toBe('false');
    expect(icon.querySelector('use')?.getAttribute('href')).toBe('#icon-trash');
  });

  it('keeps display glyphs out of dynamic panel renderers', () => {
    const rendererSources = [
      '../src/images.ts',
      '../src/main.ts',
      '../src/renderers/chat.ts',
      '../src/renderers/reasoning.ts',
      '../src/renderers/tools.ts',
      '../src/renderers/todos.ts',
      '../src/renderers/task-plan.ts',
      '../src/renderers/subagent.ts',
      '../src/renderers/orchestrate.ts',
      '../src/pages/SettingsPage.tsx',
    ].map((path) => readFileSync(resolve(process.cwd(), 'tests', path), 'utf8'));

    for (const source of rendererSources) {
      expect(source).not.toMatch(/[×✕✗⚡📋ℹ⚠▣✦⚙💭▸]/u);
      expect(source).not.toMatch(/\\u(?:00d7|25b8|d83d\\udcad)/i);
      expect(source).not.toContain('&#10022;');
      expect(source).not.toContain('&#9656;');
      expect(source).not.toContain('&times;');
    }
  });
});
