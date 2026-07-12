export type GroupMentionMember = {
  id: string;
  name: string;
  role?: string;
};

export type GroupMentionQuery = {
  start: number;
  end: number;
  query: string;
};

type ParsedProtocolMention = {
  id: string;
  isAll: boolean;
  start: number;
  length: number;
};

const PROTOCOL_CHAR_RE = /^[A-Za-z0-9@._-]$/;
const PROTOCOL_ID_RE = /^[A-Za-z0-9._-]+$/;
const ALL_PROTOCOL_CHAR_RE = /^[A-Za-z0-9@]$/;
const MENTION_QUERY_DELIMITER_RE = /[\s@()[\]{}<>,;:!?"'`，。！？；：（）【】《》“”‘’、]/u;
const ATTACHED_TRAILING_PUNCTUATION_RE = /^[,.;:!?，。！？；：、)\]}>）】》”’]+/u;

function trimmedTokenBounds(token: string, allowed: RegExp): { start: number; end: number } {
  let start = 0;
  let end = token.length;
  while (start < end && !allowed.test(token[start])) start += 1;
  while (end > start && !allowed.test(token[end - 1])) end -= 1;
  return { start, end };
}

function parseProtocolMentionToken(
  token: string,
  memberSet: Set<string>,
): ParsedProtocolMention | null {
  const allBounds = trimmedTokenBounds(token, ALL_PROTOCOL_CHAR_RE);
  const allToken = token.slice(allBounds.start, allBounds.end);
  if (allToken.toLowerCase() === '@all') {
    return { id: 'all', isAll: true, start: allBounds.start, length: 4 };
  }

  const bounds = trimmedTokenBounds(token, PROTOCOL_CHAR_RE);
  const normalized = token.slice(bounds.start, bounds.end);
  if (!normalized.startsWith('@')) return null;
  const raw = normalized.slice(1);
  if (!raw || !PROTOCOL_ID_RE.test(raw)) return null;
  if (memberSet.has(raw)) {
    return { id: raw, isAll: false, start: bounds.start, length: raw.length + 1 };
  }
  const withoutSentenceDots = raw.replace(/\.+$/, '');
  if (withoutSentenceDots !== raw && memberSet.has(withoutSentenceDots)) {
    return {
      id: withoutSentenceDots,
      isAll: false,
      start: bounds.start,
      length: withoutSentenceDots.length + 1,
    };
  }
  return null;
}

export function mentionedGroupTargets(value: string, activeMembers: string[]): string[] {
  const memberSet = new Set(activeMembers);
  const mentioned = new Set<string>();

  for (const token of String(value || '').split(/\s+/u)) {
    const mention = parseProtocolMentionToken(token, memberSet);
    if (!mention) continue;
    if (mention.isAll) return [...activeMembers];
    mentioned.add(mention.id);
  }

  return activeMembers.filter((member) => mentioned.has(member));
}

export function findGroupMentionQuery(value: string, caret: number): GroupMentionQuery | null {
  const safeCaret = Math.max(0, Math.min(caret, value.length));
  let tokenStart = safeCaret;
  while (tokenStart > 0 && !/\s/u.test(value[tokenStart - 1])) tokenStart -= 1;
  const tokenBeforeCaret = value.slice(tokenStart, safeCaret);
  const atOffset = tokenBeforeCaret.lastIndexOf('@');
  if (atOffset < 0) return null;
  const leading = tokenBeforeCaret.slice(0, atOffset);
  const query = tokenBeforeCaret.slice(atOffset + 1);
  if (/[A-Za-z0-9@._-]/.test(leading) || MENTION_QUERY_DELIMITER_RE.test(query)) return null;

  const start = tokenStart + atOffset;
  const trailingToken =
    value.slice(safeCaret).match(/^[^\s@()[\]{}<>,;:!?"'`，。！？；：（）【】《》“”‘’、]*/u)?.[0] ||
    '';
  return {
    start,
    end: safeCaret + trailingToken.length,
    query,
  };
}

export function filterGroupMentionMembers(
  query: string,
  activeMembers: string[],
  memberDetails: GroupMentionMember[],
  allLabel: string,
): GroupMentionMember[] {
  const normalizedQuery = query.trim().toLocaleLowerCase();
  const detailById = new Map(memberDetails.map((member) => [member.id, member]));
  const candidates: GroupMentionMember[] = [
    { id: 'all', name: allLabel, role: 'all' },
    ...activeMembers.map((id) => detailById.get(id) || { id, name: id, role: 'member' }),
  ];
  if (!normalizedQuery) return candidates;
  return candidates.filter((candidate) =>
    `${candidate.id}\n${candidate.name}`.toLocaleLowerCase().includes(normalizedQuery),
  );
}

export function insertGroupMention(
  value: string,
  query: GroupMentionQuery,
  sessionId: string,
): { value: string; cursor: number } {
  const token = `@${sessionId}`;
  let after = value.slice(query.end);
  let suffix = after ? (/^\s/u.test(after) ? '' : ' ') : ' ';
  const punctuation = after.match(ATTACHED_TRAILING_PUNCTUATION_RE)?.[0] || '';
  if (punctuation) {
    suffix = '';
    const rest = after.slice(punctuation.length);
    const nextWhitespace = rest.search(/\s/u);
    const tokenTail = nextWhitespace < 0 ? rest : rest.slice(0, nextWhitespace);
    if (/[A-Za-z0-9@._-]/.test(tokenTail)) {
      after = `${punctuation} ${rest}`;
    }
  }
  return {
    value: `${value.slice(0, query.start)}${token}${suffix}${after}`,
    cursor: query.start + token.length + suffix.length,
  };
}

export function decorateGroupMentions(
  container: HTMLElement,
  activeMembers: string[],
  resolveName: (sessionId: string) => string,
  allLabel: string,
): void {
  const memberSet = new Set(activeMembers);
  const walker = document.createTreeWalker(container, NodeFilter.SHOW_TEXT, {
    acceptNode(node) {
      const parent = node.parentElement;
      if (!parent || parent.closest('code, pre, a, .group-mention')) {
        return NodeFilter.FILTER_REJECT;
      }
      return node.nodeValue?.includes('@') ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_REJECT;
    },
  });
  const textNodes: Text[] = [];
  while (walker.nextNode()) textNodes.push(walker.currentNode as Text);

  for (const textNode of textNodes) {
    const text = textNode.nodeValue || '';
    let offset = 0;
    let changed = false;
    const fragment = document.createDocumentFragment();
    const tokenRe = /\S+/gu;
    let match: RegExpExecArray | null;

    while ((match = tokenRe.exec(text)) !== null) {
      const parsedMention = parseProtocolMentionToken(match[0], memberSet);
      if (!parsedMention) continue;
      const mentionStart = match.index + parsedMention.start;
      changed = true;
      fragment.append(text.slice(offset, mentionStart));
      const mention = document.createElement('span');
      mention.className = 'group-mention';
      mention.dataset.sessionId = parsedMention.isAll ? 'all' : parsedMention.id;
      mention.textContent = `@${parsedMention.isAll ? allLabel : resolveName(parsedMention.id)}`;
      mention.title = `@${parsedMention.isAll ? 'all' : parsedMention.id}`;
      fragment.appendChild(mention);
      offset = mentionStart + parsedMention.length;
    }

    if (!changed) continue;
    fragment.append(text.slice(offset));
    textNode.replaceWith(fragment);
  }
}
