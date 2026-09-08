import { tr } from './i18n.js';

const diagnosticCodes = [
  'provider_authentication',
  'provider_rate_limited',
  'provider_unavailable',
  'provider_connection',
  'provider_request_rejected',
  'provider_response_invalid',
  'model_configuration',
  'context_budget_exceeded',
] as const;

export interface RunDiagnostic {
  code: (typeof diagnosticCodes)[number];
}

export function normalizeRunDiagnostic(value: unknown): RunDiagnostic | undefined {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return undefined;
  const code = (value as { code?: unknown }).code;
  if (typeof code !== 'string' || !diagnosticCodes.includes(code as RunDiagnostic['code']))
    return undefined;
  return { code: code as RunDiagnostic['code'] };
}

export function runDiagnosticSummaryKey(diagnostic: RunDiagnostic): string {
  return `execution.diagnostic.${diagnostic.code}.summary`;
}

export function runDiagnosticDetail(diagnostic: RunDiagnostic): string {
  return tr(`execution.diagnostic.${diagnostic.code}.detail`);
}

export function runDiagnosticHasModelsRecovery(diagnostic: RunDiagnostic): boolean {
  return diagnostic.code !== 'provider_rate_limited' && diagnostic.code !== 'provider_unavailable';
}
