import { dom, state } from './state.js';
import { AUTO_SCROLL_THRESHOLD } from './constants.js';

export function syncToolDrawerBounds() {
  if (!dom.inputArea) return;
  const viewport = window.visualViewport;
  const rect = dom.inputArea.getBoundingClientRect();
  const viewportBottom = viewport
    ? viewport.offsetTop + viewport.height
    : window.innerHeight;
  const bottomInset = Math.max(16, Math.ceil(viewportBottom - rect.top + 8));
  document.documentElement.style.setProperty('--tool-drawer-bottom', `${bottomInset}px`);
  document.documentElement.style.setProperty('--jump-to-latest-bottom', `${bottomInset + 10}px`);
}

export function distanceFromBottom() {
  return dom.chat.scrollHeight - dom.chat.scrollTop - dom.chat.clientHeight;
}

export function isChatNearBottom(threshold = AUTO_SCROLL_THRESHOLD) {
  return distanceFromBottom() <= threshold;
}

export function updateJumpToLatestVisibility() {
  if (!dom.jumpToLatestBtn) return;
  const show = !state.autoFollowChat && state.hasBufferedChatUpdates;
  const hasCount = state.unreadMessageCount > 0;
  dom.jumpToLatestBtn.hidden = !show;
  dom.jumpToLatestBtn.classList.toggle('visible', show);
  dom.jumpToLatestBtn.classList.toggle('has-state-only', show && !hasCount);
  if (dom.jumpToLatestBadge) {
    if (!show) {
      dom.jumpToLatestBadge.hidden = true;
      dom.jumpToLatestBadge.textContent = '';
    } else if (hasCount) {
      dom.jumpToLatestBadge.hidden = false;
      dom.jumpToLatestBadge.textContent = state.unreadMessageCount > 99 ? '99+' : String(state.unreadMessageCount);
    } else {
      dom.jumpToLatestBadge.hidden = false;
      dom.jumpToLatestBadge.textContent = '新';
    }
  }
  dom.jumpToLatestBtn.setAttribute(
    'aria-label',
    hasCount ? `Jump to latest messages, ${state.unreadMessageCount} unread items` : 'Jump to latest messages, new content available'
  );
  dom.jumpToLatestBtn.title = hasCount ? `${state.unreadMessageCount} 条新内容` : '有新内容';
}

export function clearBufferedChatUpdates() {
  state.hasBufferedChatUpdates = false;
  state.unreadMessageCount = 0;
  updateJumpToLatestVisibility();
}

export function setAutoFollowChat(nextFollow) {
  state.autoFollowChat = nextFollow;
  if (nextFollow) {
    clearBufferedChatUpdates();
  } else {
    updateJumpToLatestVisibility();
  }
}

export function markChatUpdateOffscreen() {
  if (state.bulkRenderingChat) return;
  state.hasBufferedChatUpdates = true;
  updateJumpToLatestVisibility();
}

export function queueUnreadContent({ countable = false } = {}) {
  if (state.bulkRenderingChat || state.autoFollowChat || isChatNearBottom()) {
    return;
  }
  state.hasBufferedChatUpdates = true;
  if (countable) {
    state.unreadMessageCount += 1;
  }
  updateJumpToLatestVisibility();
}

export function syncChatScrollState() {
  if (state.suppressScrollTracking) return;
  setAutoFollowChat(isChatNearBottom());
}

export function jumpToLatest() {
  setAutoFollowChat(true);
  scrollDown(true);
}

export function scrollDown(force = false) {
  if (state.bulkRenderingChat) {
    return false;
  }

  const shouldFollow = force || state.autoFollowChat || isChatNearBottom();
  if (!shouldFollow) {
    markChatUpdateOffscreen();
    return false;
  }

  state.suppressScrollTracking = true;
  dom.chat.scrollTop = dom.chat.scrollHeight;
  requestAnimationFrame(() => {
    state.suppressScrollTracking = false;
    setAutoFollowChat(true);
  });
  return true;
}
