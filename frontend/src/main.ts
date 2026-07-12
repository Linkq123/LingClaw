// CSS imports. The highlight.js stylesheet is injected dynamically by
// `theme.ts` so that an explicit user theme choice overrides the system
// setting; we no longer import it statically here.
import './css/base.css';
import './css/layout.css';
import './css/chat.css';
import './css/panels.css';
import './css/pages.css';
import './css/responsive.css';
import './css/workspace.css';

import { initTheme, cycleTheme, disposeTheme } from './theme.js';
import { initI18n, tr, toggleLanguage, subscribeLanguageChange, translateDom } from './i18n.js';
import { dom, initDomRefs, state } from './state.js';
import { HISTORY_LOAD_CHUNK_SIZE, HISTORY_RENDER_LIMIT } from './constants.js';
import { createIcon, iconMarkup } from './icons.js';
import { findHistoryRenderStart, splitHistoryLoadChunk } from './historyWindow.js';
import {
  formatTokenCount,
  formatToolDuration,
  hideWelcome,
  scheduleBackgroundTask,
} from './utils.js';
import type { GroupMemberDetail, SessionGroupSummary, SessionSummary } from './types.js';
import {
  syncToolDrawerBounds,
  cancelToolDrawerBoundsSync,
  invalidateChatScrollCache,
  clearBufferedChatUpdates,
  setAutoFollowChat,
  scrollDown,
  syncChatScrollState,
  jumpToLatest,
  updateJumpToLatestVisibility,
} from './scroll.js';
import {
  animatePanelIn,
  animateCollapsibleSection,
  linkCollapsibleControl,
} from './renderers/timeline.js';
import {
  completeExecutionStack,
  mountExecutionPanel,
  refreshExecutionStacks,
  resetExecutionStackState,
  restoreExecutionStackState,
  syncAllExecutionStackVisibility,
  toggleExecutionStack,
} from './renderers/execution-stack.js';
import {
  addMsg,
  addSystem,
  addError,
  renderUserImageThumbnails,
  showWelcome,
  setBusy,
  loadAppVersion,
} from './renderers/chat.js';
import {
  pinReactStatusToBottom,
  clearReactStatus,
  showReactStatus,
  setReactActTool,
  requestClearReactStatus,
  renderReactStatus,
} from './renderers/react-status.js';
import {
  addToolCall,
  appendToolOutput,
  updateToolProgress,
  addToolResult,
  openToolDrawerFromHeader,
  closeToolDrawer,
  syncToolDrawer,
  syncToolDrawerResponsiveState,
  trapToolDrawerFocus,
  toggleTool,
  refreshToolPanelsLanguage,
} from './renderers/tools.js';
import { preloadMarkdownEngine, scheduleMarkdownRender } from './markdown.js';
import {
  beginAssistantStream,
  finishAssistantStream,
  finishReasoningStream,
  scheduleFlush,
} from './handlers/stream.js';
import {
  connect,
  cancelReconnect,
  reconnectToActiveSession,
  refreshConnectionStatus,
} from './socket.js';
import {
  ensureUploadTokenInternal,
  updateAttachButton,
  dropUnavailablePendingUploads,
  initImageListeners,
  renderImagePreviews,
  syncRestoredSessionCapabilities,
  updateS3ConfigIdentity,
} from './images.js';
import { sendCmd, initInputListeners } from './input.js';
import {
  toggleMobileMenu,
  closeMobileMenu,
  toggleViewControlsMenu,
  closeShellPopovers,
  toggleMobileNavigation,
  closeMobileNavigation,
  createMobileNavigationSelectionHandler,
  createCommandMenuActionHandler,
  isMobileViewport,
  syncResponsiveNavigation,
  initMobileListeners,
} from './mobile.js';
import { applyToolsVisibility } from './viewState.js';
import {
  createSubagentPanel,
  addSubagentTool,
  appendSubagentToolOutput,
  updateSubagentProgress,
  updateSubagentToolResult,
  finishSubagentPanel,
  startSubagentReasoning,
  appendSubagentReasoning,
  finishSubagentReasoning,
  restoreSubagentHistorySnapshot,
  copySubagentSummary,
  openSubagentModal,
  closeSubagentModal,
  openSubagentToolDrawer,
  refreshSubagentPanelsLanguage,
  trapSubagentModalFocus,
} from './renderers/subagent.js';
import {
  createOrchestratePanel,
  updateOrchestrateLayer,
  markOrchestrateTask,
  finishOrchestratePanel,
  openOrchestrateTaskModal,
  closeOrchestrateTaskModal,
  refreshOrchestratePanelsLanguage,
} from './renderers/orchestrate.js';
import {
  openSettingsPage,
  closeSettingsPage,
  openUsagePage,
  closeUsagePage,
  prefetchPageChunks,
} from './pages/lazy.js';
import { closeOverlayById, matchesOverlayDismissTarget } from './pages/overlay.js';
import {
  CONFIG_SAVED_EVENT,
  acceptComposerHttpModelPayloadRevision,
  acceptComposerSocketModelPayloadRevision,
  beginComposerSessionTransition,
  captureComposerSessionTransitionFallbackCapabilities,
  captureComposerSessionTransitionTargetCapabilitiesBaseline,
  completeComposerSessionTransition,
  composerSessionPayloadMatchesTransition,
  getComposerConnectionGeneration,
  groupModelRosterMatches,
  handleComposerConfigSaved,
  refreshComposerAvailability,
  resetComposerGroupModelConfiguration,
  restoreComposerSessionTransition,
  sessionIdsMatchExactly,
  setComposerExplicitPrimaryModelConfigured,
  setComposerSessionModelConfigured,
  setGroupModelConfiguredMembers,
  syncComposerAvailability,
  updateComposerSessionTransitionFallbackCapabilities,
  updateComposerSessionTransitionFallback,
} from './composerAvailability.js';
import {
  buildHistoryReasoningPanel,
  finalizeOrDiscardLiveReasoningPanel,
} from './renderers/reasoning.js';
import {
  applyCompressionOutcome,
  applyTopLevelAutoTrace,
  clearActiveAutoTrace,
  clearCompressionOutcome,
  clearCompressionOutcomeForNewAnalyzeCycle,
  clearCompressionOutcomeForNewRound,
  toggleAutoDebug,
  updateAutoDebugToggleButton,
} from './renderers/auto-trace.js';
import {
  applyTaskPlan,
  finishTaskPlanPanel,
  refreshTaskPlanPanelsLanguage,
  supersedeTaskPlanPanel,
} from './renderers/task-plan.js';
import {
  clearPendingPlanAction,
  confirmPendingPlanExecution,
  executePendingPlan,
  renderPendingPlanAction,
  restorePendingPlanAction,
} from './renderers/pending-plan.js';
import {
  initSessionDrawer,
  renderSessionDrawer,
  toggleSessionDrawerExpanded,
} from './renderers/sessions.js';
import {
  applyTodosState,
  applyTodosVisibility,
  initTodosPanel,
  renderTodosPanel,
} from './renderers/todos.js';
import {
  createSession as requestCreateSession,
  createSessionGroup as requestCreateSessionGroup,
  deleteSessionGroup as requestDeleteSessionGroup,
  getSessionGroup as requestGetSessionGroup,
  promoteSessionGroupAdmin as requestPromoteSessionGroupAdmin,
  removeSessionGroupMember as requestRemoveSessionGroupMember,
  updateSessionGroup as requestUpdateSessionGroup,
  normalizeGroupVotes,
} from './sessionApi.js';
import {
  isActiveGroupRunStatus,
  isTerminalGroupRunStatus,
  isRecoverableActiveGroupConnectionError,
  loadActiveGroupId,
  loadActiveSessionId,
  mainSessionStateForGroupControl,
  normalizeGroupRunUpdatedAt,
  persistActiveGroupId,
  persistActiveSessionId,
  sessionIdAfterLeavingGroup,
  shouldApplyGroupRunStatusUpdate,
} from './sessionPersistence.js';

// ── Initialize DOM ──
initDomRefs();
initI18n();
state.activeSessionId = loadActiveSessionId();
state.activeGroupId = loadActiveGroupId();
if (state.activeGroupId) {
  const groupSessionState = mainSessionStateForGroupControl(state.activeSessionId);
  state.groupReturnSessionId = groupSessionState.groupReturnSessionId;
  state.activeSessionId = groupSessionState.activeSessionId;
}
const switchToSession = createMobileNavigationSelectionHandler(
  (sessionId) => !state.activeGroupId && sessionId === state.activeSessionId,
  performSwitchToSession,
);
const switchToGroup = createMobileNavigationSelectionHandler(
  (groupId) => groupId === state.activeGroupId,
  performSwitchToGroup,
);
initSessionDrawer({
  onCreate: createSession,
  onCreateGroup: createGroup,
  onDelete: deleteSession,
  onDeleteGroup: deleteGroup,
  onRename: renameSession,
  onRenameGroup: renameGroup,
  onSwitch: switchToSession,
  onSwitchGroup: switchToGroup,
});
initTodosPanel();

// React islands (Settings & Usage) are now code-split and mounted lazily on
// first `openSettingsPage()` / `openUsagePage()` call. We also prefetch them
// during idle time so the first open is instant.

// ── View toggles ──

function updateViewToggleButtons() {
  const syncButton = (button: HTMLButtonElement | null, label: string, enabled: boolean) => {
    if (!button) return;
    const labelEl = button.querySelector('.control-label');
    if (labelEl) {
      labelEl.textContent = label;
    } else {
      button.textContent = `${label}: ${enabled ? tr('common.on') : tr('common.off')}`;
    }
    button.classList.toggle('is-active', enabled);
    button.setAttribute('aria-pressed', String(enabled));
  };

  if (dom.toggleTodosBtn) {
    syncButton(dom.toggleTodosBtn, tr('todos.title'), state.showTodos);
  }
  if (dom.toggleToolsBtn) {
    syncButton(dom.toggleToolsBtn, tr('common.tools'), state.showTools);
  }
  if (dom.toggleReasoningBtn) {
    syncButton(dom.toggleReasoningBtn, tr('common.reasoning'), state.showReasoning);
  }
  updateAutoDebugToggleButton();
  const activeCount = [
    state.showTodos,
    state.showTools,
    state.showReasoning,
    state.autoDebugEnabled,
  ].filter(Boolean).length;
  const count = document.getElementById('view-controls-count');
  if (count) count.textContent = String(activeCount);
}

function refreshLocalizedUi() {
  translateDom();
  refreshToolPanelsLanguage();
  refreshTaskPlanPanelsLanguage();
  refreshSubagentPanelsLanguage();
  refreshOrchestratePanelsLanguage();
  syncComposerAvailability();
  refreshConnectionStatus();
  updateViewToggleButtons();
  updateUsageBadge();
  renderSessionDrawer();
  renderGroupTargetControls();
  renderGroupMemberDrawer();
  renderTodosPanel();
  renderReactStatus();
  refreshExecutionStacks();
  if (state.activeToolPanel) {
    syncToolDrawer(state.activeToolPanel);
  }
}

function applyViewState(viewState) {
  if (!viewState) return;

  if (typeof viewState.show_tools === 'boolean') {
    applyToolsVisibility(viewState.show_tools, {
      state,
      chat: dom.chat,
      closeToolDrawer,
      closeSubagentModal,
      closeOrchestrateTaskModal,
    });
  }

  if (typeof viewState.show_reasoning === 'boolean') {
    state.showReasoning = viewState.show_reasoning;
    dom.chat?.classList.toggle('hide-reasoning', !state.showReasoning);
    if (!state.showReasoning) {
      finishReasoningStream();
      if (state.reasoningPanel) {
        state.reasoningPanel.classList.remove('reasoning-active');
        finalizeOrDiscardLiveReasoningPanel(state.reasoningPanel);
      }
      state.reasoningPanel = null;
    }
  }

  syncAllExecutionStackVisibility();
  updateViewToggleButtons();
}

function toggleTodosVisibility() {
  state.showTodos = !state.showTodos;
  applyTodosVisibility();
  updateViewToggleButtons();
}

function toggleToolsVisibility() {
  if (!state.ws || state.ws.readyState !== 1) return;
  const nextShowTools = !state.showTools;
  applyViewState({ show_tools: nextShowTools });
  if (state.activeGroupId) return;
  sendCmd(`/tool ${nextShowTools ? 'on' : 'off'}`);
}

function toggleReasoningVisibility() {
  if (!state.ws || state.ws.readyState !== 1) return;
  const nextShowReasoning = !state.showReasoning;
  applyViewState({ show_reasoning: nextShowReasoning });
  if (state.activeGroupId) return;
  sendCmd(`/reasoning ${nextShowReasoning ? 'on' : 'off'}`);
}

// ── Usage badge ──

function updateUsageBadge() {
  if (!dom.usageBadge) return;
  const inp = state.dailyInputTokens;
  const out = state.dailyOutputTokens;
  if (inp === 0 && out === 0) {
    dom.usageBadge.textContent = '';
    dom.usageBadge.removeAttribute('title');
    return;
  }
  if (!dom.usageBadge.querySelector('.usage-badge-label')) {
    dom.usageBadge.innerHTML =
      '<svg class="icon" aria-hidden="true"><use href="#icon-chart"></use></svg><span class="usage-badge-label"></span>';
  }
  const label = dom.usageBadge.querySelector('.usage-badge-label');
  if (label)
    label.textContent = tr('usage.inOut', {
      input: formatTokenCount(inp),
      output: formatTokenCount(out),
    });
  dom.usageBadge.title = tr('header.usageBadgeTitle', {
    dailyInput: formatTokenCount(inp),
    dailyOutput: formatTokenCount(out),
    totalInput: formatTokenCount(state.totalInputTokens),
    totalOutput: formatTokenCount(state.totalOutputTokens),
  });
}

function normalizeSessionListPayload(payload): SessionSummary[] {
  const sessions = Array.isArray(payload?.sessions)
    ? payload.sessions.map((session) => ({
        id: String(session.id ?? ''),
        name: String(session.name ?? session.id ?? ''),
        updated_at:
          typeof session.updated_at === 'number'
            ? session.updated_at
            : Number(session.updated_at ?? 0),
        corrupt: session.corrupt === true,
      }))
    : [];
  sessions.sort((a, b) => {
    if (a.id === 'main' && b.id !== 'main') return -1;
    if (a.id !== 'main' && b.id === 'main') return 1;
    return (b.updated_at ?? 0) - (a.updated_at ?? 0) || a.id.localeCompare(b.id);
  });
  return sessions;
}

function normalizeSessionGroupListPayload(payload): SessionGroupSummary[] {
  const groups = Array.isArray(payload?.groups)
    ? payload.groups.map((group) => ({
        id: String(group.id ?? ''),
        name: String(group.name ?? group.id ?? ''),
        members: typeof group.members === 'number' ? group.members : Number(group.members ?? 0),
        messages: typeof group.messages === 'number' ? group.messages : Number(group.messages ?? 0),
        running: typeof group.running === 'number' ? group.running : Number(group.running ?? 0),
        updated_at:
          typeof group.updated_at === 'number' ? group.updated_at : Number(group.updated_at ?? 0),
        corrupt: group.corrupt === true,
      }))
    : [];
  groups.sort((a, b) => (b.updated_at ?? 0) - (a.updated_at ?? 0) || a.id.localeCompare(b.id));
  return groups;
}

function normalizeGroupMembers(members: unknown): string[] {
  if (!Array.isArray(members)) return [];
  const seen = new Set<string>();
  const out: string[] = [];
  for (const member of members) {
    const id = String(member || '').trim();
    if (!id || seen.has(id)) continue;
    seen.add(id);
    out.push(id);
  }
  return out;
}

function normalizeGroupMemberDetails(
  details: unknown,
  members: string[] = [],
): GroupMemberDetail[] {
  const seen = new Set<string>();
  const out: GroupMemberDetail[] = [];
  const allowed = new Set(['main', ...members]);
  const sessionsById = new Map(state.sessions.map((session) => [session.id, session.name]));
  if (Array.isArray(details)) {
    for (const item of details) {
      const raw = (item ?? {}) as Record<string, unknown>;
      const id = String(raw.id ?? '').trim();
      if (!id || seen.has(id) || !allowed.has(id)) continue;
      const role = String(raw.role ?? 'member');
      const rawName = String(raw.name ?? '').trim();
      out.push({
        id,
        name: rawName && rawName !== id ? rawName : sessionsById.get(id) || id,
        role: role === 'owner' || role === 'admin' ? role : 'member',
      });
      seen.add(id);
    }
  }
  if (!seen.has('main')) {
    out.unshift({
      id: 'main',
      name: sessionsById.get('main') || tr('common.main'),
      role: 'owner',
    });
    seen.add('main');
  }
  for (const member of members) {
    if (seen.has(member)) continue;
    out.push({
      id: member,
      name: sessionsById.get(member) || member,
      role: 'member',
    });
    seen.add(member);
  }
  return out;
}

function groupMemberName(sessionId: string): string {
  const id = String(sessionId || '').trim();
  if (!id) return 'session';
  const detail = state.activeGroupMemberDetails.find((member) => member.id === id);
  if (detail?.name) return detail.name;
  const summary = state.sessions.find((session) => session.id === id);
  return summary?.name || id;
}

function renderProtocolMentions(text: string): string {
  return String(text || '').replace(
    /(^|[\s([{<"'`])@([A-Za-z0-9_.-]*[A-Za-z0-9_-])(?=$|[\s)\]}>.,;:!?'"`])/g,
    (match, prefix, rawId) => {
      const id = String(rawId || '');
      if (id.toLowerCase() === 'all') return `${prefix}@${tr('common.all')}`;
      if (!state.activeGroupMembers.includes(id)) return match;
      return `${prefix}@${groupMemberName(id)}`;
    },
  );
}

function resetGroupTargetControls() {
  state.activeGroupMembers = [];
  state.activeGroupMemberDetails = [];
  state.activeGroupPendingVotes = [];
  state.groupMembersDrawerOpen = false;
  state.groupTargetMode = 'all';
  state.groupSelectedTargets = [];
  resetComposerGroupModelConfiguration();
  renderGroupTargetControls();
  renderGroupMemberDrawer();
}

function clearGroupRunState() {
  state.activeGroupRunIds.clear();
  state.groupRunStatuses.clear();
  state.groupRunSessions.clear();
}

function enterGroupControlSession() {
  const groupSessionState = mainSessionStateForGroupControl(
    state.activeSessionId,
    state.groupReturnSessionId,
  );
  state.groupReturnSessionId = groupSessionState.groupReturnSessionId;
  state.activeSessionId = groupSessionState.activeSessionId;
}

function leaveActiveGroupForSession(fallbackSessionId = loadActiveSessionId()): string {
  const nextSessionId = sessionIdAfterLeavingGroup(state.groupReturnSessionId, fallbackSessionId);
  state.activeGroupId = '';
  state.groupReturnSessionId = '';
  persistActiveGroupId('');
  clearGroupRunState();
  resetGroupTargetControls();
  state.activeSessionId = nextSessionId;
  persistActiveSessionId(nextSessionId);
  return nextSessionId;
}

function setActiveGroupMembers(
  members: unknown,
  memberDetails: unknown = state.activeGroupMemberDetails,
  pendingVotes: unknown = state.activeGroupPendingVotes,
) {
  const normalized = normalizeGroupMembers(members);
  const memberSet = new Set(normalized);
  state.activeGroupMembers = normalized;
  state.activeGroupMemberDetails = normalizeGroupMemberDetails(memberDetails, normalized);
  state.activeGroupPendingVotes = normalizeGroupVotes(pendingVotes, normalizeGroupMembers);
  state.groupSelectedTargets = state.groupSelectedTargets.filter((target) => memberSet.has(target));
  if (state.groupTargetMode === 'selected' && state.groupSelectedTargets.length === 0) {
    state.groupSelectedTargets = [...normalized];
  }
  renderGroupTargetControls();
  renderGroupMemberDrawer();
}

function selectGroupTargetMode(mode: 'all' | 'selected' | 'mentions') {
  state.groupTargetMode = mode;
  if (mode === 'selected' && state.groupSelectedTargets.length === 0) {
    state.groupSelectedTargets = [...state.activeGroupMembers];
  }
  renderGroupTargetControls();
  syncComposerAvailability();
  syncToolDrawerBounds();
}

function toggleSelectedGroupTarget(memberId: string) {
  const selected = new Set(state.groupSelectedTargets);
  if (selected.has(memberId)) {
    selected.delete(memberId);
  } else {
    selected.add(memberId);
  }
  const memberSet = new Set(state.activeGroupMembers);
  state.groupSelectedTargets = Array.from(selected).filter((target) => memberSet.has(target));
  renderGroupTargetControls();
  syncComposerAvailability();
  syncToolDrawerBounds();
}

function insertGroupMention(sessionId: string) {
  if (!dom.input) return;
  const token = sessionId === 'all' ? '@all' : `@${sessionId}`;
  const value = dom.input.value;
  const start = dom.input.selectionStart ?? value.length;
  const end = dom.input.selectionEnd ?? value.length;
  const before = value.slice(0, start);
  const after = value.slice(end);
  const prefix = before && !/\s$/.test(before) ? ' ' : '';
  const suffix = after && !/^\s/.test(after) ? ' ' : '';
  dom.input.value = `${before}${prefix}${token}${suffix}${after}`;
  const cursor = before.length + prefix.length + token.length + suffix.length;
  dom.input.focus();
  dom.input.setSelectionRange(cursor, cursor);
  dom.input.dispatchEvent(new Event('input', { bubbles: true }));
}

function toggleGroupMemberDrawer() {
  state.groupMembersDrawerOpen = !state.groupMembersDrawerOpen;
  renderGroupMemberDrawer();
}

function renderGroupTargetControls() {
  const bar = dom.groupTargetBar;
  if (!bar) return;
  bar.replaceChildren();
  if (!state.activeGroupId) {
    bar.hidden = true;
    return;
  }
  bar.hidden = false;

  const memberButton = document.createElement('button');
  memberButton.type = 'button';
  memberButton.className = 'group-members-toggle';
  memberButton.textContent = tr('common.members', { count: state.activeGroupMembers.length });
  memberButton.setAttribute('aria-expanded', String(state.groupMembersDrawerOpen));
  memberButton.addEventListener('click', toggleGroupMemberDrawer);
  bar.appendChild(memberButton);

  const modes = [
    { id: 'all' as const, label: tr('common.all') },
    { id: 'selected' as const, label: tr('common.selected') },
    { id: 'mentions' as const, label: tr('group.mentions') },
  ];
  const modeGroup = document.createElement('div');
  modeGroup.className = 'group-target-modes';
  modeGroup.setAttribute('role', 'group');
  modeGroup.setAttribute('aria-label', tr('group.modeLabel'));
  for (const mode of modes) {
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'group-target-mode';
    button.textContent = mode.label;
    button.dataset.mode = mode.id;
    button.classList.toggle('is-active', state.groupTargetMode === mode.id);
    button.setAttribute('aria-pressed', state.groupTargetMode === mode.id ? 'true' : 'false');
    button.addEventListener('click', () => selectGroupTargetMode(mode.id));
    modeGroup.appendChild(button);
  }
  bar.appendChild(modeGroup);

  if (state.groupTargetMode !== 'selected') return;

  const chips = document.createElement('div');
  chips.className = 'group-target-members';
  const selected = new Set(state.groupSelectedTargets);
  for (const member of state.activeGroupMembers) {
    const chip = document.createElement('button');
    chip.type = 'button';
    chip.className = 'group-target-member';
    chip.classList.toggle('is-active', selected.has(member));
    chip.setAttribute('aria-pressed', selected.has(member) ? 'true' : 'false');
    chip.textContent = groupMemberName(member);
    chip.title = member;
    chip.addEventListener('click', () => toggleSelectedGroupTarget(member));
    chips.appendChild(chip);
  }
  bar.appendChild(chips);
}

function renderGroupMemberDrawer() {
  let drawer = document.getElementById('group-member-drawer');
  if (!state.activeGroupId) {
    drawer?.remove();
    return;
  }
  if (!drawer) {
    drawer = document.createElement('aside');
    drawer.id = 'group-member-drawer';
    drawer.className = 'group-member-drawer';
    document.body.appendChild(drawer);
  }
  drawer.classList.toggle('is-open', state.groupMembersDrawerOpen);
  drawer.setAttribute('aria-hidden', state.groupMembersDrawerOpen ? 'false' : 'true');
  // Skip the (expensive) DOM rebuild while the drawer is closed; it is rebuilt when
  // reopened via toggleGroupMemberDrawer. This avoids an O(events x members) re-render
  // storm when member status events stream in during an @all dispatch.
  if (!state.groupMembersDrawerOpen) return;

  const header = document.createElement('div');
  header.className = 'group-member-drawer-header';
  const title = document.createElement('div');
  title.className = 'group-member-drawer-title';
  title.textContent = tr('group.members');
  const closeButton = document.createElement('button');
  closeButton.type = 'button';
  closeButton.className = 'group-member-drawer-close';
  closeButton.appendChild(createIcon('close'));
  closeButton.setAttribute('aria-label', tr('group.closeMembers'));
  closeButton.addEventListener('click', () => {
    state.groupMembersDrawerOpen = false;
    renderGroupMemberDrawer();
    renderGroupTargetControls();
  });
  header.append(title, closeButton);

  const list = document.createElement('div');
  list.className = 'group-member-list';
  for (const detail of state.activeGroupMemberDetails) {
    const row = document.createElement('div');
    row.className = 'group-member-row';
    row.dataset.sessionId = detail.id;

    const main = document.createElement('div');
    main.className = 'group-member-main';
    const name = document.createElement('div');
    name.className = 'group-member-name';
    name.textContent = detail.name || detail.id;
    const meta = document.createElement('div');
    meta.className = 'group-member-meta';
    meta.textContent = tr('group.memberMeta', {
      id: detail.id,
      role: groupMemberRoleLabel(detail.role),
      status: groupMemberStatus(detail.id),
    });
    main.append(name, meta);

    const actions = document.createElement('div');
    actions.className = 'group-member-actions';
    if (detail.id !== 'main') {
      const mentionButton = document.createElement('button');
      mentionButton.type = 'button';
      mentionButton.className = 'group-member-action';
      mentionButton.textContent = '@';
      mentionButton.title = tr('group.mention', { name: detail.name || detail.id });
      mentionButton.addEventListener('click', () => insertGroupMention(detail.id));
      actions.appendChild(mentionButton);

      if (detail.role === 'member') {
        const promoteButton = document.createElement('button');
        promoteButton.type = 'button';
        promoteButton.className = 'group-member-action';
        promoteButton.textContent = tr('common.admin');
        promoteButton.title = tr('group.promote', { name: detail.name || detail.id });
        promoteButton.addEventListener('click', () => void promoteGroupMember(detail.id));
        actions.appendChild(promoteButton);
      }

      const removeButton = document.createElement('button');
      removeButton.type = 'button';
      removeButton.className = 'group-member-action danger';
      removeButton.textContent = tr('common.remove');
      removeButton.title = tr('group.remove', { name: detail.name || detail.id });
      removeButton.addEventListener('click', () => void removeGroupMember(detail.id));
      actions.appendChild(removeButton);
    } else {
      const mentionAllButton = document.createElement('button');
      mentionAllButton.type = 'button';
      mentionAllButton.className = 'group-member-action';
      mentionAllButton.textContent = '@all';
      mentionAllButton.title = tr('group.mentionAll');
      mentionAllButton.addEventListener('click', () => insertGroupMention('all'));
      actions.appendChild(mentionAllButton);
    }

    row.append(main, actions);
    list.appendChild(row);
  }

  const votes = document.createElement('div');
  votes.className = 'group-vote-list';
  if (state.activeGroupPendingVotes.length > 0) {
    const voteTitle = document.createElement('div');
    voteTitle.className = 'group-vote-title';
    voteTitle.textContent = tr('group.pendingVotes');
    votes.appendChild(voteTitle);
    for (const vote of state.activeGroupPendingVotes) {
      const item = document.createElement('div');
      item.className = 'group-vote-item';
      const targetName = groupMemberName(vote.target_session_id);
      item.textContent = tr('group.voteApprovals', {
        name: targetName,
        approvals: vote.approvals.length,
        threshold: vote.threshold,
      });
      votes.appendChild(item);
    }
  }

  drawer.replaceChildren(header, list, votes);
}

async function refreshSessionsList() {
  try {
    const response = await fetch('/api/sessions', { cache: 'no-store' });
    if (!response.ok) return;
    const payload = await response.json();
    state.sessions = normalizeSessionListPayload(payload);
    renderSessionDrawer();
  } catch {
    // ignore session list refresh failures; live socket state still works
  }
}

async function refreshGroupsList() {
  try {
    const response = await fetch('/api/session-groups', { cache: 'no-store' });
    if (!response.ok) return;
    const payload = await response.json();
    state.sessionGroups = normalizeSessionGroupListPayload(payload);
    renderSessionDrawer();
  } catch {
    // ignore group list refresh failures; live socket state still works
  }
}

function upsertSessionSummary(session: SessionSummary) {
  const existingIndex = state.sessions.findIndex((existing) => existing.id === session.id);
  if (existingIndex >= 0) {
    state.sessions[existingIndex] = { ...state.sessions[existingIndex], ...session };
  } else {
    state.sessions.push(session);
  }
  state.sessions = normalizeSessionListPayload({ sessions: state.sessions });
}

function sessionIdentityActionBlocked(): boolean {
  return (
    state.sessionSwitchInFlight ||
    state.sessionIdentityMutationInFlight ||
    state.composerSessionTransitionPending ||
    state.composerSessionIdentityPending ||
    state.imageUploadInFlight
  );
}

function performSwitchToSession(nextSessionId: string) {
  if (sessionIdentityActionBlocked()) return;
  beginComposerSessionTransition(false, nextSessionId);
  state.activeGroupId = '';
  state.groupReturnSessionId = '';
  persistActiveGroupId('');
  clearGroupRunState();
  resetGroupTargetControls();
  state.pendingDeleteSessionId =
    state.activeSessionId && state.activeSessionId !== 'main' ? state.activeSessionId : '';
  state.activeSessionId = nextSessionId;
  persistActiveSessionId(nextSessionId);
  state.sessionSwitchInFlight = true;
  renderSessionDrawer();
  reconnectToActiveSession(handleMessage);
}

function performSwitchToGroup(nextGroupId: string) {
  if (sessionIdentityActionBlocked()) return;
  beginComposerSessionTransition();
  resetComposerGroupModelConfiguration();
  enterGroupControlSession();
  state.activeGroupId = nextGroupId;
  persistActiveGroupId(nextGroupId);
  state.activeGroupMembers = [];
  clearGroupRunState();
  state.groupTargetMode = 'all';
  state.groupSelectedTargets = [];
  renderGroupTargetControls();
  state.sessionSwitchInFlight = true;
  renderSessionDrawer();
  reconnectToActiveSession(handleMessage);
}

let sessionCreateInFlight = false;

async function createSession() {
  if (sessionCreateInFlight || sessionIdentityActionBlocked()) return;
  sessionCreateInFlight = true;
  state.sessionIdentityMutationInFlight = true;
  renderSessionDrawer();
  syncComposerAvailability();
  updateAttachButton();
  try {
    const created = await requestCreateSession();
    upsertSessionSummary(created);
    state.pendingDeleteSessionId =
      state.activeSessionId && state.activeSessionId !== 'main' ? state.activeSessionId : '';
    state.activeGroupId = '';
    state.groupReturnSessionId = '';
    persistActiveGroupId('');
    clearGroupRunState();
    resetGroupTargetControls();
    beginComposerSessionTransition(false, created.id);
    state.sessionSwitchInFlight = true;
    state.activeSessionId = created.id;
    persistActiveSessionId(created.id);
    renderSessionDrawer();
    reconnectToActiveSession(handleMessage);
  } catch (error) {
    addError(
      tr('session.errorCreate', { error: error instanceof Error ? error.message : String(error) }),
    );
    renderSessionDrawer();
  } finally {
    sessionCreateInFlight = false;
    state.sessionIdentityMutationInFlight = false;
    renderSessionDrawer();
    syncComposerAvailability();
    updateAttachButton();
  }
}

async function createGroup() {
  if (sessionCreateInFlight || sessionIdentityActionBlocked()) return;
  const rawName = window.prompt(tr('session.promptGroupName'), tr('session.defaultGroupName'));
  const name = rawName?.trim();
  if (!name) return;
  const defaultMembers = state.sessions
    .filter((session) => !session.corrupt && session.id !== 'main')
    .map((session) => session.id)
    .join(', ');
  const rawMembers = window.prompt(tr('session.promptGroupMembers'), defaultMembers);
  const members = (rawMembers || '')
    .split(',')
    .map((item) => item.trim())
    .filter(Boolean);
  sessionCreateInFlight = true;
  state.sessionIdentityMutationInFlight = true;
  renderSessionDrawer();
  syncComposerAvailability();
  updateAttachButton();
  try {
    const created = await requestCreateSessionGroup(name, members);
    upsertGroupSummary(created);
    enterGroupControlSession();
    beginComposerSessionTransition();
    state.sessionSwitchInFlight = true;
    resetComposerGroupModelConfiguration();
    state.activeGroupId = created.id;
    persistActiveGroupId(created.id);
    clearGroupRunState();
    setActiveGroupMembers(members);
    renderSessionDrawer();
    reconnectToActiveSession(handleMessage);
  } catch (error) {
    addError(
      tr('session.errorCreateGroup', {
        error: error instanceof Error ? error.message : String(error),
      }),
    );
    renderSessionDrawer();
  } finally {
    sessionCreateInFlight = false;
    state.sessionIdentityMutationInFlight = false;
    renderSessionDrawer();
    syncComposerAvailability();
    updateAttachButton();
  }
}

function deleteSession(sessionId: string) {
  const targetSessionId = String(sessionId || '').trim();
  if (!targetSessionId || state.activeGroupId || sessionIdentityActionBlocked()) return;
  if (targetSessionId === 'main' || targetSessionId === state.activeSessionId) return;
  if (!targetSessionId) return;
  const confirmed = window.confirm(tr('session.confirmDelete', { id: targetSessionId }));
  if (!confirmed) return;
  state.pendingDeleteSessionId = targetSessionId;
  renderSessionDrawer();
  sendCmd(`/delete ${targetSessionId}`);
}

async function renameSession(sessionId: string) {
  const targetSessionId = String(sessionId || '').trim();
  if (!targetSessionId || sessionIdentityActionBlocked()) return;
  const current = state.sessions.find((session) => session.id === targetSessionId);
  if (current?.corrupt) return;
  const raw = window.prompt(tr('session.promptName'), current?.name || targetSessionId);
  const nextName = raw?.trim();
  if (!nextName || nextName === (current?.name || targetSessionId)) return;

  try {
    const response = await fetch(`/api/session?session=${encodeURIComponent(targetSessionId)}`, {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ name: nextName }),
    });
    const payload = await response.json().catch(() => ({}));
    if (!response.ok) {
      throw new Error(String(payload?.error || `HTTP ${response.status}`));
    }

    const updated = payload?.session;
    if (updated?.id) {
      const updatedSession: SessionSummary = {
        id: String(updated.id),
        name: String(updated.name ?? updated.id),
        updated_at:
          typeof updated.updated_at === 'number'
            ? updated.updated_at
            : Number(updated.updated_at ?? 0),
        corrupt: updated.corrupt === true,
      };
      upsertSessionSummary(updatedSession);
      if (updatedSession.id === state.activeSessionId) {
        dom.sessionNameEl.textContent = updatedSession.name || tr('common.main');
      }
      renderSessionDrawer();
    }
    void refreshSessionsList();
  } catch (error) {
    addError(
      tr('session.errorRename', { error: error instanceof Error ? error.message : String(error) }),
    );
  }
}

async function renameGroup(groupId: string) {
  const targetGroupId = String(groupId || '').trim();
  if (!targetGroupId || sessionIdentityActionBlocked()) return;
  const current = state.sessionGroups.find((group) => group.id === targetGroupId);
  if (current?.corrupt) return;
  const rawName = window.prompt(tr('session.promptGroupName'), current?.name || targetGroupId);
  const nextName = rawName?.trim();
  if (!nextName) return;
  let existingMembers: string[] = [];
  try {
    existingMembers = (await requestGetSessionGroup(targetGroupId)).members;
  } catch (error) {
    addError(
      tr('session.errorLoadGroupMembers', {
        error: error instanceof Error ? error.message : String(error),
      }),
    );
    return;
  }
  const rawMembers = window.prompt(tr('session.promptGroupMembers'), existingMembers.join(', '));
  if (rawMembers == null) return;
  const members = (rawMembers || '')
    .split(',')
    .map((item) => item.trim())
    .filter(Boolean);
  try {
    const updated = await requestUpdateSessionGroup(targetGroupId, nextName, members);
    upsertGroupSummary(updated);
    if (updated.id === state.activeGroupId) {
      dom.sessionNameEl.textContent = updated.name || updated.id;
      setActiveGroupMembers(members);
    }
    renderSessionDrawer();
    void refreshGroupsList();
  } catch (error) {
    addError(
      tr('session.errorUpdateGroup', {
        error: error instanceof Error ? error.message : String(error),
      }),
    );
  }
}

async function deleteGroup(groupId: string) {
  const targetGroupId = String(groupId || '').trim();
  if (!targetGroupId || sessionIdentityActionBlocked()) return;
  const confirmed = window.confirm(tr('session.confirmDeleteGroup', { id: targetGroupId }));
  if (!confirmed) return;
  const changesActiveIdentity = state.activeGroupId === targetGroupId;
  if (changesActiveIdentity) {
    state.sessionIdentityMutationInFlight = true;
    renderSessionDrawer();
    syncComposerAvailability();
    updateAttachButton();
  }
  try {
    await requestDeleteSessionGroup(targetGroupId);
    state.sessionGroups = state.sessionGroups.filter((group) => group.id !== targetGroupId);
    if (state.activeGroupId === targetGroupId) {
      leaveActiveGroupForSession();
      beginComposerSessionTransition(false, state.activeSessionId);
      state.sessionSwitchInFlight = true;
      reconnectToActiveSession(handleMessage);
    }
    renderSessionDrawer();
  } catch (error) {
    addError(
      tr('session.errorDeleteGroup', {
        error: error instanceof Error ? error.message : String(error),
      }),
    );
  } finally {
    if (changesActiveIdentity) {
      state.sessionIdentityMutationInFlight = false;
      renderSessionDrawer();
      syncComposerAvailability();
      updateAttachButton();
    }
  }
}

function applyGroupDetail(detail: {
  id?: string;
  name?: string;
  members?: unknown;
  member_details?: unknown;
  pending_votes?: unknown;
  model_override_members?: unknown;
  model_configured_members?: unknown;
  model_member_ids?: unknown;
  explicitPrimaryModelConfigured?: unknown;
  configRevision?: unknown;
  capabilities?: { s3?: boolean; s3_config_id?: string | null };
}) {
  if (!detail?.id || detail.id !== state.activeGroupId) return;
  if (detail.name) {
    dom.sessionNameEl.textContent = detail.name;
  }
  setActiveGroupMembers(detail.members, detail.member_details, detail.pending_votes);
  applyGroupModelConfigurationAfterRosterUpdate(detail, false);
}

function applyGroupModelConfiguration(
  detail: {
    members?: unknown;
    model_override_members?: unknown;
    model_configured_members?: unknown;
    model_member_ids?: unknown;
    explicitPrimaryModelConfigured?: unknown;
    configRevision?: unknown;
    capabilities?: { s3?: boolean; s3_config_id?: string | null };
  },
  connectionPayload: boolean,
  forceS3TokenRefresh = false,
): boolean {
  const modelMemberIds = Array.isArray(detail.model_member_ids)
    ? detail.model_member_ids
    : detail.members;
  const revisionAccepted = connectionPayload
    ? acceptComposerSocketModelPayloadRevision(detail.configRevision)
    : acceptComposerHttpModelPayloadRevision(detail.configRevision);
  if (!revisionAccepted) return false;
  applyS3Capabilities(detail, forceS3TokenRefresh);
  if (Array.isArray(modelMemberIds) && !groupModelRosterMatches(modelMemberIds)) {
    if (connectionPayload && state.composerGroupModelRevision !== state.composerConfigRevision) {
      void refreshActiveGroupModelConfiguration();
    }
    return false;
  }
  if (typeof detail.explicitPrimaryModelConfigured === 'boolean') {
    setComposerExplicitPrimaryModelConfigured(
      detail.explicitPrimaryModelConfigured,
      detail.configRevision,
    );
  }
  const configuredMembers = Array.isArray(detail.model_configured_members)
    ? detail.model_configured_members
    : state.composerExplicitPrimaryModelConfigured
      ? state.activeGroupMembers
      : detail.model_override_members;
  return setGroupModelConfiguredMembers(configuredMembers, detail.configRevision);
}

let activeGroupModelRefreshInFlight = false;
let activeGroupModelRefreshPending = false;

function applyGroupModelConfigurationAfterRosterUpdate(
  detail: Parameters<typeof applyGroupModelConfiguration>[0],
  connectionPayload: boolean,
  forceS3TokenRefresh = false,
): boolean {
  const refreshWasInFlight = activeGroupModelRefreshInFlight;
  const refreshWasPending = activeGroupModelRefreshPending;
  const applied = applyGroupModelConfiguration(detail, connectionPayload, forceS3TokenRefresh);
  const refreshStarted = !refreshWasInFlight && activeGroupModelRefreshInFlight;
  const refreshQueued = !refreshWasPending && activeGroupModelRefreshPending;
  if (
    !applied &&
    state.composerGroupModelRevision !== state.composerConfigRevision &&
    !refreshStarted &&
    !refreshQueued &&
    !activeGroupModelRefreshPending
  ) {
    void refreshActiveGroupModelConfiguration();
  }
  return applied;
}

async function refreshActiveGroupModelConfiguration(): Promise<void> {
  const groupId = state.activeGroupId;
  if (!groupId) return;
  if (activeGroupModelRefreshInFlight) {
    activeGroupModelRefreshPending = true;
    return;
  }
  const connectionGeneration = getComposerConnectionGeneration();
  activeGroupModelRefreshInFlight = true;
  try {
    for (let attempt = 0; attempt < 2; attempt += 1) {
      const detail = await requestGetSessionGroup(groupId);
      if (state.activeGroupId !== groupId) return;
      if (getComposerConnectionGeneration() !== connectionGeneration) {
        activeGroupModelRefreshPending = true;
        return;
      }
      if (applyGroupModelConfiguration(detail, false)) return;
    }
  } catch {
    // A later reliable model event or reconnect can retry this bounded refresh.
  } finally {
    activeGroupModelRefreshInFlight = false;
    if (activeGroupModelRefreshPending) {
      activeGroupModelRefreshPending = false;
      void refreshActiveGroupModelConfiguration();
    }
  }
}

function applyS3Capabilities(data, forceTokenRefresh = false): void {
  const identityChanged = updateS3ConfigIdentity(data.capabilities?.s3_config_id, true);
  if (data.capabilities && typeof data.capabilities.s3 === 'boolean') {
    const previousS3Capable = state.s3Capable;
    state.s3Capable = data.capabilities.s3;
    if (state.s3Capable) {
      if (forceTokenRefresh || identityChanged || !previousS3Capable || !state.uploadToken) {
        void ensureUploadTokenInternal(forceTokenRefresh || identityChanged).catch(() => {});
      }
    } else {
      state.uploadToken = '';
      state.uploadTokenPromise = null;
      dropUnavailablePendingUploads(previousS3Capable);
    }
    updateAttachButton();
  }
}

function applySessionCapabilities(data): void {
  if (data.capabilities && typeof data.capabilities.image === 'boolean') {
    state.imageCapable = data.capabilities.image;
    updateAttachButton();
  }
  applyS3Capabilities(data, true);
}

function restoreComposerSessionTransitionWithCapabilities(): void {
  const restored = restoreComposerSessionTransition();
  syncRestoredSessionCapabilities(restored);
  if (restored) renderSessionDrawer();
}

function sessionModelPayloadTargetsActiveSession(data, authoritativeFullPayload = false): boolean {
  const payloadSessionId = String(data.id || '').trim();
  if (!payloadSessionId) return false;
  if (!state.composerSessionTransitionPending) {
    if (authoritativeFullPayload && state.composerSessionIdentityPending) return true;
    return sessionIdsMatchExactly(payloadSessionId, state.activeSessionId || 'main');
  }
  const updatedFallback = updateComposerSessionTransitionFallback(
    data.id,
    data.modelOverridePresent === true,
    data.modelOverrideConfigured === true,
    typeof data.effectiveModelConfigured === 'boolean' ? data.effectiveModelConfigured : undefined,
    typeof data.explicitPrimaryModelConfigured === 'boolean'
      ? data.explicitPrimaryModelConfigured
      : undefined,
    data.configRevision,
  );
  if (updatedFallback) {
    const targetCapabilitiesApplied = updateComposerSessionTransitionFallbackCapabilities(
      typeof data.capabilities?.image === 'boolean' ? data.capabilities.image : undefined,
      typeof data.capabilities?.s3 === 'boolean' ? data.capabilities.s3 : undefined,
      typeof data.capabilities?.s3_config_id === 'string'
        ? data.capabilities.s3_config_id
        : data.capabilities?.s3_config_id === null
          ? ''
          : undefined,
    );
    if (!targetCapabilitiesApplied) {
      applySessionCapabilities(data);
      captureComposerSessionTransitionFallbackCapabilities();
    }
    return false;
  }
  if (!composerSessionPayloadMatchesTransition(data.id)) return false;
  return true;
}

function applySessionModelFields(data, completeTransition = true): boolean {
  if (!acceptComposerSocketModelPayloadRevision(data.configRevision)) return false;
  if (typeof data.explicitPrimaryModelConfigured === 'boolean') {
    setComposerExplicitPrimaryModelConfigured(
      data.explicitPrimaryModelConfigured,
      data.configRevision,
    );
  }
  setComposerSessionModelConfigured(
    data.modelOverridePresent === true,
    data.modelOverrideConfigured === true,
    typeof data.effectiveModelConfigured === 'boolean' ? data.effectiveModelConfigured : undefined,
    data.configRevision,
    completeTransition,
  );
  captureComposerSessionTransitionTargetCapabilitiesBaseline();
  applySessionCapabilities(data);
  return true;
}

function applySessionModelConfiguration(data): boolean {
  return sessionModelPayloadTargetsActiveSession(data) && applySessionModelFields(data, false);
}

async function promoteGroupMember(sessionId: string) {
  if (!state.activeGroupId) return;
  try {
    const detail = await requestPromoteSessionGroupAdmin(state.activeGroupId, sessionId);
    applyGroupDetail(detail);
    void refreshGroupsList();
  } catch (error) {
    addError(
      tr('group.errorPromote', { error: error instanceof Error ? error.message : String(error) }),
    );
  }
}

async function removeGroupMember(sessionId: string) {
  if (!state.activeGroupId || sessionId === 'main') return;
  const label = groupMemberName(sessionId);
  const confirmed = window.confirm(tr('group.confirmRemove', { name: label }));
  if (!confirmed) return;
  try {
    const detail = await requestRemoveSessionGroupMember(state.activeGroupId, sessionId);
    if (detail) {
      applyGroupDetail(detail);
    } else {
      state.activeGroupMembers = state.activeGroupMembers.filter((member) => member !== sessionId);
      setActiveGroupMembers(state.activeGroupMembers);
    }
    void refreshGroupsList();
  } catch (error) {
    addError(
      tr('group.errorRemove', { error: error instanceof Error ? error.message : String(error) }),
    );
  }
}

function appendRoundUsage(messageEl, inputTokens, outputTokens, firstTokenMs = null) {
  const lastAssistantRow = messageEl ? messageEl.closest('.msg-row') : null;
  if (!lastAssistantRow) return;
  const content = lastAssistantRow.querySelector('.msg-content');
  if (!content) return;
  if (content.querySelector('.msg-usage')) return;
  const label = document.createElement('div');
  label.className = 'msg-usage';
  const parts = [`${formatTokenCount(inputTokens)} in / ${formatTokenCount(outputTokens)} out`];
  if (firstTokenMs != null) parts.push(`首 token ${formatToolDuration(firstTokenMs)}`);
  label.replaceChildren(
    ...parts.map((part) => {
      const item = document.createElement('span');
      item.textContent = part;
      return item;
    }),
  );
  label.title = [
    `Input: ${inputTokens.toLocaleString()} tokens`,
    `Output: ${outputTokens.toLocaleString()} tokens`,
    firstTokenMs != null ? `First token latency: ${formatToolDuration(firstTokenMs)}` : '',
  ]
    .filter(Boolean)
    .join('\n');
  content.appendChild(label);
}

function upsertGroupSummary(group: SessionGroupSummary) {
  const existingIndex = state.sessionGroups.findIndex((existing) => existing.id === group.id);
  if (existingIndex >= 0) {
    state.sessionGroups[existingIndex] = { ...state.sessionGroups[existingIndex], ...group };
  } else {
    state.sessionGroups.push(group);
  }
  state.sessionGroups = normalizeSessionGroupListPayload({ groups: state.sessionGroups });
}

function markCurrentRoundFirstTokenAt() {
  if (!state.currentRoundStartedAt || state.currentRoundFirstTokenAt) return;
  state.currentRoundFirstTokenAt = performance.now();
}

function resetRoundTimers() {
  state.currentRoundStartedAt = 0;
  state.currentRoundFirstTokenAt = 0;
}

// ── History lazy-load ──

function createLoadMoreRow(count: number): HTMLElement {
  const loadMoreRow = document.createElement('div');
  loadMoreRow.className = 'msg-row system';
  loadMoreRow.id = 'load-more-row';
  const btn = document.createElement('button');
  btn.className = 'load-more-btn';
  btn.dataset.action = 'load-earlier';
  btn.type = 'button';
  btn.textContent = tr('composer.loadEarlier', { count });
  btn.setAttribute('aria-label', tr('composer.loadEarlierAria', { count }));
  loadMoreRow.appendChild(btn);
  return loadMoreRow;
}

function parseOrchestrationHistoryResult(resultText) {
  const text = (resultText || '').trimStart();
  const aborted = /^## Orchestration Aborted\b/m.test(text);
  const completedMatch = text.match(/(\d+) completed/);
  const failedMatch = text.match(/(\d+) failed/);
  const skippedMatch = text.match(/(\d+) skipped/);
  const taskStatuses = new Map();
  const taskHeaderRe = /^### \[\d+\] (.+?) \((.+?)\) — (✅|❌|⏭️)/gm;

  let match;
  while ((match = taskHeaderRe.exec(text)) !== null) {
    const [, taskId, _agent, icon] = match;
    const status = icon === '✅' ? 'completed' : icon === '❌' ? 'failed' : 'skipped';
    taskStatuses.set(taskId, status);
  }

  return {
    aborted,
    completed: completedMatch ? parseInt(completedMatch[1], 10) : 0,
    failed: failedMatch ? parseInt(failedMatch[1], 10) : 0,
    skipped: skippedMatch ? parseInt(skippedMatch[1], 10) : 0,
    taskStatuses,
  };
}

function markHistoryMessageIndex(messageEl: Element | null, messageIndex: unknown) {
  if (typeof messageIndex !== 'number') return;
  const row = messageEl?.closest('.msg-row') as HTMLElement | null;
  if (row) row.dataset.messageIndex = String(messageIndex);
}

function renderHistoryMessage(m, options: { followMarkdown?: boolean } = {}) {
  const { followMarkdown = true } = options;
  switch (m.role) {
    case 'user': {
      completeExecutionStack({ immediate: true, durationMs: null });
      const el = addMsg('user', m.content, m.timestamp);
      markHistoryMessageIndex(el, m.message_index);
      if (m.images && m.images.length > 0) renderUserImageThumbnails(el, m.images);
      break;
    }
    case 'assistant': {
      if (m.thinking && m.thinking.trim() && state.showReasoning) {
        const panel = buildHistoryReasoningPanel(m.thinking);
        mountExecutionPanel(panel, 'reasoning');
        invalidateChatScrollCache();
      }
      // Thinking-only cycles (no text, tool call follows) have empty content.
      // Only create a bubble when there is actual message text.
      if (m.content) {
        completeExecutionStack({ immediate: true, durationMs: null });
        const el = addMsg('assistant', m.content, m.timestamp);
        markHistoryMessageIndex(el, m.message_index);
        el._rawText = m.content;
        scheduleMarkdownRender(el, { followScroll: followMarkdown });
      }
      break;
    }
    case 'tool_call': {
      if (m.name === 'task') {
        try {
          const args = JSON.parse(m.arguments || '{}');
          const ref = { task_id: m.id, agent: args.agent || 'sub-agent' };
          createSubagentPanel(ref.agent, args.prompt || '', ref.task_id);
          if (!state._historyTaskIds) state._historyTaskIds = new Map();
          state._historyTaskIds.set(m.id, ref);
        } catch {
          addToolCall(m.name, m.arguments, m.id);
        }
        break;
      }
      if (m.name === 'orchestrate') {
        try {
          const args = JSON.parse(m.arguments || '{}');
          const tasks = Array.isArray(args.tasks) ? args.tasks : [];
          const orchestrateId = `hist-${m.id || Date.now()}`;
          createOrchestratePanel({
            orchestrate_id: orchestrateId,
            task_count: tasks.length,
            layer_count: 0,
            tasks: tasks.map((t) => ({
              id: t.id,
              agent: t.agent,
              depends_on: t.depends_on || [],
              prompt_preview: t.prompt || '',
            })),
          });
          if (!state._historyOrchestrateIds) state._historyOrchestrateIds = new Map();
          state._historyOrchestrateIds.set(m.id, orchestrateId);
        } catch {
          addToolCall(m.name, m.arguments, m.id);
        }
        break;
      }
      addToolCall(m.name, m.arguments, m.id);
      break;
    }
    case 'tool_result': {
      if (state._historyTaskIds && state._historyTaskIds.has(m.id)) {
        const ref = state._historyTaskIds.get(m.id);
        state._historyTaskIds.delete(m.id);
        if (ref && m.subagent_snapshot) {
          restoreSubagentHistorySnapshot(ref, m.subagent_snapshot);
        } else {
          const r = (m.result || '').trimStart();
          const failed =
            m.is_error === true ||
            r.startsWith('task error:') ||
            r.startsWith('[rejected') ||
            /^Sub-agent '.+' (failed|timed out)/.test(r);
          finishSubagentPanel(ref, !failed, {}, { immediate: true });
        }
        break;
      }
      if (state._historyOrchestrateIds && state._historyOrchestrateIds.has(m.id)) {
        const orchestrateId = state._historyOrchestrateIds.get(m.id);
        state._historyOrchestrateIds.delete(m.id);
        const r = (m.result || '').trimStart();
        const summary = parseOrchestrationHistoryResult(r);
        const registry = state.activeOrchestrations;
        const entry = registry && registry.get(orchestrateId);
        if (entry) {
          for (const [taskId, status] of summary.taskStatuses.entries()) {
            if (entry.taskRows.has(taskId)) {
              markOrchestrateTask({ orchestrate_id: orchestrateId, id: taskId }, status);
            }
          }
        }
        finishOrchestratePanel({
          orchestrate_id: orchestrateId,
          aborted: summary.aborted,
          completed: summary.completed,
          failed: summary.failed,
          skipped: summary.skipped,
        });
        break;
      }
      addToolResult('', m.result, m.id, m.duration_ms ?? null, m.is_error === true);
      break;
    }
  }
}

function loadEarlierMessages() {
  const { remaining, chunk: msgs } = splitHistoryLoadChunk(
    state.deferredHistory,
    HISTORY_LOAD_CHUNK_SIZE,
  );
  if (msgs.length === 0) return;
  state.deferredHistory = remaining;
  state._historyTaskIds = null;
  state._historyOrchestrateIds = null;
  const loadMoreRow = document.getElementById('load-more-row');
  const anchor = loadMoreRow ? loadMoreRow.nextElementSibling : dom.chat.firstElementChild;
  if (loadMoreRow) loadMoreRow.remove();
  const existing = [...dom.chat.children];
  dom.chat.replaceChildren();
  const liveExecutionStack = resetExecutionStackState();
  invalidateChatScrollCache();
  dom.chat.classList.add('no-animate');
  state.bulkRenderingChat = true;
  if (state.deferredHistory.length > 0) {
    dom.chat.appendChild(createLoadMoreRow(state.deferredHistory.length));
    invalidateChatScrollCache();
  }
  for (const m of msgs) renderHistoryMessage(m, { followMarkdown: false });
  // Finalize orphaned panels from deferred history.
  if (state._historyTaskIds && state._historyTaskIds.size > 0) {
    for (const ref of state._historyTaskIds.values()) {
      finishSubagentPanel(ref, false, {}, { immediate: true });
    }
    state._historyTaskIds = null;
  }
  if (state._historyOrchestrateIds && state._historyOrchestrateIds.size > 0) {
    for (const orchestrateId of state._historyOrchestrateIds.values()) {
      finishOrchestratePanel({ orchestrate_id: orchestrateId, aborted: true });
    }
    state._historyOrchestrateIds = null;
  }
  completeExecutionStack({ immediate: true, durationMs: null });
  for (const el of existing) dom.chat.appendChild(el);
  restoreExecutionStackState(liveExecutionStack);
  invalidateChatScrollCache();
  requestAnimationFrame(() => {
    state.bulkRenderingChat = false;
    dom.chat.classList.remove('no-animate');
    if (anchor) anchor.scrollIntoView({ block: 'start' });
    requestAnimationFrame(syncChatScrollState);
  });
}

function groupMessageText(message): string {
  const role = String(message?.role || 'system');
  const sessionId = message?.session_id ? String(message.session_id) : '';
  const prefix =
    role === 'session'
      ? `[${groupMemberName(sessionId)}] `
      : role === 'main'
        ? `[${tr('common.main')}] `
        : role === 'user'
          ? `[${tr('common.you')}] `
          : role === 'system'
            ? `[${tr('common.system')}] `
            : '';
  return `${prefix}${renderProtocolMentions(String(message?.content || ''))}`;
}

function renderGroupMessage(message): void {
  const role = String(message?.role || 'system');
  const bubbleRole = role === 'user' ? 'user' : role === 'system' ? 'system' : 'assistant';
  const el = addMsg(bubbleRole, groupMessageText(message), message?.timestamp);
  if (bubbleRole === 'assistant') {
    el._rawText = groupMessageText(message);
    scheduleMarkdownRender(el);
  }
}

function isNoReplyGroupResult(value: unknown): boolean {
  let normalized = String(value ?? '').trim();
  if (!normalized) return true;
  if (normalized.startsWith('```')) {
    let inner = normalized.slice(3);
    const newline = inner.indexOf('\n');
    if (newline >= 0) inner = inner.slice(newline + 1);
    inner = inner.trim();
    if (inner.endsWith('```')) inner = inner.slice(0, -3);
    normalized = inner.trim();
  }
  normalized = normalized
    .replace(/^[\s`"'()[\]{}.,;:!?]+|[\s`"'()[\]{}.,;:!?]+$/g, '')
    .toLowerCase();
  return normalized === 'no_reply';
}

function renderGroupHistory(data): void {
  clearCompressionOutcome();
  closeToolDrawer();
  closeSubagentModal();
  closeOrchestrateTaskModal();
  clearReactStatus();
  clearActiveAutoTrace();
  clearPendingPlanAction();
  clearBufferedChatUpdates();
  setAutoFollowChat(true);
  state.pendingImages = [];
  renderImagePreviews();
  state.inputHistoryIndex = -1;
  dom.chat.replaceChildren();
  resetExecutionStackState();
  invalidateChatScrollCache();
  state.deferredHistory = [];
  state.activeSubagentPanels.clear();
  state.activeOrchestrations.clear();
  const messages = Array.isArray(data?.messages) ? data.messages : [];
  if (messages.length > 0) {
    for (const message of messages) renderGroupMessage(message);
  }
  const messageRunIds = new Set(
    messages.map((message) => String(message?.run_id || '')).filter(Boolean),
  );
  const runs = Array.isArray(data?.runs) ? data.runs : [];
  clearGroupRunState();
  for (const run of runs) {
    const runId = String(run?.id || '');
    const status = String(run?.status || '');
    const sessionId = String(run?.session_id || 'session');
    const resultExcerpt = String(run?.result_excerpt ?? '');
    applyGroupRunStatus(runId, status, run?.updated_at, sessionId);
    if (status === 'failed' && run?.error) {
      addError(`[${groupMemberName(sessionId)}] ${run.error}`);
    } else if (
      status === 'completed' &&
      runId &&
      !messageRunIds.has(runId) &&
      !isNoReplyGroupResult(resultExcerpt)
    ) {
      renderGroupMessage({
        role: 'session',
        session_id: sessionId,
        content: resultExcerpt,
        timestamp: run.completed_at || run.updated_at,
        run_id: runId,
      });
    }
  }
  setBusy(state.activeGroupRunIds.size > 0);
  scrollDown(true);
}

function groupMemberRoleLabel(role: GroupMemberDetail['role']): string {
  if (role === 'owner') return tr('common.main');
  if (role === 'admin') return tr('common.admin');
  return tr('common.member');
}

function groupMemberStatus(sessionId: string): string {
  for (const runId of state.activeGroupRunIds) {
    if (state.groupRunSessions.get(runId) === sessionId) return tr('common.running');
  }
  return tr('common.idle');
}

function applyGroupRunStatus(
  runId: unknown,
  status: unknown,
  updatedAt: unknown,
  sessionId: unknown = '',
): boolean {
  const id = String(runId || '');
  if (!id) return false;
  const value = String(status || '');
  const normalizedUpdatedAt = normalizeGroupRunUpdatedAt(updatedAt);
  const normalizedSessionId = String(sessionId || '').trim();
  const current = state.groupRunStatuses.get(id);
  if (!shouldApplyGroupRunStatusUpdate(current, value, normalizedUpdatedAt)) {
    // A terminal status is final. Even if it arrives out of order (older updated_at
    // than the cached status), drop the run from the active set so it can't leave a
    // phantom "running" member or a stuck busy indicator.
    if (isTerminalGroupRunStatus(value)) {
      state.groupRunSessions.delete(id);
      if (state.activeGroupRunIds.delete(id) && state.activeGroupRunIds.size === 0) {
        setBusy(false);
      }
      renderGroupMemberDrawer();
    }
    return false;
  }
  if (normalizedSessionId && !isTerminalGroupRunStatus(value)) {
    state.groupRunSessions.set(id, normalizedSessionId);
  }
  state.groupRunStatuses.set(id, {
    status: value,
    updatedAt: normalizedUpdatedAt,
  });
  if (isTerminalGroupRunStatus(value)) {
    state.groupRunSessions.delete(id);
  }
  if (isActiveGroupRunStatus(value)) {
    state.activeGroupRunIds.add(id);
    setBusy(true);
    renderGroupMemberDrawer();
    return true;
  }
  state.activeGroupRunIds.delete(id);
  if (state.activeGroupRunIds.size === 0) {
    setBusy(false);
  }
  renderGroupMemberDrawer();
  return true;
}

function handleGroupMemberEvent(data): void {
  const event = data?.event || {};
  const sessionId = String(data?.session_id || '');
  if (event.type === 'error') {
    addError(`[${groupMemberName(sessionId)}] ${event.content || 'error'}`);
  }
}

function isActiveGroupConnectionError(content: string): boolean {
  return isRecoverableActiveGroupConnectionError(content, state.activeGroupId);
}

function recoverFromInvalidActiveGroup(): void {
  leaveActiveGroupForSession();
  beginComposerSessionTransition(false, state.activeSessionId);
  state.sessionSwitchInFlight = true;
  reconnectToActiveSession(handleMessage);
}

// ── handleMessage ──

function handleMessage(data) {
  switch (data.type) {
    case 'session_group_list':
      state.sessionGroups = normalizeSessionGroupListPayload(data);
      renderSessionDrawer();
      break;

    case 'group':
      {
        const nextGroupId = String(data.id || '').trim();
        if (!nextGroupId) break;
        state.composerSessionIdentityPending = false;
        clearCompressionOutcome();
        const groupChanged = state.activeGroupId !== '' && state.activeGroupId !== nextGroupId;
        state.activeGroupId = nextGroupId;
        if (groupChanged) {
          clearGroupRunState();
        }
      }
      persistActiveGroupId(state.activeGroupId || '');
      setActiveGroupMembers(data.members, data.member_details, data.pending_votes);
      applyGroupModelConfigurationAfterRosterUpdate(data, true, true);
      state.sessionSwitchInFlight = false;
      syncComposerAvailability();
      updateAttachButton();
      dom.sessionNameEl.textContent = data.name || tr('group.nameFallback');
      dom.sessionIdEl.textContent = state.activeGroupId.slice(0, 12);
      renderSessionDrawer();
      void refreshGroupsList();
      break;

    case 'group_history':
      if (Array.isArray(data.members)) {
        applyGroupModelConfiguration(data, true);
      }
      renderGroupHistory(data);
      break;

    case 'group_model_configuration':
      if (String(data.id || '').trim() === state.activeGroupId) {
        applyGroupModelConfiguration(data, true);
      }
      break;

    case 'group_message':
      if (data.message) {
        renderGroupMessage(data.message);
        scrollDown();
      }
      break;

    case 'group_run_started':
      if (data.run) {
        applyGroupRunStatus(
          data.run.id,
          data.run.status || 'queued',
          data.run.updated_at,
          data.run.session_id,
        );
      }
      break;

    case 'group_member_event':
      handleGroupMemberEvent(data);
      break;

    case 'group_member_status':
      if (data.status && data.session_id) {
        applyGroupRunStatus(data.run_id, data.status, data.updated_at, data.session_id);
      }
      break;

    case 'group_run_completed': {
      const applied = applyGroupRunStatus(
        data.run_id,
        data.status || 'completed',
        data.updated_at ?? data.completed_at,
        data.session_id,
      );
      if (applied && data.error) {
        addError(`[${groupMemberName(String(data.session_id || ''))}] ${data.error}`);
      }
      break;
    }

    case 'session_list':
      state.sessions = normalizeSessionListPayload(data);
      if (state.activeGroupId) {
        setActiveGroupMembers(state.activeGroupMembers);
      }
      renderSessionDrawer();
      break;
    case 'session_model_configuration':
      applySessionModelConfiguration(data);
      break;

    case 'session':
      if (!sessionModelPayloadTargetsActiveSession(data, true)) break;
      state.composerSessionIdentityPending = false;
      applySessionModelFields(data, false);
      completeComposerSessionTransition();
      clearCompressionOutcome();
      state.activeGroupId = '';
      state.groupReturnSessionId = '';
      persistActiveGroupId('');
      clearGroupRunState();
      resetGroupTargetControls();
      state.activeSessionId = data.id;
      persistActiveSessionId(state.activeSessionId || 'main');
      state.sessionSwitchInFlight = false;
      syncComposerAvailability();
      updateAttachButton();
      dom.sessionNameEl.textContent = data.name || tr('common.main');
      dom.sessionIdEl.textContent = data.id.slice(0, 12);
      renderSessionDrawer();
      if (data.usage) {
        state.dailyInputTokens = data.usage.daily_input ?? 0;
        state.dailyOutputTokens = data.usage.daily_output ?? 0;
        state.totalInputTokens = data.usage.total_input ?? 0;
        state.totalOutputTokens = data.usage.total_output ?? 0;
        updateUsageBadge();
      }
      applyViewState(data);
      void refreshSessionsList();
      break;

    case 'todos_state':
      applyTodosState(data);
      break;

    case 'history': {
      clearCompressionOutcome();
      closeToolDrawer();
      closeSubagentModal();
      closeOrchestrateTaskModal();
      clearReactStatus();
      clearActiveAutoTrace();
      clearPendingPlanAction();
      clearBufferedChatUpdates();
      setAutoFollowChat(true);
      state.pendingImages = [];
      renderImagePreviews();
      state.inputHistoryIndex = -1;
      // replaceChildren() avoids the extra HTML parser invocation of
      // `innerHTML = ''` and is slightly friendlier to GC on large chats.
      dom.chat.replaceChildren();
      resetExecutionStackState();
      invalidateChatScrollCache();
      state.deferredHistory = [];
      state.activeSubagentPanels.clear();
      state.activeOrchestrations.clear();
      state._historyTaskIds = null;
      state._historyOrchestrateIds = null;
      const msgs = data.messages || [];
      if (msgs.length === 0) {
        showWelcome();
      } else {
        dom.chat.classList.add('no-animate');
        state.bulkRenderingChat = true;
        let startIdx = 0;
        if (msgs.length > HISTORY_RENDER_LIMIT) {
          startIdx = findHistoryRenderStart(msgs, msgs.length - HISTORY_RENDER_LIMIT);
          state.deferredHistory = msgs.slice(0, startIdx);
          dom.chat.appendChild(createLoadMoreRow(state.deferredHistory.length));
          invalidateChatScrollCache();
        }
        for (let i = startIdx; i < msgs.length; i++) {
          renderHistoryMessage(msgs[i]);
        }
        if (state._historyTaskIds && state._historyTaskIds.size > 0) {
          for (const ref of state._historyTaskIds.values()) {
            finishSubagentPanel(ref, false, {}, { immediate: true });
          }
          state._historyTaskIds = null;
        }
        // Finalize orphaned orchestrate panels that never got a tool_result.
        if (state._historyOrchestrateIds && state._historyOrchestrateIds.size > 0) {
          for (const orchestrateId of state._historyOrchestrateIds.values()) {
            finishOrchestratePanel({ orchestrate_id: orchestrateId, aborted: true });
          }
          state._historyOrchestrateIds = null;
        }
        completeExecutionStack({ immediate: true, durationMs: null });
        requestAnimationFrame(() => {
          state.bulkRenderingChat = false;
          dom.chat.classList.remove('no-animate');
          if (data.pending_plan) renderPendingPlanAction(data.pending_plan);
          scrollDown(true);
        });
      }
      break;
    }

    case 'view_state':
      applyViewState(data);
      break;

    case 'start': {
      if (data.subagent) break;
      clearCompressionOutcomeForNewRound(data.cycle);
      clearActiveAutoTrace();
      if (data.run_mode === 'execute') confirmPendingPlanExecution();
      clearPendingPlanAction();
      supersedeTaskPlanPanel(data.round, data.cycle);
      const isNewTurn = !state.busy || state.currentRoundStartedAt === 0;
      setBusy(true);
      if (isNewTurn) {
        completeExecutionStack({ immediate: true });
        state.currentRoundStartedAt = performance.now();
        state.currentRoundFirstTokenAt = 0;
      }
      finishAssistantStream({ discardIfEmpty: true });
      beginAssistantStream();
      if (data.react_visible && data.phase) {
        showReactStatus(data.phase, data.cycle);
      }
      break;
    }

    case 'auto_trace':
      applyTopLevelAutoTrace(data);
      break;

    case 'task_plan':
      applyTaskPlan(data);
      break;

    case 'plan_ready':
      renderPendingPlanAction(data);
      break;

    case 'context_compressed':
      applyCompressionOutcome({
        outcome: 'compressed',
        saved_tokens: data.saved_tokens,
        saved_percent: data.saved_percent,
      });
      addSystem(
        `Context auto-compressed: removed ${data.messages_removed || 0} messages, token estimate ${data.before_estimate || 0} -> ${data.after_estimate || 0}`,
      );
      break;

    case 'context_compress_skipped':
      applyCompressionOutcome({
        outcome: 'skipped',
        reason: data.reason,
      });
      break;

    case 'context_pruned':
      addSystem(
        `Context pruned to fit budget: removed ${data.messages_removed || 0} additional messages`,
      );
      break;

    case 'context_compress_failed':
      applyCompressionOutcome({
        outcome: 'failed',
        reason: data.error,
      });
      addError(`Context auto-compress failed: ${data.error || 'unknown error'}`);
      break;

    case 'delta':
      if (data.subagent) break;
      if (data.content) markCurrentRoundFirstTokenAt();
      if (state.currentMsg) {
        state.pendingAssistantText += data.content;
        scheduleFlush();
      }
      break;

    case 'done': {
      clearCompressionOutcome();
      const finishedAssistantMsg = finishAssistantStream({ discardIfEmpty: true });
      const activeReasoningPanel = state.reasoningPanel;
      finishReasoningStream();
      if (activeReasoningPanel) {
        activeReasoningPanel.classList.remove('reasoning-active');
        const body = activeReasoningPanel.querySelector('.reasoning-body') as Element | null;
        const chevron = activeReasoningPanel.querySelector('.chevron') as Element | null;
        if (finalizeOrDiscardLiveReasoningPanel(activeReasoningPanel)) {
          setTimeout(() => {
            if (body) animateCollapsibleSection(body, false);
            if (chevron) chevron.classList.remove('open');
          }, 600);
        }
      }
      requestClearReactStatus();
      finishTaskPlanPanel();
      completeExecutionStack({
        durationMs: state.currentRoundStartedAt
          ? Math.max(1, performance.now() - state.currentRoundStartedAt)
          : null,
      });
      state.reasoningPanel = null;
      if (data.daily_input_tokens != null) {
        state.dailyInputTokens = data.daily_input_tokens;
        state.dailyOutputTokens = data.daily_output_tokens ?? 0;
        state.totalInputTokens = data.total_input_tokens ?? 0;
        state.totalOutputTokens = data.total_output_tokens ?? 0;
        updateUsageBadge();
      }
      if (data.round_input_tokens != null || data.round_output_tokens != null) {
        const firstTokenMs = state.currentRoundFirstTokenAt
          ? Math.max(0, state.currentRoundFirstTokenAt - state.currentRoundStartedAt)
          : null;
        appendRoundUsage(
          finishedAssistantMsg,
          data.round_input_tokens ?? 0,
          data.round_output_tokens ?? 0,
          firstTokenMs,
        );
      }
      resetRoundTimers();
      setBusy(false);
      break;
    }

    case 'react_phase':
      if (data.phase === 'analyze') {
        clearCompressionOutcomeForNewAnalyzeCycle(data.cycle ?? 0);
      }
      showReactStatus(data.phase, data.cycle);
      break;

    case 'thinking_start': {
      if (!state.showReasoning) break;
      if (data.subagent) {
        startSubagentReasoning({ task_id: data.task_id, agent: data.subagent });
        break;
      }
      const panel = document.createElement('div');
      panel.className = 'reasoning-panel reasoning-active';
      const header = document.createElement('button');
      header.type = 'button';
      header.className = 'reasoning-header';
      header.dataset.action = 'toggle-tool';
      header.setAttribute('aria-expanded', 'true');
      header.innerHTML = `
          <span class="reasoning-icon">${iconMarkup('reasoning')}</span>
          <span class="reasoning-label" data-i18n="common.reasoning">${tr('common.reasoning')}</span>
          <span class="reasoning-status" data-i18n="execution.reasoningActive">${tr('execution.reasoningActive')}</span>
          <span class="chevron open">${iconMarkup('chevron-right')}</span>
      `;
      const body = document.createElement('div');
      body.className = 'reasoning-body show';
      linkCollapsibleControl(header, body, 'reasoning-body');
      panel.appendChild(header);
      panel.appendChild(body);
      const currentRow = state.currentMsg ? state.currentMsg.closest('.msg-row') : null;
      mountExecutionPanel(panel, 'reasoning', currentRow);
      invalidateChatScrollCache();
      pinReactStatusToBottom();
      animatePanelIn(panel);
      state.reasoningPanel = panel;
      hideWelcome();
      scrollDown();
      break;
    }

    case 'thinking_delta':
      if (data.content && !data.subagent) {
        markCurrentRoundFirstTokenAt();
      }
      if (!state.showReasoning) break;
      if (data.subagent) {
        appendSubagentReasoning(
          { task_id: data.task_id, agent: data.subagent },
          data.content || '',
        );
        break;
      }
      if (state.reasoningPanel) {
        state.pendingReasoningText += data.content;
        scheduleFlush();
      }
      break;

    case 'thinking_done':
      if (!state.showReasoning) {
        finishReasoningStream();
        state.reasoningPanel = null;
        break;
      }
      if (data.subagent) {
        finishSubagentReasoning({ task_id: data.task_id, agent: data.subagent });
        break;
      }
      if (state.reasoningPanel) {
        finishReasoningStream();
        const reasoningPanel = state.reasoningPanel;
        reasoningPanel.classList.remove('reasoning-active');
        const body = reasoningPanel.querySelector('.reasoning-body') as Element | null;
        const chevron = reasoningPanel.querySelector('.chevron') as Element | null;
        if (!finalizeOrDiscardLiveReasoningPanel(reasoningPanel)) {
          state.reasoningPanel = null;
          break;
        }
        setTimeout(() => {
          if (body) animateCollapsibleSection(body, false);
          if (chevron) chevron.classList.remove('open');
        }, 600);
        state.reasoningPanel = null;
      }
      break;

    case 'tool_call':
      if (data.subagent) break;
      markCurrentRoundFirstTokenAt();
      setReactActTool(data.name, 0);
      addToolCall(data.name, data.arguments, data.id);
      break;

    case 'tool_progress':
      if (data.subagent) break;
      setReactActTool(data.name, data.elapsed_ms || 0);
      updateToolProgress(data.id, data.elapsed_ms || 0);
      break;

    case 'tool_output':
      if (data.subagent) {
        appendSubagentToolOutput(
          { task_id: data.task_id, agent: data.subagent },
          data.id,
          data.stream,
          data.chunk,
          data.name,
        );
        break;
      }
      appendToolOutput(data.id, data.stream, data.chunk);
      break;

    case 'tool_result':
      if (data.subagent) {
        updateSubagentToolResult(
          { task_id: data.task_id, agent: data.subagent },
          data.id,
          data.duration_ms,
          data.result,
          data.is_error,
          data.name,
        );
        break;
      }
      if (state.reactStatusPhase === 'act' && state.reactStatusToolName === data.name) {
        state.reactStatusElapsedMs = data.duration_ms || state.reactStatusElapsedMs;
        renderReactStatus();
      }
      addToolResult(
        data.name,
        data.result,
        data.id,
        data.duration_ms ?? null,
        data.is_error === true,
      );
      break;

    case 'task_started':
      createSubagentPanel(data.agent, data.prompt, data.task_id);
      break;
    case 'task_progress':
      updateSubagentProgress({ task_id: data.task_id, agent: data.agent }, data.cycle);
      break;
    case 'task_tool':
      addSubagentTool(
        { task_id: data.task_id, agent: data.agent },
        data.tool,
        data.id,
        data.arguments,
      );
      break;
    case 'task_completed':
      finishSubagentPanel({ task_id: data.task_id, agent: data.agent }, true, {
        cycles: data.cycles,
        tool_calls: data.tool_calls,
        duration_ms: data.duration_ms,
        input_tokens: data.input_tokens,
        output_tokens: data.output_tokens,
        result_preview: data.result_preview,
        result_excerpt: data.result_excerpt,
      });
      break;
    case 'task_failed':
      finishSubagentPanel({ task_id: data.task_id, agent: data.agent }, false, {
        cycles: data.cycles,
        tool_calls: data.tool_calls,
        duration_ms: data.duration_ms,
        input_tokens: data.input_tokens,
        output_tokens: data.output_tokens,
        error: data.error,
      });
      break;

    case 'orchestrate_started':
      createOrchestratePanel(data);
      break;
    case 'orchestrate_layer':
      updateOrchestrateLayer(data);
      break;
    case 'orchestrate_task_started':
      markOrchestrateTask(data, 'running');
      break;
    case 'orchestrate_task_completed':
      markOrchestrateTask(data, 'completed');
      break;
    case 'orchestrate_task_failed':
      markOrchestrateTask(data, 'failed');
      break;
    case 'orchestrate_task_skipped':
      markOrchestrateTask(data, 'skipped');
      break;
    case 'orchestrate_completed':
      finishOrchestratePanel(data);
      break;

    case 'progress':
      addSystem(data.content);
      break;

    case 'success':
      clearReactStatus();
      completeExecutionStack();
      addSystem(data.content, 'success', { dismissible: data.dismissible === true });
      restoreComposerSessionTransitionWithCapabilities();
      setBusy(false);
      break;

    case 'system':
      clearReactStatus();
      completeExecutionStack();
      addSystem(data.content, 'info', { dismissible: data.dismissible === true });
      restorePendingPlanAction();
      restoreComposerSessionTransitionWithCapabilities();
      setBusy(false);
      break;

    case 'error':
      clearCompressionOutcome();
      finishAssistantStream({ discardIfEmpty: true });
      finishReasoningStream();
      if (state.reasoningPanel) {
        state.reasoningPanel.classList.remove('reasoning-active');
        finalizeOrDiscardLiveReasoningPanel(state.reasoningPanel);
      }
      finishTaskPlanPanel();
      clearReactStatus();
      completeExecutionStack({ failed: true });
      addError(data.content, { dismissible: data.dismissible === true });
      state.reasoningPanel = null;
      resetRoundTimers();
      restorePendingPlanAction();
      restoreComposerSessionTransitionWithCapabilities();
      setBusy(false);
      if (isActiveGroupConnectionError(String(data.content || ''))) {
        recoverFromInvalidActiveGroup();
      }
      break;
  }
}

// ── Event delegation for data-action buttons ──

const handleCommandMenuAction = createCommandMenuActionHandler(sendCmd);

const actionHandlers = {
  'toggle-tools': () => toggleToolsVisibility(),
  'toggle-todos': () => toggleTodosVisibility(),
  'toggle-reasoning': () => toggleReasoningVisibility(),
  'toggle-auto-debug': () => toggleAutoDebug(),
  'nav-settings': () => {
    closeMobileMenu();
    closeShellPopovers();
    closeMobileNavigation({ restoreFocus: true });
    openSettingsPage(state.activeSessionId || 'main');
  },
  'nav-usage': () => {
    closeMobileMenu();
    closeShellPopovers();
    closeMobileNavigation({ restoreFocus: true });
    openUsagePage(state.activeSessionId);
  },
  'close-page': (el) => {
    const overlay = el.closest('.page-overlay');
    if (!(overlay instanceof HTMLElement)) return;
    if (!closeOverlayById(overlay.id, closeSettingsPage, closeUsagePage)) {
      overlay.hidden = true;
    }
  },
  cmd: (el) => {
    const cmd = el.dataset.cmd;
    if (cmd) sendCmd(cmd);
  },
  'cmd-close-menu': handleCommandMenuAction,
  'toggle-mobile-menu': () => toggleMobileMenu(),
  'toggle-view-controls': () => toggleViewControlsMenu(),
  'toggle-mobile-navigation': (el) => toggleMobileNavigation(el),
  'close-mobile-navigation': () => closeMobileNavigation({ restoreFocus: true }),
  'toggle-theme': () => cycleTheme(),
  'toggle-language': () => {
    toggleLanguage();
    closeMobileMenu();
  },
  'show-shortcuts': () => {
    closeMobileMenu();
    toggleShortcutsOverlay();
  },
  'close-tool-drawer': () => closeToolDrawer(),
  'dismiss-system-card': (el) => {
    if (!el) return;
    const row = el.closest('.msg-row.system, .msg-row.error');
    if (row) row.remove();
  },
  'load-earlier': () => loadEarlierMessages(),
  'execute-plan': (el) => executePendingPlan(el),
  'retry-composer-config': () => void refreshComposerAvailability(),
  'open-tool-drawer': (el) => openToolDrawerFromHeader(el),
  'toggle-tool': (el) => toggleTool(el),
  'toggle-execution-stack': (el) => toggleExecutionStack(el),
  'subagent-copy-summary': (el) => copySubagentSummary(el),
  'subagent-open-tool-drawer': (el) => openSubagentToolDrawer(el),
  'open-subagent-modal': (el) => {
    closeOrchestrateTaskModal();
    openSubagentModal(el);
  },
  'close-subagent-modal': () => closeSubagentModal(),
  'open-orchestrate-task-modal': (el) => {
    closeSubagentModal();
    openOrchestrateTaskModal(el);
  },
  'close-orchestrate-task-modal': () => closeOrchestrateTaskModal(),
};

// ── Named global listeners ───────────────────────────────────────────────────
// Named so we can remove them in HMR `dispose` hooks and keep the set of
// active listeners bounded across hot reloads. (In production the page owns
// them for its entire lifetime.)

function handleDocumentClick(e: MouseEvent) {
  const target = e.target;
  if (!(target instanceof Element)) return;

  const el = target.closest('[data-action]');
  if (!el) {
    // Click on overlay backdrop to close
    if (target instanceof HTMLElement && target.classList.contains('page-overlay')) {
      if (!closeOverlayById(target.id, closeSettingsPage, closeUsagePage)) {
        target.hidden = true;
      }
    }
    return;
  }
  const action = (el as HTMLElement).dataset.action;
  const handler = action ? actionHandlers[action] : null;
  if (handler) handler(el);
}

function handleDocumentKeydown(e: KeyboardEvent) {
  if (e.key === 'Tab' && trapToolDrawerFocus(e)) {
    return;
  }
  if (e.key === 'Tab' && trapSubagentModalFocus(e)) {
    return;
  }
  if (e.key === 'Escape') {
    closeToolDrawer();
    closeShellPopovers({ restoreFocus: true });
    closeMobileNavigation({ restoreFocus: true });
    closeSubagentModal();
    closeOrchestrateTaskModal();
    closeSettingsPage();
    closeUsagePage();
    closeShortcutsOverlay();
    return;
  }

  if (e.key === 'Enter' || e.key === ' ') {
    const target = e.target;
    const el = target instanceof Element ? target.closest('[data-action]') : null;
    if (el instanceof HTMLElement && el.getAttribute('role') === 'button') {
      const action = el.dataset.action;
      const handler = action ? actionHandlers[action] : null;
      if (handler) {
        e.preventDefault();
        handler(el);
        return;
      }
    }
  }

  if (trapShortcutsFocus(e)) {
    return;
  }

  if (shortcutsOverlay && !shortcutsOverlay.hidden) {
    if ((e.ctrlKey || e.metaKey) && (e.key === '/' || e.key === 'k' || e.key === 'K')) {
      e.preventDefault();
    }
    return;
  }

  // Avoid stealing keys while the user types. We only treat shortcuts as
  // global when the active element is not an editable field, except for the
  // meta-combo variants which are still safe to intercept (Ctrl/Cmd is rarely
  // part of literal text input).
  const active = document.activeElement;
  const inField =
    active instanceof HTMLElement &&
    (active.tagName === 'INPUT' || active.tagName === 'TEXTAREA' || active.isContentEditable);
  const modKey = e.ctrlKey || e.metaKey;

  // Ctrl/Cmd + / → cycle theme. Matches the shortcut shown in the help
  // overlay; avoids the `?` key which conflicts with text entry.
  if (modKey && e.key === '/') {
    e.preventDefault();
    cycleTheme();
    return;
  }

  // Ctrl/Cmd + K → focus composer. Familiar pattern from Slack/Discord.
  if (modKey && (e.key === 'k' || e.key === 'K')) {
    e.preventDefault();
    if (dom.input) {
      dom.input.focus();
      dom.input.setSelectionRange(dom.input.value.length, dom.input.value.length);
    }
    return;
  }

  // Shift + / (i.e. the `?` key on US layouts) opens the shortcuts overlay.
  // We skip this when inside an editable field so typing a literal `?`
  // into a message still works.
  if (!inField && !modKey && e.key === '?') {
    e.preventDefault();
    toggleShortcutsOverlay();
  }
}

let shortcutsOverlay: HTMLElement | null = null;
let lastFocusBeforeShortcuts: Element | null = null;

function ensureShortcutsOverlay(): HTMLElement {
  if (shortcutsOverlay) return shortcutsOverlay;
  const el = document.createElement('div');
  el.className = 'shortcuts-overlay';
  el.hidden = true;
  el.setAttribute('role', 'dialog');
  el.setAttribute('aria-modal', 'true');
  el.setAttribute('aria-label', 'Keyboard shortcuts');
  el.innerHTML = `
    <div class="shortcuts-panel">
      <div class="shortcuts-header">
        <h2>Keyboard shortcuts</h2>
        <button type="button" class="shortcuts-close" aria-label="Close">${iconMarkup('close')}</button>
      </div>
      <dl class="shortcuts-list">
        <dt><kbd>Enter</kbd></dt><dd>Send message</dd>
        <dt><kbd>Shift</kbd>+<kbd>Enter</kbd></dt><dd>Newline in composer</dd>
        <dt><kbd>↑</kbd> / <kbd>↓</kbd></dt><dd>Browse input history</dd>
        <dt><kbd>Ctrl</kbd>+<kbd>K</kbd></dt><dd>Focus the composer</dd>
        <dt><kbd>Ctrl</kbd>+<kbd>/</kbd></dt><dd>Cycle theme (auto / light / dark)</dd>
        <dt><kbd>?</kbd></dt><dd>Show this help</dd>
        <dt><kbd>Esc</kbd></dt><dd>Close panels & menus</dd>
      </dl>
      <p class="shortcuts-hint">Press <kbd>Esc</kbd> to close.</p>
    </div>
  `;
  el.addEventListener('click', (ev) => {
    if (matchesOverlayDismissTarget(ev.target, el, '.shortcuts-close')) {
      closeShortcutsOverlay();
    }
  });
  document.body.appendChild(el);
  shortcutsOverlay = el;
  return el;
}

function getShortcutsFocusableElements(): HTMLElement[] {
  if (!shortcutsOverlay) return [];
  const selector = [
    'button:not([disabled])',
    '[href]',
    'input:not([disabled])',
    'select:not([disabled])',
    'textarea:not([disabled])',
    '[tabindex]:not([tabindex="-1"])',
  ].join(',');
  return Array.from(shortcutsOverlay.querySelectorAll<HTMLElement>(selector)).filter(
    (el) => !el.hasAttribute('hidden') && el.getAttribute('aria-hidden') !== 'true',
  );
}

function trapShortcutsFocus(e: KeyboardEvent): boolean {
  if (e.key !== 'Tab' || !shortcutsOverlay || shortcutsOverlay.hidden) return false;
  const focusable = getShortcutsFocusableElements();
  if (focusable.length === 0) {
    e.preventDefault();
    shortcutsOverlay.focus();
    return true;
  }

  const first = focusable[0];
  const last = focusable[focusable.length - 1];
  const active = document.activeElement;
  if (!(active instanceof HTMLElement) || !shortcutsOverlay.contains(active)) {
    e.preventDefault();
    first.focus();
    return true;
  }
  if (e.shiftKey && active === first) {
    e.preventDefault();
    last.focus();
    return true;
  }
  if (!e.shiftKey && active === last) {
    e.preventDefault();
    first.focus();
    return true;
  }
  return false;
}

function toggleShortcutsOverlay(): void {
  const el = ensureShortcutsOverlay();
  if (el.hidden) {
    lastFocusBeforeShortcuts = document.activeElement;
    el.hidden = false;
    const close = el.querySelector('.shortcuts-close');
    if (close instanceof HTMLElement) close.focus();
  } else {
    closeShortcutsOverlay();
  }
}

function closeShortcutsOverlay(): void {
  if (shortcutsOverlay && !shortcutsOverlay.hidden) {
    shortcutsOverlay.hidden = true;
    // Restore focus to whatever the user had active before opening; falling
    // back to the composer is nicer than leaving focus on <body>.
    if (lastFocusBeforeShortcuts instanceof HTMLElement && lastFocusBeforeShortcuts.isConnected) {
      lastFocusBeforeShortcuts.focus();
    } else if (dom.input) {
      dom.input.focus();
    }
    lastFocusBeforeShortcuts = null;
  }
}

function handleWindowResizeMenu() {
  if (window.innerWidth > 768) closeShellPopovers();
  syncResponsiveNavigation();
  syncToolDrawerResponsiveState();
}

function handleJumpToLatestClick() {
  jumpToLatest();
}

function handleSessionDrawerToggleClick() {
  if (isMobileViewport()) {
    closeMobileNavigation({ restoreFocus: true });
  } else {
    toggleSessionDrawerExpanded();
  }
}

function handleSessionDrawerNewClick() {
  closeMobileNavigation({ restoreFocus: true });
  void createSession();
}

// Throttle the chat scroll handler to one invocation per animation frame.
// `scroll` fires at device refresh rate on fast wheels/touchpads; running
// `syncChatScrollState` every single event produced redundant state writes
// and jump-to-latest button re-renders. rAF collapses bursts into a single
// update per frame without adding perceptible latency.
let scrollSyncRafId = 0;
function handleChatScroll() {
  if (scrollSyncRafId) return;
  scrollSyncRafId = requestAnimationFrame(() => {
    scrollSyncRafId = 0;
    // User-driven scroll means any cached scroll-distance read is stale.
    invalidateChatScrollCache();
    syncChatScrollState();
  });
}

// ResizeObserver handles the input composer growing as the user types (auto
// resizing textarea), panels opening/closing inside `#chat`, and the chat
// scroll container itself being resized. We used to pile three `window.resize`
// and two `visualViewport` listeners on top of each other for this, firing
// `getBoundingClientRect` on every burst; a single RO keeps the work O(frame).
let chatResizeObserver: ResizeObserver | null = null;
function installChatResizeObserver(): void {
  if (typeof ResizeObserver !== 'function') return;
  chatResizeObserver = new ResizeObserver(() => {
    invalidateChatScrollCache();
    syncToolDrawerBounds();
  });
  if (dom.chat) chatResizeObserver.observe(dom.chat);
  if (dom.inputArea) chatResizeObserver.observe(dom.inputArea);
}

document.addEventListener('click', handleDocumentClick);
window.addEventListener(CONFIG_SAVED_EVENT, handleComposerConfigSaved);
const unsubscribeLanguageChange = subscribeLanguageChange(refreshLocalizedUi);

// ── Init ──
initTheme();
scheduleBackgroundTask(() => {
  void preloadMarkdownEngine();
});
updateViewToggleButtons();
applyTodosVisibility();
syncToolDrawerBounds();
updateJumpToLatestVisibility();

initImageListeners();
initInputListeners();
initMobileListeners();
void refreshComposerAvailability();

document.addEventListener('keydown', handleDocumentKeydown);
dom.chat.addEventListener('scroll', handleChatScroll, { passive: true });
window.addEventListener('resize', syncToolDrawerBounds);
window.addEventListener('resize', handleWindowResizeMenu);
if (window.visualViewport) {
  window.visualViewport.addEventListener('resize', syncToolDrawerBounds);
  window.visualViewport.addEventListener('scroll', syncToolDrawerBounds);
}
installChatResizeObserver();
if (dom.jumpToLatestBtn) {
  dom.jumpToLatestBtn.addEventListener('click', handleJumpToLatestClick);
}
if (dom.sessionDrawerToggleBtn) {
  dom.sessionDrawerToggleBtn.addEventListener('click', handleSessionDrawerToggleClick);
}
if (dom.sessionDrawerNewBtn) {
  dom.sessionDrawerNewBtn.addEventListener('click', handleSessionDrawerNewClick);
}
void refreshSessionsList();
void refreshGroupsList();

// Vite HMR: remove global listeners on module dispose so hot reloads don't
// accumulate duplicate handlers in the dev build. No-op in production.
if (import.meta.hot) {
  import.meta.hot.dispose(() => {
    if (scrollSyncRafId) {
      cancelAnimationFrame(scrollSyncRafId);
      scrollSyncRafId = 0;
    }
    cancelToolDrawerBoundsSync();
    if (chatResizeObserver) {
      chatResizeObserver.disconnect();
      chatResizeObserver = null;
    }
    disposeTheme();
    unsubscribeLanguageChange();
    cancelReconnect();
    document.removeEventListener('click', handleDocumentClick);
    window.removeEventListener(CONFIG_SAVED_EVENT, handleComposerConfigSaved);
    document.removeEventListener('keydown', handleDocumentKeydown);
    dom.chat.removeEventListener('scroll', handleChatScroll);
    window.removeEventListener('resize', syncToolDrawerBounds);
    window.removeEventListener('resize', handleWindowResizeMenu);
    if (window.visualViewport) {
      window.visualViewport.removeEventListener('resize', syncToolDrawerBounds);
      window.visualViewport.removeEventListener('scroll', syncToolDrawerBounds);
    }
    if (dom.jumpToLatestBtn) {
      dom.jumpToLatestBtn.removeEventListener('click', handleJumpToLatestClick);
    }
    if (dom.sessionDrawerToggleBtn) {
      dom.sessionDrawerToggleBtn.removeEventListener('click', handleSessionDrawerToggleClick);
    }
    if (dom.sessionDrawerNewBtn) {
      dom.sessionDrawerNewBtn.removeEventListener('click', handleSessionDrawerNewClick);
    }
  });
}

void loadAppVersion();
connect(handleMessage);
prefetchPageChunks();
