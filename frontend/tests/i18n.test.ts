import { beforeEach, describe, expect, it, vi } from 'vitest';
import { SLASH_COMMANDS } from '../src/slashCommands.js';
import {
  language,
  setLanguage,
  subscribeLanguageChange,
  toggleLanguage,
  tr,
  translateDom,
} from '../src/i18n.js';

describe('i18n', () => {
  beforeEach(() => {
    localStorage.clear();
    document.body.innerHTML = '';
    setLanguage('en');
  });

  it('translates strings and persists the active language', () => {
    setLanguage('zh-CN');

    expect(language()).toBe('zh-CN');
    expect(tr('common.settings')).toBe('设置');
    expect(tr('welcome.title')).toBe('工作区已就绪');
    expect(tr('welcome.ready')).toBe('输入消息，或使用 / 命令开始。');
    expect(tr('workspace.subtitle')).toBe('本地工作区');
    expect(tr('workspace.viewControls')).toBe('视图控制');
    expect(tr('settings.configChangedWhileLoading')).toBe('加载期间配置发生变化，请重试。');
    expect(document.documentElement.lang).toBe('zh-CN');
    expect(localStorage.getItem('lingclaw.language')).toBe('zh-CN');
  });

  it('updates text and accessibility attributes on marked DOM nodes', () => {
    document.body.innerHTML = `
      <button
        data-i18n="common.settings"
        data-i18n-title="common.languageTitle"
        data-i18n-aria-label="common.settings"
      >Settings</button>
      <textarea data-i18n-placeholder="composer.placeholder"></textarea>
    `;

    setLanguage('zh-CN');
    translateDom();

    const button = document.querySelector('button')!;
    const textarea = document.querySelector('textarea')!;
    expect(button.textContent).toBe('设置');
    expect(button.title).toBe('切换语言');
    expect(button.getAttribute('aria-label')).toBe('设置');
    expect(textarea.getAttribute('placeholder')).toContain('给 LingClaw 发消息');
  });

  it('emits a language-change event when toggled', () => {
    const listener = vi.fn();
    const unsubscribe = subscribeLanguageChange(listener);

    toggleLanguage();

    expect(listener).toHaveBeenCalledTimes(1);
    unsubscribe();
  });

  it('returns slash command descriptions in the current language', () => {
    const helpCommand = SLASH_COMMANDS.find((command) => command.command === '/help')!;

    setLanguage('en');
    expect(helpCommand.description()).toBe('Show command help.');

    setLanguage('zh-CN');
    expect(helpCommand.description()).toBe('显示命令帮助。');
  });
});
