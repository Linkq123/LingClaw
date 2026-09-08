import type { HistoryMessage } from './types.js';

export interface HistoryRunScope {
  start: number;
  end: number;
}

// The server expands one stored message into several visible rows, and attaches
// terminal facts only to the last visible row in the run's message interval.
// Earlier Plan runs may share the original user anchor with a later revision;
// a row already owned by an earlier terminal must not be claimed again.
export function historyRunScopes(messages: HistoryMessage[]): HistoryRunScope[] {
  const scopes: HistoryRunScope[] = [];
  let previousEnd = -1;
  for (let end = 0; end < messages.length; end += 1) {
    const message = messages[end];
    const index = message.message_index;
    if (typeof index !== 'number' || !Number.isSafeInteger(index) || index < 0) continue;
    const outcomes = (Array.isArray(message.run_outcomes) ? message.run_outcomes : []).filter(
      (outcome) =>
        Boolean(outcome?.run_id) &&
        Number.isSafeInteger(outcome.start_message_index) &&
        outcome.start_message_index >= 0 &&
        Number.isSafeInteger(outcome.end_message_index) &&
        outcome.start_message_index <= index &&
        index <= outcome.end_message_index,
    );
    if (!outcomes.length) continue;
    const firstMessageIndex = Math.min(...outcomes.map((outcome) => outcome.start_message_index));
    let start = previousEnd + 1;
    while (start < end) {
      const startIndex = messages[start].message_index;
      if (
        typeof startIndex === 'number' &&
        Number.isSafeInteger(startIndex) &&
        startIndex >= firstMessageIndex
      )
        break;
      start += 1;
    }
    scopes.push({ start, end });
    previousEnd = end;
  }
  return scopes;
}

export function findHistoryRenderStart(messages: HistoryMessage[], preferredStart: number): number {
  let startIdx = Math.min(messages.length, Math.max(0, preferredStart));
  if (startIdx === 0 || startIdx === messages.length) return startIdx;
  const scopes = historyRunScopes(messages);

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

    // Keep the server's whole run interval in one render window, including
    // intermediate assistant text and user interventions. Legacy history has
    // no terminal facts, so it still expands to the preceding user boundary.
    // Preserving a tool pair below may reveal an even earlier run boundary.
    const scope = scopes.find((run) => run.start <= startIdx && startIdx <= run.end);
    if (scope && scope.start < startIdx) {
      startIdx = scope.start;
      expanded = true;
    } else if (!scope && messages[startIdx]?.role !== 'user') {
      let userBoundary = 0;
      for (let i = startIdx - 1; i >= 0; i -= 1) {
        if (messages[i].role === 'user') {
          userBoundary = i;
          break;
        }
      }
      if (userBoundary < startIdx) {
        startIdx = userBoundary;
        expanded = true;
      }
    }

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
