import { describe, it, expect } from 'vitest';
import { escHtml, formatToolDuration, canSendWhileBusy } from '../src/utils.js';

describe('escHtml', () => {
  it('escapes XSS vector', () => {
    expect(escHtml('<script>alert("xss")</script>')).toBe(
      '&lt;script&gt;alert(&quot;xss&quot;)&lt;/script&gt;',
    );
  });
  it('handles null value', () => {
    expect(escHtml(null)).toBe('');
  });
  it('passes normal text through', () => {
    expect(escHtml('Hello World')).toBe('Hello World');
  });
  it('escapes ampersand and quotes', () => {
    expect(escHtml('a & b "c" \'d\'')).toBe('a &amp; b &quot;c&quot; &#39;d&#39;');
  });
});

describe('formatToolDuration', () => {
  it('formats milliseconds', () => {
    expect(formatToolDuration(42)).toBe('42ms');
  });
  it('formats seconds', () => {
    expect(formatToolDuration(3500)).toBe('3.5s');
  });
  it('formats large seconds', () => {
    expect(formatToolDuration(15000)).toBe('15s');
  });
  it('returns empty for null', () => {
    expect(formatToolDuration(null)).toBe('');
  });
  it('treats zero as 1ms', () => {
    expect(formatToolDuration(0)).toBe('1ms');
  });
});

describe('canSendWhileBusy', () => {
  it('allows /stop', () => {
    expect(canSendWhileBusy('/stop')).toBe(true);
  });
  it('allows /tool on', () => {
    expect(canSendWhileBusy('/tool on')).toBe(true);
  });
  it('allows /reasoning off', () => {
    expect(canSendWhileBusy('/reasoning off')).toBe(true);
  });
  it('blocks /clear', () => {
    expect(canSendWhileBusy('/clear')).toBe(false);
  });
  it('blocks /help', () => {
    expect(canSendWhileBusy('/help')).toBe(false);
  });
});
