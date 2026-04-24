import type { HistoryMessage } from './types.js';

export function findHistoryRenderStart(messages: HistoryMessage[], preferredStart: number): number {
  let startIdx = Math.max(0, preferredStart);
  if (startIdx === 0) {
    return 0;
  }

  const toolCallById = new Map<string, number>();
  for (let i = 0; i < messages.length; i++) {
    const message = messages[i];
    if (message.role === 'tool_call' && message.id) {
      toolCallById.set(message.id, i);
    }
  }

  let expanded = true;
  while (expanded) {
    expanded = false;
    for (let i = startIdx; i < messages.length; i++) {
      const message = messages[i];
      if (message.role !== 'tool_result' || !message.id) {
        continue;
      }

      const callIdx = toolCallById.get(message.id);
      if (callIdx !== undefined && callIdx < startIdx) {
        startIdx = callIdx;
        expanded = true;
        break;
      }
    }
  }

  return startIdx;
}

export function splitHistoryLoadChunk(
  messages: HistoryMessage[],
  chunkSize: number,
): { remaining: HistoryMessage[]; chunk: HistoryMessage[] } {
  if (messages.length === 0) {
    return { remaining: [], chunk: [] };
  }

  const preferredStart = Math.max(0, messages.length - Math.max(1, chunkSize));
  const startIdx = findHistoryRenderStart(messages, preferredStart);
  return {
    remaining: messages.slice(0, startIdx),
    chunk: messages.slice(startIdx),
  };
}
