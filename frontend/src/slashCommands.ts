import { canSendWhileBusy } from './utils.js';
import { tr } from './i18n.js';

export interface SlashCommandSpec {
  command: string;
  args?: string;
  description: () => string;
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
    description: () => tr('slash.new'),
    keywords: ['reset', 'conversation', 'memory'],
  },
  {
    command: '/status',
    description: () => tr('slash.status'),
    keywords: ['model', 'runtime', 'context'],
  },
  {
    command: '/system-prompt',
    description: () => tr('slash.systemPrompt'),
    keywords: ['prompt', 'tokens'],
  },
  {
    command: '/mcp',
    args: '[refresh]',
    description: () => tr('slash.mcp'),
    keywords: ['tools', 'server', 'cache'],
  },
  {
    command: '/usage',
    description: () => tr('slash.usage'),
    keywords: ['tokens', 'cost'],
  },
  {
    command: '/model',
    args: '[name]',
    description: () => tr('slash.model'),
    keywords: ['provider', 'switch'],
  },
  {
    command: '/switch',
    args: '<id>',
    description: () => tr('slash.switch'),
    keywords: ['session', 'workspace'],
  },
  {
    command: '/sessions',
    description: () => tr('slash.sessions'),
    keywords: ['session', 'list'],
  },
  {
    command: '/delete',
    args: '<id>',
    description: () => tr('slash.delete'),
    keywords: ['session', 'remove'],
  },
  {
    command: '/think',
    args: '[level]',
    description: () => tr('slash.think'),
    keywords: ['reasoning', 'mode'],
  },
  {
    command: '/react',
    args: '[on|off]',
    description: () => tr('slash.react'),
    keywords: ['timeline', 'debug'],
  },
  {
    command: '/tool',
    args: '[on|off]',
    description: () => tr('slash.tool'),
    keywords: ['panels', 'tools'],
  },
  {
    command: '/reasoning',
    args: '[on|off]',
    description: () => tr('slash.reasoning'),
    keywords: ['thinking', 'panels'],
  },
  {
    command: '/stop',
    description: () => tr('slash.stop'),
    keywords: ['cancel', 'interrupt'],
  },
  {
    command: '/skills',
    description: () => tr('slash.skills'),
    keywords: ['tools', 'plugins'],
  },
  {
    command: '/skills-system',
    args: '[install|uninstall <pattern>]',
    description: () => tr('slash.skillsSystem'),
    keywords: ['skills', 'system'],
  },
  {
    command: '/skills-global',
    description: () => tr('slash.skillsGlobal'),
    keywords: ['skills', 'global'],
  },
  {
    command: '/skills-session',
    description: () => tr('slash.skillsSession'),
    keywords: ['skills', 'session'],
  },
  {
    command: '/agents',
    description: () => tr('slash.agents'),
    keywords: ['subagents', 'delegation'],
  },
  {
    command: '/clear',
    description: () => tr('slash.clear'),
    keywords: ['reset', 'todos', 'chat'],
  },
  {
    command: '/memory',
    args: '[stats|debug]',
    description: () => tr('slash.memory'),
    keywords: ['memory', 'debug'],
  },
  {
    command: '/reflection',
    args: '[today|yesterday|list]',
    description: () => tr('slash.reflection'),
    keywords: ['reflection', 'daily'],
  },
  {
    command: '/help',
    description: () => tr('slash.help'),
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
    SLASH_COMMANDS.find((spec) => spec.command.toLowerCase() === lowerCommand)?.command ||
    lowerCommand;

  return `${canonicalCommand}${rest}`;
}

export function isBusyAllowedSlashCommand(spec: SlashCommandSpec): boolean {
  return canSendWhileBusy(spec.command);
}
