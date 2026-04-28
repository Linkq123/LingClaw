import { afterEach, describe, it, expect } from 'vitest';
import {
  extractMath,
  findProgressiveSplitPoint,
  isSentenceSplitChar,
  needsFinalMarkdownRender,
  preloadMarkdownEngine,
  renderMarkdown,
} from '../src/markdown.js';
import { finishAssistantStream } from '../src/handlers/stream.js';
import { state } from '../src/state.js';

afterEach(() => {
  state.currentMsg = null;
  state.pendingAssistantText = '';
  document.body.innerHTML = '';
});

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

  it('period in ordered list marker', () => {
    expect(isSentenceSplitChar('3. 中期修', 1)).toBe(false);
  });

  it('period in blockquote ordered list marker', () => {
    expect(isSentenceSplitChar('> 12. 中期修', 4)).toBe(false);
  });

  it('period in common abbreviation', () => {
    expect(isSentenceSplitChar('e.g. 后面还是同一句', 3)).toBe(false);
    expect(isSentenceSplitChar('U.S. 后面还是同一句', 3)).toBe(false);
  });

  it('full-width punctuation before inline markdown opener', () => {
    expect(isSentenceSplitChar('标题：**粗体**后续说明', 2)).toBe(false);
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

  it('startFrom: incremental scan from intermediate boundary agrees with full scan', () => {
    // Four paragraphs — full scan finds the boundary before the last paragraph.
    const text = 'First paragraph.\n\nSecond paragraph.\n\nThird paragraph.\n\nFourth.';
    const full = findProgressiveSplitPoint(text);
    expect(full).toBeGreaterThan(0);
    // Scan from the boundary BEFORE the third paragraph (position 36).
    // Should find the same final boundary as the full scan.
    const incremental = findProgressiveSplitPoint(text, 36);
    expect(incremental).toBe(full);
    expect(incremental).toBeGreaterThan(36);
  });

  it('startFrom: returns startFrom when no further boundary exists', () => {
    // Text ends right after the first paragraph with no further double-newline.
    const text = 'Para one.\n\nTail.';
    const full = findProgressiveSplitPoint(text);
    expect(full).toBeGreaterThan(0);
    // Resume from that boundary — nothing beyond, so result equals startFrom.
    const incremental = findProgressiveSplitPoint(text, full);
    expect(incremental).toBe(full);
  });

  it('startFrom: handles soft split as a valid resume point', () => {
    // A long paragraph ending in a sentence split, followed by two more paragraphs.
    const line1 =
      'This is a fairly long sentence that exceeds the minimum chars threshold. And this follows.';
    const text = line1 + '\n\nSecond paragraph.\n\nThird paragraph.';
    const full = findProgressiveSplitPoint(text);
    expect(full).toBeGreaterThan(0);
    // Resume from the paragraph boundary after line1 (len(line1) + 2 = position of "Second").
    const midOffset = line1.length + 2;
    const incremental = findProgressiveSplitPoint(text, midOffset);
    // Should find the boundary before "Third paragraph.", same as full scan.
    expect(incremental).toBe(full);
    expect(incremental).toBeGreaterThan(midOffset);
  });

  it('does not soft-split inside a GFM table row', () => {
    // A table row with a long cell containing a sentence-split character should
    // NOT trigger a soft split, so the whole table renders atomically.
    const tableMarkdown = [
      '| Column A | Column B: This is a description that is long enough to exceed SOFT_SPLIT_MIN_CHARS. More text. |',
      '| -------- | ------- |',
      '| val1     | val2    |',
    ].join('\n');
    // The table rows contain '.' after > 72 chars, but no soft split should occur.
    const result = findProgressiveSplitPoint(tableMarkdown);
    // Expect no split point found (returns -1) — no paragraph boundary either.
    expect(result).toBe(-1);
  });

  it('does not soft-split inside a table even when preceded by a paragraph', () => {
    // Paragraph followed immediately by a table (no blank line between them would
    // not be valid GFM, but even with a blank line the table rows must be atomic).
    const para = 'Introductory text here.\n\n';
    const tableRows = [
      '| Option | A very long description that easily exceeds the soft-split minimum threshold. And more. |',
      '| ------ | ------- |',
      '| A      | val     |',
    ].join('\n');
    const text = para + tableRows;
    const result = findProgressiveSplitPoint(text);
    // The only valid split is the paragraph boundary before the table (after para).
    // Any split inside the table rows is forbidden.
    // The split point should be ≤ para.length (right at the start of the table),
    // not inside a table row.
    if (result > 0) {
      expect(result).toBeLessThanOrEqual(para.length);
    }
  });

  it('does not soft-split inside a GFM table without a leading pipe in the header row', () => {
    const tableMarkdown = [
      'Column A | A long column title that exceeds the soft-split threshold. More text.',
      '-------- | -------',
      'val1     | val2',
    ].join('\n');

    expect(findProgressiveSplitPoint(tableMarkdown)).toBe(-1);
  });

  it('does not soft-split inside a GFM table without a leading pipe in data rows', () => {
    const tableMarkdown = [
      'Column A | Column B',
      '-------- | -------',
      'val1 | This is a long table cell that exceeds the minimum split threshold. More text.',
    ].join('\n');

    expect(findProgressiveSplitPoint(tableMarkdown)).toBe(-1);
  });

  it('does not soft-split immediately after an ordered list marker', () => {
    const text =
      'a'.repeat(90) +
      '\n3. 中期修 — 这一项后面还有足够长的正文内容，用来确保尾部长度超过软分段阈值。';

    const listMarkerDotIndex = text.indexOf('3.') + 1;

    expect(findProgressiveSplitPoint(text)).not.toBe(listMarkerDotIndex + 1);
  });

  it('does not soft-split after a blockquote ordered list marker', () => {
    const text =
      'a'.repeat(90) +
      '\n> 12. 这是 blockquote 里的有序列表项，后面还有足够长的正文内容来触发软分段检查。';

    const listMarkerDotIndex = text.indexOf('12.') + 2;

    expect(findProgressiveSplitPoint(text)).not.toBe(listMarkerDotIndex + 1);
  });

  it('does not soft-split after a reference-style link definition colon', () => {
    const text =
      'a'.repeat(90) +
      '\n[spec]: https://example.com/very/long/path/that/should/remain-attached';

    expect(findProgressiveSplitPoint(text)).toBe(-1);
  });

  it('does not soft-split after a blockquote reference-style link definition colon', () => {
    const text =
      'a'.repeat(90) +
      '\n> [spec]: https://example.com/very/long/path/that/should/remain-attached';

    expect(findProgressiveSplitPoint(text)).toBe(-1);
  });

  it('does not soft-split after common abbreviations', () => {
    const text =
      'a'.repeat(90) +
      ' e.g. 这里其实还在同一句里，后面还有足够长的正文内容来验证缩写误判。';

    const abbreviationDotIndex = text.indexOf('e.g.') + 'e.g.'.length - 1;

    expect(findProgressiveSplitPoint(text)).not.toBe(abbreviationDotIndex + 1);
  });

  it('does not soft-split after full-width punctuation before inline markdown', () => {
    const text =
      'a'.repeat(90) +
      '\n### 长标题长标题长标题长标题：**后面紧跟粗体内容**并继续说明，且尾部再补一段更长的正文避免回落到别的切分点';

    const fullWidthColonIndex = text.indexOf('：');

    expect(findProgressiveSplitPoint(text)).not.toBe(fullWidthColonIndex + 1);
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

  it('wraps gfm tables for horizontal scrolling and styling', async () => {
    const el = document.createElement('div');
    el._rawText = [
      '| # | 热点 | 核心内容 |',
      '| --- | --- | --- |',
      '| 1 | OpenAI | 发布新模型 |',
    ].join('\n');

    await renderMarkdown(el);

    const wrap = el.querySelector('.markdown-table-wrap');
    expect(wrap).not.toBeNull();
    expect(wrap?.querySelector('table')).not.toBeNull();
    expect(el.querySelectorAll('thead th')).toHaveLength(3);
    expect(el.querySelector('tbody td')?.textContent).toBe('1');
  });

  it('renders a gfm table after a paragraph without a blank line', async () => {
    const el = document.createElement('div');
    el._rawText = [
      'Intro before the table:',
      '| Site | Url | Notes |',
      '| --- | --- | --- |',
      '| BateCode | https://www.batecode.xyz | Works |',
    ].join('\n');

    await renderMarkdown(el);

    expect(el.querySelector('p')?.textContent).toContain('Intro before the table');
    expect(el.querySelector('.markdown-table-wrap table')).not.toBeNull();
    expect(el.querySelectorAll('tbody tr')).toHaveLength(1);
  });

  it('renders common markdown blocks and inline elements', async () => {
    const el = document.createElement('div');
    el._rawText = [
      '# 标题',
      '',
      '包含 **加粗**、*强调*、`inline` 和 [链接](https://example.com)。',
      '',
      '> 引用内容',
      '',
      '- [x] 已完成任务',
      '- 普通列表',
      '',
      '1. 有序项目',
      '',
      '```ts',
      'const value = 1;',
      '```',
      '',
      '---',
      '',
      '![示例图片](https://example.com/image.png)',
    ].join('\n');

    await renderMarkdown(el);

    expect(el.querySelector('h1')?.textContent).toBe('标题');
    expect(el.querySelector('strong')?.textContent).toBe('加粗');
    expect(el.querySelector('em')?.textContent).toBe('强调');
    expect(el.querySelector('p code')?.textContent).toBe('inline');
    expect(el.querySelector('a')?.getAttribute('target')).toBe('_blank');
    expect(el.querySelector('a')?.getAttribute('rel')).toBe('noopener noreferrer');
    expect(el.querySelector('blockquote')?.textContent).toContain('引用内容');
    expect(el.querySelector('input[type="checkbox"]')).not.toBeNull();
    expect(el.querySelector('ol li')?.textContent).toContain('有序项目');
    expect(el.querySelector('pre code')?.textContent).toContain('const value = 1;');
    expect(el.querySelector('hr')).not.toBeNull();
    expect(el.querySelector('img')?.getAttribute('alt')).toBe('示例图片');
  });
});

describe('needsFinalMarkdownRender', () => {
  it('returns false for plain text that already rendered cleanly', () => {
    const el = document.createElement('div');
    el.innerHTML = '<p>这是一段已经稳定显示的正文。</p>';

    expect(needsFinalMarkdownRender(el, '这是一段已经稳定显示的正文。')).toBe(false);
  });

  it('detects unresolved table markdown when no table element exists yet', () => {
    const el = document.createElement('div');
    el.innerHTML = '<p>| # | 热点 | 核心内容 |</p><p>| --- | --- | --- |</p>';

    expect(
      needsFinalMarkdownRender(
        el,
        ['| # | 热点 | 核心内容 |', '| --- | --- | --- |', '| 1 | OpenAI | 发布新模型 |'].join(
          '\n',
        ),
      ),
    ).toBe(true);
  });

  it('detects unresolved later tables even when an earlier table rendered', () => {
    const el = document.createElement('div');
    el.innerHTML = [
      '<div class="markdown-table-wrap"><table><tbody><tr><td>1</td></tr></tbody></table></div>',
      '<p>| Name | Value |</p><p>| --- | --- |</p>',
    ].join('');

    expect(
      needsFinalMarkdownRender(
        el,
        [
          '| # | Item |',
          '| --- | --- |',
          '| 1 | First |',
          '',
          '| Name | Value |',
          '| --- | --- |',
          '| Second | Pending |',
        ].join('\n'),
      ),
    ).toBe(true);
  });
});

describe('finishAssistantStream markdown finalization', () => {
  it('re-renders segmented table markdown as a final table', async () => {
    await preloadMarkdownEngine();

    const row = document.createElement('div');
    row.className = 'msg-row assistant';
    row.hidden = true;

    const message = document.createElement('div');
    message.className = 'msg assistant';
    message._rawText = [
      '今日热点日报',
      '',
      '| # | 热点 | 核心内容 |',
      '| --- | --- | --- |',
      '| 1 | OpenAI | 发布新模型 |',
      '| 2 | Gemini | 网关修复 |',
    ].join('\n');
    message._renderedOffset = 18;
    message.innerHTML = '<p>今日热点日报</p><p>| # | 热点 | 核心内容 |</p>';

    row.appendChild(message);
    document.body.appendChild(row);
    state.currentMsg = message;

    const result = finishAssistantStream();
    expect(result).toBe(message);

    await Promise.resolve();
    await new Promise((resolve) => setTimeout(resolve, 0));
    await Promise.resolve();

    expect(row.hidden).toBe(false);
    expect(message.querySelector('.markdown-table-wrap table')).not.toBeNull();
    expect(message.querySelectorAll('tbody tr')).toHaveLength(2);
  });

  it('finalizes a table after a paragraph without a blank line', async () => {
    await preloadMarkdownEngine();

    const row = document.createElement('div');
    row.className = 'msg-row assistant';
    row.hidden = true;

    const message = document.createElement('div');
    message.className = 'msg assistant';
    message._rawText = [
      'Intro before the table:',
      '| Site | Url | Notes |',
      '| --- | --- | --- |',
      '| BateCode | https://www.batecode.xyz | Works |',
    ].join('\n');
    message._renderedOffset = message._rawText.length;
    message.innerHTML =
      '<p>Intro before the table:</p><p>| Site | Url | Notes |</p><p>| --- | --- | --- |</p>';

    row.appendChild(message);
    document.body.appendChild(row);
    state.currentMsg = message;

    const result = finishAssistantStream();
    expect(result).toBe(message);

    await Promise.resolve();
    await new Promise((resolve) => setTimeout(resolve, 0));
    await Promise.resolve();

    expect(message.querySelector('.markdown-table-wrap table')).not.toBeNull();
    expect(message.querySelectorAll('tbody tr')).toHaveLength(1);
  });

  it('keeps already-rendered plain text DOM when no final markdown correction is needed', async () => {
    await preloadMarkdownEngine();

    const row = document.createElement('div');
    row.className = 'msg-row assistant';
    row.hidden = true;

    const message = document.createElement('div');
    message.className = 'msg assistant';
    message._rawText = '第一段已经渲染完成。\n\n第二段也是普通正文。';
    message._renderedOffset = message._rawText.length;
    message.innerHTML =
      '<p>第一段已经渲染完成。</p><p>第二段也是普通正文。</p><span data-test-marker="keep"></span>';

    row.appendChild(message);
    document.body.appendChild(row);
    state.currentMsg = message;

    const result = finishAssistantStream();
    expect(result).toBe(message);

    await Promise.resolve();
    await new Promise((resolve) => setTimeout(resolve, 0));
    await Promise.resolve();

    expect(message.querySelector('[data-test-marker="keep"]')).not.toBeNull();
  });
});
