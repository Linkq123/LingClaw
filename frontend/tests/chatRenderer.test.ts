import { beforeEach, describe, expect, it } from 'vitest';

import { addMsg, addSystem } from '../src/renderers/chat.js';
import { initDomRefs } from '../src/state.js';

describe('chat renderer semantics', () => {
  beforeEach(() => {
    document.body.innerHTML = '<main id="chat"></main>';
    initDomRefs();
  });

  it('renders message timestamps as semantic time elements', () => {
    addMsg('user', 'Hello', 1_700_000_000, { trackUnread: false });

    const timestamp = document.querySelector('.msg-time');
    expect(timestamp?.tagName).toBe('TIME');
    expect(timestamp?.getAttribute('datetime')).toBe(new Date(1_700_000_000_000).toISOString());
  });

  it('falls back to a valid current timestamp for malformed history data', () => {
    const before = Date.now();

    addMsg('user', 'Hello', Number.POSITIVE_INFINITY, { trackUnread: false });
    const timestamp = document.querySelector('time.msg-time');
    const parsed = Date.parse(timestamp?.getAttribute('datetime') || '');

    expect(Number.isFinite(parsed)).toBe(true);
    expect(parsed).toBeGreaterThanOrEqual(before);
    expect(parsed).toBeLessThanOrEqual(Date.now());
  });

  it('keeps short system notices in the compact inline form', () => {
    addSystem('Configuration updated', 'success');

    expect(document.querySelector('.system-card.system-inline.success-card')).not.toBeNull();
    expect(document.querySelector('.system-body')).toBeNull();
  });
});
