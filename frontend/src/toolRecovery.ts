function canonicalJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`;
  if (value && typeof value === 'object') {
    return `{${Object.entries(value as Record<string, unknown>)
      .sort(([left], [right]) => (left < right ? -1 : left > right ? 1 : 0))
      .map(([key, item]) => `${JSON.stringify(key)}:${canonicalJson(item)}`)
      .join(',')}}`;
  }
  return JSON.stringify(value);
}

// Keep this conservative relation aligned with src/tool_recovery.rs. A matching
// path alone cannot prove recovery of a different search, command or action.
export function canonicalToolRetryKey(name: string, args: string): string {
  const normalizedName = String(name || '').trim();
  try {
    const parsed: unknown = JSON.parse(args);
    if (
      normalizedName === 'read_file' &&
      parsed &&
      typeof parsed === 'object' &&
      !Array.isArray(parsed)
    ) {
      const read = parsed as Record<string, unknown>;
      delete read.start_line;
      delete read.end_line;
    }
    return `${normalizedName}:args:${canonicalJson(parsed)}`;
  } catch {
    return `${normalizedName}:raw:${args}`;
  }
}
