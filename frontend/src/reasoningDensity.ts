export type ReasoningDensity = 'summary' | 'normal' | 'verbose';

export const REASONING_DENSITY_STORAGE_KEY = 'lingclaw.reasoningDensity';

const DENSITIES: readonly ReasoningDensity[] = ['summary', 'normal', 'verbose'];

export function reasoningDensityForNavigationKey(
  current: ReasoningDensity,
  key: string,
): ReasoningDensity | null {
  if (key === 'Home') return DENSITIES[0];
  if (key === 'End') return DENSITIES[DENSITIES.length - 1];
  const delta =
    key === 'ArrowRight' || key === 'ArrowDown'
      ? 1
      : key === 'ArrowLeft' || key === 'ArrowUp'
        ? -1
        : 0;
  if (!delta) return null;
  const index = DENSITIES.indexOf(current);
  return DENSITIES[(index + delta + DENSITIES.length) % DENSITIES.length];
}

export function normalizeReasoningDensity(value: unknown): ReasoningDensity {
  return DENSITIES.includes(value as ReasoningDensity) ? (value as ReasoningDensity) : 'summary';
}

export function loadReasoningDensity(storage?: Pick<Storage, 'getItem'> | null): ReasoningDensity {
  try {
    const resolvedStorage = storage === undefined ? globalThis.localStorage : storage;
    return normalizeReasoningDensity(resolvedStorage?.getItem(REASONING_DENSITY_STORAGE_KEY));
  } catch {
    return 'summary';
  }
}

export function persistReasoningDensity(
  density: ReasoningDensity,
  storage?: Pick<Storage, 'setItem'> | null,
): void {
  try {
    const resolvedStorage = storage === undefined ? globalThis.localStorage : storage;
    resolvedStorage?.setItem(REASONING_DENSITY_STORAGE_KEY, density);
  } catch {
    // A blocked localStorage leaves the preference session-local.
  }
}
