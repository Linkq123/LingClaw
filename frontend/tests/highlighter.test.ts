import { describe, expect, it } from 'vitest';
import hljs from '../src/highlighter.js';

describe('syntax highlighter bundle', () => {
  it('registers common LingClaw languages without the full language bundle', () => {
    for (const language of [
      'typescript',
      'javascript',
      'rust',
      'python',
      'json',
      'yaml',
      'toml',
      'powershell',
      'dockerfile',
    ]) {
      expect(hljs.getLanguage(language), language).toBeDefined();
    }
  });
});
