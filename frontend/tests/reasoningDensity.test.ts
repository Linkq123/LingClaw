import { beforeEach, describe, expect, it } from 'vitest';
import {
  loadReasoningDensity,
  normalizeReasoningDensity,
  persistReasoningDensity,
  REASONING_DENSITY_STORAGE_KEY,
  reasoningDensityForNavigationKey,
} from '../src/reasoningDensity.js';

describe('reasoning density preference', () => {
  beforeEach(() => localStorage.clear());

  it('defaults to Summary and normalizes unknown stored values', () => {
    expect(loadReasoningDensity()).toBe('summary');
    localStorage.setItem(REASONING_DENSITY_STORAGE_KEY, 'raw');
    expect(loadReasoningDensity()).toBe('summary');
    expect(normalizeReasoningDensity('verbose')).toBe('verbose');
  });

  it.each(['summary', 'normal', 'verbose'] as const)('persists the %s density', (density) => {
    persistReasoningDensity(density);
    expect(loadReasoningDensity()).toBe(density);
  });

  it('falls back safely when browser storage is blocked', () => {
    const blocked = {
      getItem: () => {
        throw new Error('blocked');
      },
      setItem: () => {
        throw new Error('blocked');
      },
    };
    expect(loadReasoningDensity(blocked)).toBe('summary');
    expect(() => persistReasoningDensity('normal', blocked)).not.toThrow();
  });

  it('treats null as intentionally unavailable storage', () => {
    localStorage.setItem(REASONING_DENSITY_STORAGE_KEY, 'verbose');
    expect(loadReasoningDensity(null)).toBe('summary');
    expect(() => persistReasoningDensity('normal', null)).not.toThrow();
    expect(localStorage.getItem(REASONING_DENSITY_STORAGE_KEY)).toBe('verbose');
  });

  it('uses explicitly supplied storage without touching the global property', () => {
    const values = new Map<string, string>([[REASONING_DENSITY_STORAGE_KEY, 'normal']]);
    const storage = {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => values.set(key, value),
    };
    const original = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');
    Object.defineProperty(globalThis, 'localStorage', {
      configurable: true,
      get: () => {
        throw new DOMException('blocked', 'SecurityError');
      },
    });

    try {
      expect(loadReasoningDensity(storage)).toBe('normal');
      expect(() => persistReasoningDensity('verbose', storage)).not.toThrow();
      expect(values.get(REASONING_DENSITY_STORAGE_KEY)).toBe('verbose');
      expect(loadReasoningDensity(undefined)).toBe('summary');
      expect(() => persistReasoningDensity('normal', undefined)).not.toThrow();
    } finally {
      if (original) {
        Object.defineProperty(globalThis, 'localStorage', original);
      } else {
        Reflect.deleteProperty(globalThis, 'localStorage');
      }
    }
  });

  it('falls back safely when the browser storage property itself is blocked', () => {
    const original = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');
    Object.defineProperty(globalThis, 'localStorage', {
      configurable: true,
      get: () => {
        throw new DOMException('blocked', 'SecurityError');
      },
    });

    try {
      expect(loadReasoningDensity()).toBe('summary');
      expect(() => persistReasoningDensity('normal')).not.toThrow();
    } finally {
      if (original) {
        Object.defineProperty(globalThis, 'localStorage', original);
      } else {
        Reflect.deleteProperty(globalThis, 'localStorage');
      }
    }
  });

  it('implements wrapped arrow and Home/End navigation for the radio group', () => {
    expect(reasoningDensityForNavigationKey('summary', 'ArrowLeft')).toBe('verbose');
    expect(reasoningDensityForNavigationKey('verbose', 'ArrowRight')).toBe('summary');
    expect(reasoningDensityForNavigationKey('normal', 'ArrowDown')).toBe('verbose');
    expect(reasoningDensityForNavigationKey('normal', 'ArrowUp')).toBe('summary');
    expect(reasoningDensityForNavigationKey('verbose', 'Home')).toBe('summary');
    expect(reasoningDensityForNavigationKey('summary', 'End')).toBe('verbose');
    expect(reasoningDensityForNavigationKey('summary', 'Enter')).toBeNull();
  });
});
