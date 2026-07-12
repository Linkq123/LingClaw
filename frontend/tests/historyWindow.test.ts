import { describe, expect, it } from 'vitest';
import { findHistoryRenderStart, splitHistoryLoadChunk } from '../src/historyWindow.js';
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
});
