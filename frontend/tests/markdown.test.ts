import { describe, it, expect } from 'vitest';
import {
  extractMath,
  findProgressiveSplitPoint,
  isSentenceSplitChar,
  renderMarkdown,
} from '../src/markdown.js';

describe('extractMath', () => {
  it('extracts inline math', () => {
    const r = extractMath('Hello $x^2$ world');
    expect(r.blocks).toHaveLength(1);
    expect(r.blocks[0].formula).toBe('x^2');
    expect(r.blocks[0].displayMode).toBe(false);
  });

  it('ignores currency dollar', () => {
    const r = extractMath('Price is $100');
    expect(r.blocks).toHaveLength(0);
  });

  it('extracts display math', () => {
    const r = extractMath('$$\\sum_{i=0}^n i$$');
    expect(r.blocks).toHaveLength(1);
    expect(r.blocks[0].displayMode).toBe(true);
  });

  it('ignores escaped dollar', () => {
    const r = extractMath('escaped \\$100');
    expect(r.blocks).toHaveLength(0);
  });

  it('ignores math in inline code', () => {
    const r = extractMath('`$x$` is code');
    expect(r.blocks).toHaveLength(0);
  });

  it('ignores math in fenced code', () => {
    const r = extractMath('```\n$x$\n```\nafter');
    expect(r.blocks).toHaveLength(0);
  });
});

describe('isSentenceSplitChar', () => {
  it('Chinese period', () => {
    expect(isSentenceSplitChar('你好。世界', 2)).toBe(true);
  });

  it('English period with space', () => {
    expect(isSentenceSplitChar('Hello. World', 5)).toBe(true);
  });

  it('period in number', () => {
    expect(isSentenceSplitChar('3.14', 1)).toBe(false);
  });

  it('exclamation CJK', () => {
    expect(isSentenceSplitChar('好！', 1)).toBe(true);
  });
});

describe('findProgressiveSplitPoint', () => {
  it('returns -1 for short text', () => {
    expect(findProgressiveSplitPoint('Short text')).toBe(-1);
  });

  it('splits at paragraph boundary', () => {
    const text = 'First paragraph.\n\nSecond paragraph.\n\nThird.';
    expect(findProgressiveSplitPoint(text)).toBeGreaterThan(0);
  });

  it('splits at code fence boundary', () => {
    const text = '```js\nconsole.log("hi")\n```\nAfter code.';
    expect(findProgressiveSplitPoint(text)).toBeGreaterThan(0);
  });
});

describe('renderMarkdown memoization', () => {
  it('skips unchanged raw content', async () => {
    const el = document.createElement('div');
    el._rawText = '**bold**';

    await renderMarkdown(el);
    expect(el.querySelector('strong')?.textContent).toBe('bold');

    const marker = document.createElement('span');
    marker.dataset.testMarker = 'preserved';
    el.appendChild(marker);

    await renderMarkdown(el);
    expect(el.querySelector('[data-test-marker="preserved"]')).not.toBeNull();
  });

  it('re-renders when raw content changes', async () => {
    const el = document.createElement('div');
    el._rawText = '**first**';
    await renderMarkdown(el);

    el._rawText = '**second**';
    await renderMarkdown(el);

    expect(el.querySelector('strong')?.textContent).toBe('second');
  });
});
