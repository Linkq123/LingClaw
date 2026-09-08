import { describe, expect, it } from 'vitest';
import { normalizeRunDiagnostic, runDiagnosticHasModelsRecovery } from '../src/runDiagnostics.js';

describe('closed run diagnostic protocol', () => {
  it('rejects unknown codes and retains no raw response fields', () => {
    for (const value of [
      null,
      [],
      'provider_authentication',
      { code: '__proto__' },
      { code: '<img src=x onerror=alert(1)>' },
    ]) {
      expect(normalizeRunDiagnostic(value)).toBeUndefined();
    }
    const body = '<html>token=private https://user:secret@example.test</html>'.repeat(10000);
    expect(
      normalizeRunDiagnostic({
        code: 'provider_authentication',
        body,
        headers: { authorization: 'secret' },
      }),
    ).toEqual({ code: 'provider_authentication' });
  });

  it('offers Models only when configuration inspection can help', () => {
    expect(runDiagnosticHasModelsRecovery({ code: 'provider_authentication' })).toBe(true);
    expect(runDiagnosticHasModelsRecovery({ code: 'model_configuration' })).toBe(true);
    expect(runDiagnosticHasModelsRecovery({ code: 'provider_rate_limited' })).toBe(false);
    expect(runDiagnosticHasModelsRecovery({ code: 'provider_unavailable' })).toBe(false);
  });
});
