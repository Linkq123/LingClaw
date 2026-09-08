import { describe, expect, it } from 'vitest';
import {
  findHistoryRenderStart,
  historyRunScopes,
  splitHistoryLoadChunk,
} from '../src/historyWindow.js';
import type { HistoryMessage } from '../src/types.js';

const user = (content: string): HistoryMessage => ({ role: 'user', content });
const toolCall = (id: string): HistoryMessage => ({
  role: 'tool_call',
  id,
  name: 'read_file',
  content: '',
  arguments: '{}',
});
const toolResult = (id: string): HistoryMessage => ({
  role: 'tool_result',
  id,
  content: '',
  result: 'ok',
});
const assistant = (content: string, thinking = ''): HistoryMessage => ({
  role: 'assistant',
  content,
  thinking,
});

describe('history window helpers', () => {
  it('uses the preferred user boundary when no tool result crosses it', () => {
    const messages = [user('a'), user('b'), user('c'), user('d')];

    expect(findHistoryRenderStart(messages, 2)).toBe(2);
  });

  it('expands the window to include a matching tool call before the boundary', () => {
    const messages = [user('a'), toolCall('x'), user('b'), toolResult('x'), user('c')];

    expect(findHistoryRenderStart(messages, 3)).toBe(0);
  });

  it('splits the newest chunk and preserves older remaining history', () => {
    const messages = [user('a'), user('b'), user('c'), user('d'), user('e')];

    const { remaining, chunk } = splitHistoryLoadChunk(messages, 2);

    expect(remaining.map((m) => m.content)).toEqual(['a', 'b', 'c']);
    expect(chunk.map((m) => m.content)).toEqual(['d', 'e']);
  });

  it('expands a chunk so tool_result never renders without its call', () => {
    const messages = [user('a'), toolCall('x'), user('b'), user('c'), toolResult('x')];

    const { remaining, chunk } = splitHistoryLoadChunk(messages, 2);

    expect(remaining).toEqual([]);
    expect(chunk.map((m) => m.role)).toEqual(['user', 'tool_call', 'user', 'user', 'tool_result']);
  });

  it('accepts an end-of-list preferred boundary without reading past the array', () => {
    const messages = [user('a'), assistant('answer')];

    expect(findHistoryRenderStart(messages, messages.length)).toBe(messages.length);
  });

  it('keeps every ReAct cycle from one user turn in the same history chunk', () => {
    const messages = [
      user('first'),
      assistant('first answer'),
      user('second'),
      assistant('', 'cycle one'),
      toolCall('a'),
      toolResult('a'),
      assistant('', 'cycle two'),
      toolCall('b'),
      toolResult('b'),
      assistant('second answer'),
    ];

    const { remaining, chunk } = splitHistoryLoadChunk(messages, 3);

    expect(remaining.map((message) => message.content)).toEqual(['first', 'first answer']);
    expect(chunk[0]).toMatchObject({ role: 'user', content: 'second' });
    expect(chunk.filter((message) => message.role === 'tool_call')).toHaveLength(2);
    expect(chunk.at(-1)).toMatchObject({ role: 'assistant', content: 'second answer' });
  });

  it('keeps same-index assistant/tool rows and an intervention inside the terminal interval', () => {
    const messages: HistoryMessage[] = [
      { ...user('previous'), message_index: 0 },
      { ...assistant('previous answer'), message_index: 1 },
      { ...user('current'), message_index: 2 },
      { ...toolCall('first'), message_index: 3 },
      { ...toolResult('first'), message_index: 4 },
      { ...assistant('I will adjust the read window'), message_index: 5 },
      { ...toolCall('retry'), message_index: 5 },
      { ...user('Include the heading'), message_index: 6 },
      {
        ...toolResult('retry'),
        message_index: 7,
        run_outcomes: [
          {
            run_id: 'current',
            status: 'completed',
            phase: 'finish',
            reason: 'completed',
            start_message_index: 2,
            end_message_index: 7,
          },
        ],
      },
    ];
    expect(historyRunScopes(messages)).toEqual([{ start: 2, end: 8 }]);
    for (const boundary of [5, 6, 7, 8]) expect(findHistoryRenderStart(messages, boundary)).toBe(2);
    expect(splitHistoryLoadChunk(messages, 2).remaining).toEqual(messages.slice(0, 2));
  });

  it('does not reclaim a previous Plan run when a later revision shares its original user anchor', () => {
    const messages: HistoryMessage[] = [
      { ...user('plan'), message_index: 0 },
      { ...toolCall('plan-one'), message_index: 1 },
      {
        ...toolResult('plan-one'),
        message_index: 2,
        run_outcomes: [
          {
            run_id: 'plan-one',
            status: 'waiting_user',
            phase: 'needs_input',
            reason: 'needs_input',
            start_message_index: 0,
            end_message_index: 2,
          },
        ],
      },
      { ...assistant('Revising the plan'), message_index: 3 },
      { ...toolCall('plan-two'), message_index: 4 },
      {
        ...toolResult('plan-two'),
        message_index: 5,
        run_outcomes: [
          {
            run_id: 'plan-two',
            status: 'completed',
            phase: 'finish',
            reason: 'completed',
            start_message_index: 0,
            end_message_index: 5,
          },
        ],
      },
    ];
    expect(historyRunScopes(messages)).toEqual([
      { start: 0, end: 2 },
      { start: 3, end: 5 },
    ]);
    expect(findHistoryRenderStart(messages, 4)).toBe(3);
    expect(splitHistoryLoadChunk(messages, 2)).toEqual({
      remaining: messages.slice(0, 3),
      chunk: messages.slice(3),
    });
  });

  it('ignores a terminal outside its stored interval and retains the legacy user boundary', () => {
    const messages: HistoryMessage[] = [
      { ...user('legacy'), message_index: 0 },
      { ...assistant('middle'), message_index: 1 },
      { ...toolCall('legacy-tool'), message_index: 2 },
      {
        ...toolResult('legacy-tool'),
        message_index: 3,
        run_outcomes: [
          {
            run_id: 'invalid',
            status: 'completed',
            phase: 'finish',
            reason: 'completed',
            start_message_index: 4,
            end_message_index: 5,
          },
        ],
      },
    ];
    expect(historyRunScopes(messages)).toEqual([]);
    expect(findHistoryRenderStart(messages, 2)).toBe(0);
  });
});
