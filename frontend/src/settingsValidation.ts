// Pure validation helpers for settings forms — ported from settings.ts

export function isPlainObject(v: unknown): v is Record<string, unknown> {
  return v !== null && typeof v === 'object' && !Array.isArray(v);
}

export function isAbsent(v: unknown): boolean {
  return v === undefined || v === null || v === '';
}

export function ensureOptionalString(value: unknown, label: string): void {
  if (!isAbsent(value) && typeof value !== 'string') {
    throw new Error(`${label} must be a string.`);
  }
}

export function ensureOptionalBoolean(value: unknown, label: string): void {
  if (!isAbsent(value) && typeof value !== 'boolean') {
    throw new Error(`${label} must be a boolean.`);
  }
}

export function ensureOptionalInteger(value: unknown, label: string): void {
  if (!isAbsent(value) && (!Number.isInteger(value) || typeof value !== 'number')) {
    throw new Error(`${label} must be an integer.`);
  }
}

export function ensureStringArray(value: unknown, label: string): void {
  if (!Array.isArray(value)) throw new Error(`${label} must be an array.`);
  for (let i = 0; i < value.length; i++) {
    if (typeof value[i] !== 'string') throw new Error(`${label}[${i}] must be a string.`);
  }
}

export function ensureOptionalStringArray(value: unknown, label: string): void {
  if (!isAbsent(value)) ensureStringArray(value, label);
}

export function parseNonNegativeIntegerField(rawValue: string, label: string): number | undefined {
  const trimmed = rawValue.trim();
  if (trimmed === '') return undefined;
  if (!/^\d+$/.test(trimmed)) throw new Error(`${label} must be a non-negative integer.`);
  return parseInt(trimmed, 10);
}

export function parsePositiveIntegerField(rawValue: string, label: string): number | undefined {
  const parsed = parseNonNegativeIntegerField(rawValue, label);
  if (parsed === 0) throw new Error(`${label} must be greater than 0.`);
  return parsed;
}

export function isBuiltinProviderName(name: string): boolean {
  return ['openai', 'anthropic', 'ollama', 'gemini'].includes((name || '').toLowerCase());
}

export function isSupportedProviderApiKind(value: string): boolean {
  return ['openai-completions', 'anthropic', 'ollama', 'gemini'].includes(
    (value || '').trim().toLowerCase(),
  );
}

export function validateProviderName(
  name: string,
  existingProviders: Record<string, unknown> | null = null,
): string {
  const trimmed = name.trim();
  if (!trimmed) throw new Error('Provider name cannot be empty.');
  if (trimmed.includes('/')) throw new Error("Provider name cannot contain '/'.");
  if (/\s/.test(trimmed)) throw new Error('Provider name cannot contain whitespace.');
  if (!/^[A-Za-z0-9._-]+$/.test(trimmed)) {
    throw new Error("Provider name may only contain letters, numbers, '.', '-' or '_'.");
  }
  if (existingProviders && Object.prototype.hasOwnProperty.call(existingProviders, trimmed)) {
    throw new Error(`Provider name '${trimmed}' already exists.`);
  }
  return trimmed;
}

export function validateMcpCwdValue(value: string, fieldLabel = 'MCP cwd'): void {
  const raw = String(value || '').trim();
  if (!raw) return;
  const parts = raw.split(/[\\/]+/).filter(Boolean);
  if (parts.includes('.lingclaw-bootstrap')) {
    throw new Error(`${fieldLabel} targets protected internal workspace data.`);
  }
  const isAbsolutePath = /^[a-zA-Z]:[\\/]/.test(raw) || raw.startsWith('/') || raw.startsWith('\\');
  if (isAbsolutePath) return;
  let depth = 0;
  for (const part of parts) {
    if (part === '.') continue;
    if (part === '..') {
      if (depth === 0) throw new Error(`${fieldLabel} must stay inside the session workspace.`);
      depth -= 1;
      continue;
    }
    depth += 1;
  }
}

export function validateModelsConfigDraftShape(parsed: unknown): void {
  if (!isPlainObject(parsed)) throw new Error('Models JSON must be an object.');
  if (parsed['providers'] !== undefined && !isPlainObject(parsed['providers'])) {
    throw new Error('Models JSON field "providers" must be an object.');
  }
  for (const [name, provider] of Object.entries(
    (parsed['providers'] as Record<string, unknown>) || {},
  )) {
    validateProviderName(name);
    if (!isPlainObject(provider))
      throw new Error(`Models JSON field "providers.${name}" must be an object.`);
    const p = provider as Record<string, unknown>;
    ensureOptionalString(p['api'], `Models JSON field "providers.${name}.api"`);
    if (!isAbsent(p['api']) && !isSupportedProviderApiKind(p['api'] as string)) {
      throw new Error(
        `Models JSON field "providers.${name}.api" must be one of: openai-completions, anthropic, ollama, gemini.`,
      );
    }
    if (typeof p['baseUrl'] !== 'string') {
      throw new Error(`Models JSON field "providers.${name}.baseUrl" must be a string.`);
    }
    if ((p['baseUrl'] as string).trim() === '') {
      throw new Error(`Models JSON field "providers.${name}.baseUrl" cannot be empty.`);
    }
    if (typeof p['apiKey'] !== 'string') {
      throw new Error(`Models JSON field "providers.${name}.apiKey" must be a string.`);
    }
    if (p['models'] !== undefined && !Array.isArray(p['models'])) {
      throw new Error(`Models JSON field "providers.${name}.models" must be an array.`);
    }
    if (Array.isArray(p['models'])) {
      (p['models'] as unknown[]).forEach((model, index) => {
        if (!isPlainObject(model))
          throw new Error(
            `Models JSON field "providers.${name}.models[${index}]" must be an object.`,
          );
        const m = model as Record<string, unknown>;
        if (typeof m['id'] !== 'string' || (m['id'] as string).trim() === '') {
          throw new Error(
            `Models JSON field "providers.${name}.models[${index}].id" must be a non-empty string.`,
          );
        }
        ensureOptionalString(
          m['name'],
          `Models JSON field "providers.${name}.models[${index}].name"`,
        );
        ensureOptionalBoolean(
          m['reasoning'],
          `Models JSON field "providers.${name}.models[${index}].reasoning"`,
        );
        ensureOptionalStringArray(
          m['input'],
          `Models JSON field "providers.${name}.models[${index}].input"`,
        );
        ensureOptionalInteger(
          m['contextWindow'],
          `Models JSON field "providers.${name}.models[${index}].contextWindow"`,
        );
        ensureOptionalInteger(
          m['maxTokens'],
          `Models JSON field "providers.${name}.models[${index}].maxTokens"`,
        );
      });
    }
  }
}

export function validateMcpConfigDraftShape(parsed: unknown): void {
  if (!isPlainObject(parsed)) throw new Error('MCP JSON must be an object.');
  for (const [name, server] of Object.entries(parsed as Record<string, unknown>)) {
    if (!isPlainObject(server)) throw new Error(`MCP JSON field "${name}" must be an object.`);
    const s = server as Record<string, unknown>;
    ensureOptionalString(s['command'], `MCP JSON field "${name}.command"`);
    if (!isAbsent(s['command']) && (s['command'] as string).trim() === '') {
      throw new Error(`MCP JSON field "${name}.command" cannot be empty.`);
    }
    ensureOptionalString(s['cwd'], `MCP JSON field "${name}.cwd"`);
    if (!isAbsent(s['cwd']))
      validateMcpCwdValue(s['cwd'] as string, `MCP JSON field "${name}.cwd"`);
    if (!isAbsent(s['timeoutSecs']) && s['timeoutSecs'] === 0) {
      throw new Error(`MCP JSON field "${name}.timeoutSecs" must be greater than 0.`);
    }
    ensureOptionalInteger(s['timeoutSecs'], `MCP JSON field "${name}.timeoutSecs"`);
    ensureOptionalBoolean(s['enabled'], `MCP JSON field "${name}.enabled"`);
    if (!isAbsent(s['args'])) ensureStringArray(s['args'], `MCP JSON field "${name}.args"`);
    if (!isAbsent(s['env'])) {
      if (!isPlainObject(s['env']))
        throw new Error(`MCP JSON field "${name}.env" must be an object.`);
      for (const [k, v] of Object.entries(s['env'] as Record<string, unknown>)) {
        if (typeof v !== 'string')
          throw new Error(`MCP JSON field "${name}.env.${k}" must be a string.`);
      }
    }
  }
}

export function buildModelOptions(
  providers: Record<string, { models?: Array<{ id: string }> }>,
): string[] {
  const options: string[] = [];
  for (const [name, p] of Object.entries(providers)) {
    for (const m of p.models || []) {
      if (!m || typeof m.id !== 'string' || m.id.trim() === '') continue;
      options.push(`${name}/${m.id}`);
    }
  }
  return options.sort();
}
