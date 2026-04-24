import { dom, state } from './state.js';
import {
  SOFT_SPLIT_MIN_CHARS,
  SOFT_SPLIT_MAX_CHARS,
  SOFT_SPLIT_TAIL_MIN_CHARS,
} from './constants.js';
import { escHtml, fallbackCopy, scheduleBackgroundTask, isAsciiDigit } from './utils.js';
import { scrollDown, invalidateChatScrollCache } from './scroll.js';

type MarkdownDeps = {
  hljs: typeof import('highlight.js').default;
  marked: typeof import('marked').marked;
  DOMPurify: typeof import('dompurify').default;
  katex: typeof import('katex').default;
};

let markdownDepsPromise: Promise<MarkdownDeps> | null = null;

function defaultExport<T>(module: T): T extends { default: infer D } ? D : T {
  return ((module as { default?: unknown }).default ?? module) as T extends { default: infer D }
    ? D
    : T;
}

async function loadMarkdownDeps(): Promise<MarkdownDeps> {
  if (markdownDepsPromise) return markdownDepsPromise;
  markdownDepsPromise = Promise.all([
    import('highlight.js'),
    import('marked'),
    import('marked-highlight'),
    import('dompurify'),
    import('katex'),
    import('katex/dist/katex.min.css'),
  ])
    .then(([hljsModule, markedModule, markedHighlightModule, domPurifyModule, katexModule]) => {
      const hljs = defaultExport(hljsModule);
      const { marked } = markedModule;
      const { markedHighlight } = markedHighlightModule;
      const DOMPurify = defaultExport(domPurifyModule);
      const katex = defaultExport(katexModule);

      marked.use(
        markedHighlight({
          highlight(code, lang) {
            if (code.length > 4000) {
              return escHtml(code);
            }
            if (lang && hljs.getLanguage(lang)) {
              return hljs.highlight(code, { language: lang }).value;
            }
            return escHtml(code);
          },
        }),
      );
      marked.setOptions({ breaks: true });

      return { hljs, marked, DOMPurify, katex };
    })
    .catch((error) => {
      markdownDepsPromise = null;
      throw error;
    });
  return markdownDepsPromise;
}

export function preloadMarkdownEngine(): Promise<void> {
  return loadMarkdownDeps().then(() => undefined);
}

// ── Markdown rendering queue ──

function cancelScheduledMarkdownRender(el) {
  if (!el) return;
  if (el._markdownIdleHandle) {
    if (typeof cancelIdleCallback === 'function') {
      cancelIdleCallback(el._markdownIdleHandle);
    } else {
      clearTimeout(el._markdownIdleHandle);
    }
    el._markdownIdleHandle = 0;
  }
}

function shouldHighlightBlock(block, index, totalBlocks) {
  const code = block.textContent || '';
  if (code.length > 4000) return false;
  if (totalBlocks > 6 && index >= 4) return false;
  return true;
}

export function scheduleCodeHighlight(blocks) {
  const codeBlocks = [...blocks];
  void loadMarkdownDeps()
    .then(({ hljs }) => {
      const highlightQueue = codeBlocks.filter((block, index) => {
        if (!block.isConnected || !shouldHighlightBlock(block, index, codeBlocks.length)) {
          block.classList.add('code-highlight-deferred');
          return false;
        }
        return true;
      });

      const highlightChunk = () => {
        let processed = 0;
        while (highlightQueue.length && processed < 2) {
          const block = highlightQueue.shift();
          if (block?.isConnected) {
            hljs.highlightElement(block);
          }
          processed += 1;
        }
        if (highlightQueue.length) {
          scheduleBackgroundTask(highlightChunk, 120);
        }
      };

      if (highlightQueue.length) {
        scheduleBackgroundTask(highlightChunk, 120);
      }
    })
    .catch((error) => {
      console.warn('Markdown highlighter failed to load:', error);
      codeBlocks.forEach((block) => block.classList.add('code-highlight-deferred'));
    });
}

export function scheduleMarkdownRender(el, options: { followScroll?: boolean } = {}) {
  if (!el) return;
  const { followScroll } = options;
  const raw = getMarkdownRaw(el);
  if (el._markdownRenderedRaw === raw && !el.classList.contains('markdown-pending')) {
    return;
  }
  cancelScheduledMarkdownRender(el);
  const queuedIndex = state.markdownRenderQueue.indexOf(el);
  if (queuedIndex !== -1) {
    state.markdownRenderQueue.splice(queuedIndex, 1);
  }
  el.classList.add('markdown-pending');
  el._markdownShouldFollow =
    typeof followScroll === 'boolean'
      ? followScroll
      : dom.chat.scrollHeight - dom.chat.scrollTop - dom.chat.clientHeight < 80;
  state.markdownRenderQueue.push(el);
  if (!state.markdownQueueHandle) {
    state.markdownQueueHandle = scheduleBackgroundTask(processMarkdownQueue);
  }
}

async function processMarkdownQueue() {
  state.markdownQueueHandle = 0;
  const el = state.markdownRenderQueue.shift();
  if (!el) return;
  el._markdownIdleHandle = 0;
  if (el.isConnected) {
    try {
      await renderMarkdown(el);
      if (el._markdownShouldFollow) scrollDown();
    } catch (error) {
      console.warn('Markdown render failed:', error);
    } finally {
      el.classList.remove('markdown-pending');
      invalidateChatScrollCache();
    }
  }
  el._markdownShouldFollow = false;
  if (state.markdownRenderQueue.length) {
    state.markdownQueueHandle = scheduleBackgroundTask(processMarkdownQueue);
  }
}

// ── Progressive segmented markdown ──

export function isSentenceSplitChar(text, index) {
  const ch = text[index];
  if ('。！？；：'.includes(ch)) return true;
  if ('!?;:'.includes(ch)) {
    const next = text[index + 1] || '';
    return !next || /\s/.test(next);
  }
  if (ch === '.') {
    const prev = text[index - 1] || '';
    const next = text[index + 1] || '';
    return /[A-Za-z0-9\)]/.test(prev) && (!next || /\s/.test(next));
  }
  return false;
}

export function findProgressiveSplitPoint(text) {
  let inFence = false;
  let lastSplit = -1;
  let lastSoftSplit = -1;
  let charsSinceBoundary = 0;
  let i = 0;
  while (i < text.length) {
    const atLineStart = i === 0 || text[i - 1] === '\n';
    if (
      atLineStart &&
      i + 2 < text.length &&
      text[i] === '`' &&
      text[i + 1] === '`' &&
      text[i + 2] === '`'
    ) {
      const wasFenced = inFence;
      inFence = !inFence;
      let j = i + 3;
      while (j < text.length && text[j] !== '\n') j++;
      i = j < text.length ? j + 1 : text.length;
      if (wasFenced && !inFence && i < text.length) {
        lastSplit = i;
        lastSoftSplit = -1;
        charsSinceBoundary = 0;
      }
      continue;
    }
    if (!inFence && text[i] === '\n' && i + 1 < text.length && text[i + 1] === '\n') {
      let j = i + 2;
      while (j < text.length && text[j] === '\n') j++;
      lastSplit = j;
      lastSoftSplit = -1;
      charsSinceBoundary = 0;
      i = j;
      continue;
    }
    if (!inFence) {
      charsSinceBoundary += 1;
      if (isSentenceSplitChar(text, i) && charsSinceBoundary >= SOFT_SPLIT_MIN_CHARS) {
        lastSoftSplit = i + 1;
      } else if (/\s/.test(text[i]) && charsSinceBoundary >= SOFT_SPLIT_MAX_CHARS) {
        lastSoftSplit = i + 1;
      }
    }
    i++;
  }
  if (!inFence && lastSoftSplit > lastSplit) {
    const tailLength = text.length - lastSoftSplit;
    if (tailLength === 0 || tailLength >= SOFT_SPLIT_TAIL_MIN_CHARS) {
      return lastSoftSplit;
    }
  }
  return lastSplit;
}

// ── Math (KaTeX) integration ──

export function extractMath(text) {
  const blocks = [];
  let out = '';
  let i = 0;
  const len = text.length;

  while (i < len) {
    if (text[i] === '\\' && i + 1 < len && text[i + 1] === '$') {
      out += '\\$';
      i += 2;
      continue;
    }

    const atLineStart = i === 0 || text[i - 1] === '\n';
    if (
      atLineStart &&
      i + 2 < len &&
      text[i] === '`' &&
      text[i + 1] === '`' &&
      text[i + 2] === '`'
    ) {
      const endMarker = text.indexOf('\n```', i + 3);
      if (endMarker === -1) {
        out += text.substring(i);
        i = len;
        continue;
      }
      let end = endMarker + 4;
      while (end < len && text[end] !== '\n') end++;
      out += text.substring(i, end);
      i = end;
      continue;
    }

    if (text[i] === '`') {
      let j = i + 1;
      while (j < len && text[j] !== '`' && text[j] !== '\n') j++;
      if (j < len && text[j] === '`') {
        out += text.substring(i, j + 1);
        i = j + 1;
        continue;
      }
    }

    if (text[i] === '$' && i + 1 < len && text[i + 1] === '$') {
      const searchFrom = i + 2;
      const end = text.indexOf('$$', searchFrom);
      if (end !== -1 && end > searchFrom) {
        const formula = text.substring(searchFrom, end);
        const id = blocks.length;
        blocks.push({ formula: formula.trim(), displayMode: true });
        out += `<span data-math="${id}"></span>`;
        i = end + 2;
        continue;
      }
    }

    const prev = i > 0 ? text[i - 1] : '';
    if (
      text[i] === '$' &&
      i + 1 < len &&
      text[i + 1] !== '$' &&
      text[i + 1] !== ' ' &&
      text[i + 1] !== '\n' &&
      !isAsciiDigit(prev)
    ) {
      let j = i + 1;
      while (j < len && text[j] !== '$' && text[j] !== '\n') j++;
      const next = j + 1 < len ? text[j + 1] : '';
      if (j < len && text[j] === '$' && j > i + 1 && text[j - 1] !== ' ' && !isAsciiDigit(next)) {
        const formula = text.substring(i + 1, j);
        const id = blocks.length;
        blocks.push({ formula, displayMode: false });
        out += `<span data-math="${id}"></span>`;
        i = j + 1;
        continue;
      }
    }

    out += text[i];
    i++;
  }

  return { text: out, blocks };
}

export function renderMathPlaceholders(html, mathBlocks, katexRenderer) {
  if (!mathBlocks.length) return html;
  return html.replace(/<span data-math="(\d+)"><\/span>/g, (_, idStr) => {
    const id = parseInt(idStr, 10);
    if (id < 0 || id >= mathBlocks.length) return '';
    const { formula, displayMode } = mathBlocks[id];
    try {
      return katexRenderer.renderToString(formula, {
        displayMode,
        throwOnError: false,
        output: 'htmlAndMathml',
      });
    } catch (e) {
      console.warn('KaTeX render failed:', formula, e);
      return `<code>${escHtml(formula)}</code>`;
    }
  });
}

// ── Code blocks ──

export function decorateCodeBlocks(container) {
  container.querySelectorAll('pre').forEach((pre) => {
    pre.style.position = 'relative';
    const codeEl = pre.querySelector('code');
    if (codeEl) {
      const cls = [...codeEl.classList].find((c) => c.startsWith('language-'));
      if (cls) {
        const label = document.createElement('span');
        label.className = 'code-lang-label';
        label.textContent = cls.replace('language-', '');
        pre.appendChild(label);
      }
    }
    const btn = document.createElement('button');
    btn.className = 'copy-btn';
    btn.textContent = '复制';
    btn.onclick = () => {
      const code = pre.querySelector('code');
      const text = code?.textContent || pre.textContent;
      if (navigator.clipboard) {
        navigator.clipboard.writeText(text).catch(() => fallbackCopy(text));
      } else {
        fallbackCopy(text);
      }
      btn.textContent = '已复制';
      setTimeout(() => (btn.textContent = '复制'), 1500);
    };
    pre.appendChild(btn);
  });
}

export async function appendRenderedSegment(el, markdownText) {
  const { marked, DOMPurify, katex } = await loadMarkdownDeps();
  const { text: preprocessed, blocks: mathBlocks } = extractMath(markdownText);
  const html = marked.parse(preprocessed) as string;
  const sanitized = DOMPurify.sanitize(html, { ADD_ATTR: ['target'] });
  const temp = document.createElement('div');
  temp.innerHTML = renderMathPlaceholders(sanitized, mathBlocks, katex);
  temp.querySelectorAll('a[href]').forEach((a) => {
    a.setAttribute('target', '_blank');
    a.setAttribute('rel', 'noopener noreferrer');
  });
  decorateCodeBlocks(temp);
  const codeBlocks = [...temp.querySelectorAll('pre code')];
  const tail = el._liveTail;
  while (temp.firstChild) {
    if (tail && tail.parentNode === el) {
      el.insertBefore(temp.firstChild, tail);
    } else {
      el.appendChild(temp.firstChild);
    }
  }
  invalidateChatScrollCache();
  scheduleCodeHighlight(codeBlocks);
}

// ── Live tail (streaming partial content) ──

export function parseOpenCodeFence(text) {
  const normalized = text.replace(/^\n+/, '');
  if (!normalized.startsWith('```')) return null;

  const firstNewline = normalized.indexOf('\n');
  if (firstNewline === -1) return null;

  const rest = normalized.slice(firstNewline + 1);
  if (/(^|\n)```/.test(rest)) return null;

  const info = normalized.slice(3, firstNewline).trim();
  const language = info ? info.split(/\s+/, 1)[0] : '';
  return { language, code: rest };
}

function ensureLiveTail(el, mode) {
  if (el._liveTail && el._liveTail.dataset.mode === mode) {
    return el._liveTail;
  }

  removeLiveTail(el);

  if (mode === 'code') {
    const pre = document.createElement('pre');
    pre.className = 'live-tail live-code-tail';
    pre.dataset.mode = 'code';
    const code = document.createElement('code');
    pre.appendChild(code);
    el.appendChild(pre);
    el._liveTail = pre;
    invalidateChatScrollCache();
    return pre;
  }

  const span = document.createElement('span');
  span.className = 'live-tail';
  span.dataset.mode = 'text';
  el.appendChild(span);
  el._liveTail = span;
  invalidateChatScrollCache();
  return span;
}

export function updateLiveTail(el, text) {
  if (!text) {
    removeLiveTail(el);
    return;
  }

  const codeTail = parseOpenCodeFence(text);
  if (codeTail) {
    const tail = ensureLiveTail(el, 'code');
    let label = tail.querySelector('.code-lang-label');
    if (codeTail.language) {
      if (!label) {
        label = document.createElement('span');
        label.className = 'code-lang-label';
        tail.insertBefore(label, tail.firstChild);
      }
      label.textContent = codeTail.language;
    } else if (label) {
      label.remove();
    }
    const codeEl = tail.querySelector('code');
    if (codeEl) {
      codeEl.textContent = codeTail.code;
    }
    invalidateChatScrollCache();
    return;
  }

  const tail = ensureLiveTail(el, 'text');
  tail.textContent = text;
  invalidateChatScrollCache();
}

export function removeLiveTail(el) {
  if (el._liveTail) {
    if (el._liveTail.parentNode) el._liveTail.parentNode.removeChild(el._liveTail);
    el._liveTail = null;
    invalidateChatScrollCache();
  }
}

function getMarkdownRaw(el) {
  const raw = el._rawText ?? el.textContent ?? '';
  if (!el._rawText) {
    el._rawText = raw;
  }
  return raw;
}

export async function renderMarkdown(el) {
  const raw = getMarkdownRaw(el);
  if (el._markdownRenderedRaw === raw) {
    invalidateChatScrollCache();
    return;
  }
  const { marked, DOMPurify, katex } = await loadMarkdownDeps();
  const { text: preprocessed, blocks: mathBlocks } = extractMath(raw);
  const html = marked.parse(preprocessed) as string;
  const sanitized = DOMPurify.sanitize(html, { ADD_ATTR: ['target'] });
  el.innerHTML = renderMathPlaceholders(sanitized, mathBlocks, katex);
  el._liveTail = null;
  el.querySelectorAll('a[href]').forEach((a) => {
    a.setAttribute('target', '_blank');
    a.setAttribute('rel', 'noopener noreferrer');
  });
  el._markdownIdleHandle = 0;

  decorateCodeBlocks(el);
  scheduleCodeHighlight(el.querySelectorAll('pre code'));
  el._markdownRenderedRaw = raw;
  invalidateChatScrollCache();
}
