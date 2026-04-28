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

describe('history window helpers', () => {
  it('uses the preferred start when no tool result crosses the boundary', () => {
    const messages = [user('a'), user('b'), user('c'), user('d')];

    expect(findHistoryRenderStart(messages, 2)).toBe(2);
  });

  it('expands the window to include a matching tool call before the boundary', () => {
    const messages = [user('a'), toolCall('x'), user('b'), toolResult('x'), user('c')];

    expect(findHistoryRenderStart(messages, 3)).toBe(1);
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

    expect(remaining.map((m) => m.content)).toEqual(['a']);
    expect(chunk.map((m) => m.role)).toEqual(['tool_call', 'user', 'user', 'tool_result']);
  });
});
