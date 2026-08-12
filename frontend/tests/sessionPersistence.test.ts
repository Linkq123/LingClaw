import { beforeEach, describe, expect, it } from 'vitest';
import {
  ACTIVE_SESSION_STORAGE_KEY,
  loadActiveSessionId,
  persistActiveSessionId,
  resolveRestoredActiveSessionId,
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

  it('restores a persisted session that is present in the server list', () => {
    expect(
      resolveRestoredActiveSessionId(
        ' research-notes ',
        [{ id: 'main' }, { id: 'research-notes' }],
        true,
      ),
    ).toBe('research-notes');
  });

  it('restores the canonical server id for a Windows case alias', () => {
    expect(
      resolveRestoredActiveSessionId(
        'research-notes',
        [{ id: 'main' }, { id: 'Research-Notes' }],
        false,
      ),
    ).toBe('Research-Notes');
  });

  it('keeps case-distinct ids separate under case-sensitive server semantics', () => {
    expect(
      resolveRestoredActiveSessionId(
        'research-notes',
        [{ id: 'main' }, { id: 'Research-Notes' }],
        true,
      ),
    ).toBe('main');
  });

  it('prefers an exact match before a Windows case alias', () => {
    expect(
      resolveRestoredActiveSessionId(
        'research-notes',
        [{ id: 'Research-Notes' }, { id: 'research-notes' }],
        false,
      ),
    ).toBe('research-notes');
  });

  it('falls back to main when the persisted session is missing or blank', () => {
    const sessions = [{ id: 'main' }, { id: 'research-notes' }];

    expect(resolveRestoredActiveSessionId('ghost-session', sessions, false)).toBe('main');
    expect(resolveRestoredActiveSessionId('  ', sessions, false)).toBe('main');
  });

  it('falls back to main when the server session list could not be loaded', () => {
    expect(resolveRestoredActiveSessionId('research-notes', null, false)).toBe('main');
  });
});
