import { describe, expect, it } from 'vitest';

import {
  SLASH_COMMANDS,
  buildSlashCommandInput,
  getSlashCommandMenuState,
  isBusyAllowedSlashCommand,
} from '../src/slashCommands.js';

describe('slashCommands', () => {
  it('shows the documented command list when the composer starts with a slash', () => {
    const menuState = getSlashCommandMenuState('/');

    expect(menuState?.suggestions.length).toBe(SLASH_COMMANDS.length);
    expect(menuState?.suggestions[0]?.command).toBe('/new');
    expect(menuState?.suggestions.some((spec) => spec.command === '/help')).toBe(true);
  });

  it('filters by prefix and keeps exact matches flagged for send-through', () => {
    const menuState = getSlashCommandMenuState('/skills');

    expect(menuState?.exactMatch).toBe(true);
    expect(menuState?.suggestions[0]?.command).toBe('/skills');
    expect(menuState?.suggestions.slice(1, 4).map((spec) => spec.command).sort()).toEqual([
      '/skills-global',
      '/skills-session',
      '/skills-system',
    ]);
  });

  it('does not treat mixed-case input as an exact match', () => {
    const menuState = getSlashCommandMenuState('/HELP');

    expect(menuState?.exactMatch).toBe(false);
    expect(menuState?.suggestions[0]?.command).toBe('/help');
  });

  it('hides the menu after command arguments begin', () => {
    expect(getSlashCommandMenuState('/help now')).toBeNull();
    expect(getSlashCommandMenuState('hello /help')).toBeNull();
  });

  it('builds inserted command text with preserved indentation and optional trailing space', () => {
    const helpSpec = SLASH_COMMANDS.find((spec) => spec.command === '/help');
    const modelSpec = SLASH_COMMANDS.find((spec) => spec.command === '/model');

    expect(helpSpec).toBeDefined();
    expect(modelSpec).toBeDefined();
    expect(buildSlashCommandInput('   /he', helpSpec!)).toBe('   /help');
    expect(buildSlashCommandInput('/mo', modelSpec!)).toBe('/model ');
  });

  it('marks the live-safe commands that can still be sent while busy', () => {
    const liveCommands = SLASH_COMMANDS.filter((spec) => isBusyAllowedSlashCommand(spec)).map(
      (spec) => spec.command,
    );

    expect(liveCommands).toEqual(['/tool', '/reasoning', '/stop']);
  });
});
