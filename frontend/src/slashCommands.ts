import { canSendWhileBusy } from './utils.js';

export interface SlashCommandSpec {
  command: string;
  args?: string;
  description: string;
  keywords?: string[];
}

export interface SlashCommandMenuState {
  query: string;
  exactMatch: boolean;
  suggestions: SlashCommandSpec[];
}

export const SLASH_COMMANDS: SlashCommandSpec[] = [
  {
    command: '/new',
    description: 'Compress conversation to memory and clear the current context.',
    keywords: ['reset', 'conversation', 'memory'],
  },
  {
    command: '/status',
    description: 'Show the current session status.',
    keywords: ['model', 'runtime', 'context'],
  },
  {
    command: '/system-prompt',
    description: 'Show the current system prompt and estimated token cost.',
    keywords: ['prompt', 'tokens'],
  },
  {
    command: '/mcp',
    args: '[refresh]',
    description: 'Show MCP server status or refresh the cache.',
    keywords: ['tools', 'server', 'cache'],
  },
  {
    command: '/usage',
    description: 'Show session token usage.',
    keywords: ['tokens', 'cost'],
  },
  {
    command: '/model',
    args: '[name]',
    description: 'List available models or switch the current session model.',
    keywords: ['provider', 'switch'],
  },
  {
    command: '/switch',
    args: '<id>',
    description: 'Switch to or create a session.',
    keywords: ['session', 'workspace'],
  },
  {
    command: '/sessions',
    description: 'List saved sessions.',
    keywords: ['session', 'list'],
  },
  {
    command: '/delete',
    args: '<id>',
    description: 'Delete a non-current session.',
    keywords: ['session', 'remove'],
  },
  {
    command: '/think',
    args: '[level]',
    description: 'Set thinking mode: auto, off, minimal, low, medium, high, xhigh, or max.',
    keywords: ['reasoning', 'mode'],
  },
  {
    command: '/react',
    args: '[on|off]',
    description: 'Toggle ReAct phase visibility.',
    keywords: ['timeline', 'debug'],
  },
  {
    command: '/tool',
    args: '[on|off]',
    description: 'Toggle tool card visibility.',
    keywords: ['panels', 'tools'],
  },
  {
    command: '/reasoning',
    args: '[on|off]',
    description: 'Toggle reasoning panel visibility.',
    keywords: ['thinking', 'panels'],
  },
  {
    command: '/stop',
    description: 'Stop the running agent.',
    keywords: ['cancel', 'interrupt'],
  },
  {
    command: '/skills',
    description: 'List available tools and installed skills.',
    keywords: ['tools', 'plugins'],
  },
  {
    command: '/skills-system',
    args: '[install|uninstall <pattern>]',
    description: 'Show system skill status or install and uninstall built-in skills.',
    keywords: ['skills', 'system'],
  },
  {
    command: '/skills-global',
    description: 'List global skills.',
    keywords: ['skills', 'global'],
  },
  {
    command: '/skills-session',
    description: 'List session-local skills.',
    keywords: ['skills', 'session'],
  },
  {
    command: '/agents',
    description: 'List discovered sub-agents and their effective tools.',
    keywords: ['subagents', 'delegation'],
  },
  {
    command: '/clear',
    description: 'Clear messages and the current session todos.',
    keywords: ['reset', 'todos', 'chat'],
  },
  {
    command: '/memory',
    args: '[stats|debug]',
    description: 'Show structured memory status or updater diagnostics.',
    keywords: ['memory', 'debug'],
  },
  {
    command: '/reflection',
    args: '[today|yesterday|list]',
    description: 'Show daily reflection status and reflection entries.',
    keywords: ['reflection', 'daily'],
  },
  {
    command: '/help',
    description: 'Show command help.',
    keywords: ['commands', 'docs'],
  },
];

function extractSlashQuery(value: string): string | null {
  const trimmedStart = String(value ?? '').trimStart();
  if (!trimmedStart.startsWith('/')) return null;
  if (!/^\/\S*$/.test(trimmedStart)) return null;
  return trimmedStart.slice(1);
}

function commandName(spec: SlashCommandSpec): string {
  return spec.command.slice(1).toLowerCase();
}

function commandScore(spec: SlashCommandSpec, query: string): number {
  if (!query) return 0;
  const name = commandName(spec);
  if (name === query) return 4000 - name.length;
  if (name.startsWith(query)) return 3000 - name.length;
  const nameIndex = name.indexOf(query);
  if (nameIndex >= 0) return 2000 - nameIndex * 10 - name.length;
  const keywordIndex = (spec.keywords || []).findIndex((keyword) => keyword.includes(query));
  if (keywordIndex >= 0) return 1000 - keywordIndex * 10;
  return Number.NEGATIVE_INFINITY;
}

export function getSlashCommandMenuState(value: string): SlashCommandMenuState | null {
  const rawQuery = extractSlashQuery(value);
  if (rawQuery == null) return null;
  const query = rawQuery.toLowerCase();

  const suggestions = SLASH_COMMANDS.map((spec, index) => ({
    spec,
    index,
    score: commandScore(spec, query),
  }))
    .filter((entry) => Number.isFinite(entry.score))
    .sort((left, right) => {
      if (right.score !== left.score) return right.score - left.score;
      return left.index - right.index;
    })
    .map((entry) => entry.spec);

  return {
    query,
    exactMatch: suggestions.some((spec) => spec.command.slice(1) === rawQuery),
    suggestions,
  };
}

export function buildSlashCommandInput(currentValue: string, spec: SlashCommandSpec): string {
  const leadingWhitespace = String(currentValue ?? '').match(/^\s*/)?.[0] ?? '';
  return `${leadingWhitespace}${spec.command}${spec.args ? ' ' : ''}`;
}

export function normalizeSlashCommandText(value: string): string {
  const raw = String(value ?? '');
  if (!raw.startsWith('/')) return raw;

  const firstWhitespaceIndex = raw.search(/\s/);
  const command = firstWhitespaceIndex >= 0 ? raw.slice(0, firstWhitespaceIndex) : raw;
  const rest = firstWhitespaceIndex >= 0 ? raw.slice(firstWhitespaceIndex) : '';
  const lowerCommand = command.toLowerCase();
  const canonicalCommand =
    SLASH_COMMANDS.find((spec) => spec.command.toLowerCase() === lowerCommand)?.command || lowerCommand;

  return `${canonicalCommand}${rest}`;
}

export function isBusyAllowedSlashCommand(spec: SlashCommandSpec): boolean {
  return canSendWhileBusy(spec.command);
}
