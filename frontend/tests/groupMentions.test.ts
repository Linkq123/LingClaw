import { describe, expect, it } from 'vitest';

import {
  decorateGroupMentions,
  filterGroupMentionMembers,
  findGroupMentionQuery,
  insertGroupMention,
  mentionedGroupTargets,
} from '../src/groupMentions.js';

describe('group mentions', () => {
  it('extracts protocol ids with punctuation and expands @all', () => {
    expect(
      mentionedGroupTargets('(@worker-a), please ask @worker-b.', ['worker-a', 'worker-b']),
    ).toEqual(['worker-a', 'worker-b']);
    expect(mentionedGroupTargets('notify @all', ['worker-a', 'worker-b'])).toEqual([
      'worker-a',
      'worker-b',
    ]);
    expect(mentionedGroupTargets('你好，@worker-a，请检查。', ['worker-a', 'worker-b'])).toEqual([
      'worker-a',
    ]);
    expect(mentionedGroupTargets('**@worker-b**', ['worker-a', 'worker-b'])).toEqual(['worker-b']);
    expect(mentionedGroupTargets('ask(@worker-a)', ['worker-a', 'worker-b'])).toEqual([]);
  });

  it('finds and replaces the mention at the caret without writing the display name', () => {
    const value = 'Ask @前端 about this later';
    const caret = value.indexOf(' about');
    const query = findGroupMentionQuery(value, caret);

    expect(query).toEqual({ start: 4, end: caret, query: '前端' });
    expect(insertGroupMention(value, query!, 'worker-a')).toEqual({
      value: 'Ask @worker-a about this later',
      cursor: 13,
    });
  });

  it('keeps trailing punctuation attached while preserving a valid protocol boundary', () => {
    const value = 'Ask @front,please continue';
    const caret = value.indexOf(',');
    const query = findGroupMentionQuery(value, caret);

    expect(insertGroupMention(value, query!, 'worker-a')).toEqual({
      value: 'Ask @worker-a, please continue',
      cursor: 13,
    });

    const chineseValue = '请问 @前端，继续';
    const chineseCaret = chineseValue.indexOf('，');
    expect(
      insertGroupMention(
        chineseValue,
        findGroupMentionQuery(chineseValue, chineseCaret)!,
        'worker-a',
      ),
    ).toEqual({ value: '请问 @worker-a，继续', cursor: 12 });
  });

  it('opens a query after full-width Chinese punctuation', () => {
    expect(findGroupMentionQuery('请问，@', 4)).toEqual({ start: 3, end: 4, query: '' });
  });

  it('filters by either display name or id while keeping duplicate names distinct', () => {
    const members = [
      { id: 'worker-a', name: '代码审查', role: 'member' },
      { id: 'worker-b', name: '代码审查', role: 'admin' },
    ];
    expect(filterGroupMentionMembers('代码', ['worker-a', 'worker-b'], members, '全部')).toEqual(
      members,
    );
    expect(
      filterGroupMentionMembers('worker-b', ['worker-a', 'worker-b'], members, '全部'),
    ).toEqual([members[1]]);
  });

  it('decorates known mentions but leaves code, links, and unknown ids untouched', () => {
    const container = document.createElement('div');
    container.innerHTML =
      '<p>Hello @worker-a and @missing</p><code>@worker-a</code><a href="#">@worker-a</a>';

    decorateGroupMentions(container, ['worker-a'], () => '<Ops> **Team**', 'All');

    const mentions = container.querySelectorAll<HTMLElement>('.group-mention');
    expect(mentions).toHaveLength(1);
    expect(mentions[0].textContent).toBe('@<Ops> **Team**');
    expect(mentions[0].dataset.sessionId).toBe('worker-a');
    expect(container.querySelector('code')?.textContent).toBe('@worker-a');
    expect(container.querySelector('a')?.textContent).toBe('@worker-a');
    expect(container.textContent).toContain('@missing');
    expect(container.querySelector('strong')).toBeNull();
  });

  it('decorates only tokens accepted by the backend mention protocol', () => {
    const container = document.createElement('div');
    container.textContent = '**@worker-a** ask(@worker-a) ：@worker-a';

    decorateGroupMentions(container, ['worker-a'], () => 'Worker A', 'All');

    expect(container.querySelectorAll('.group-mention')).toHaveLength(2);
    expect(container.textContent).toContain('ask(@worker-a)');
  });
});
