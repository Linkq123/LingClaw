import { beforeEach, describe, expect, it } from 'vitest';
import {
  ACTIVE_SESSION_STORAGE_KEY,
  loadActiveSessionId,
  persistActiveSessionId,
} from '../src/sessionPersistence.js';

describe('session persistence', () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it('persists and restores the selected session id', () => {
    persistActiveSessionId('research-notes');

    expect(localStorage.getItem(ACTIVE_SESSION_STORAGE_KEY)).toBe('research-notes');
    expect(loadActiveSessionId()).toBe('research-notes');
  });

  it('clears the selected session id when given an empty value', () => {
    localStorage.setItem(ACTIVE_SESSION_STORAGE_KEY, 'research-notes');

    persistActiveSessionId('  ');

    expect(localStorage.getItem(ACTIVE_SESSION_STORAGE_KEY)).toBeNull();
    expect(loadActiveSessionId()).toBe('');
  });
});
