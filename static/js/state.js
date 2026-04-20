/** @type {Record<string, HTMLElement|null>} */
export const dom = {};

/** Shared mutable application state. */
export const state = {
  ws: null,
  currentMsg: null,
  busy: false,
  currentSessionId: '',
  reasoningPanel: null,
  reactStatusRow: null,
  reactStatusPhase: '',
  reactStatusCycle: null,
  reactStatusToolName: '',
  reactStatusElapsedMs: 0,
  reactPhaseShownAt: 0,
  reactPhaseTimer: 0,
  reactPhaseQueue: [],
  reactPendingClear: false,
  reconnectDelay: 1000,
  reconnectAttempts: 0,
  pendingAssistantText: '',
  pendingReasoningText: '',
  flushHandle: 0,
  deferredHistory: [],
  activeToolPanel: null,
  showTools: true,
  showReasoning: true,
  autoFollowChat: true,
  hasBufferedChatUpdates: false,
  unreadMessageCount: 0,
  bulkRenderingChat: false,
  suppressScrollTracking: false,
  currentAppVersion: '',
  imageCapable: false,
  s3Capable: false,
  uploadToken: '',
  uploadTokenPromise: null,
  pendingImages: [],
  inputHistory: [],
  inputHistoryIndex: -1,
  inputHistoryDraft: '',
  markdownRenderQueue: [],
  markdownQueueHandle: 0,
  activeSubagentPanels: new Map(),
  activeOrchestrations: new Map(),
  dailyInputTokens: 0,
  dailyOutputTokens: 0,
  totalInputTokens: 0,
  totalOutputTokens: 0,
};

/** Populate dom refs from the live document. Call once after DOMContentLoaded. */
export function initDomRefs() {
  dom.chat = document.getElementById('chat');
  dom.input = document.getElementById('input');
  dom.inputArea = document.getElementById('input-area');
  dom.jumpToLatestBtn = document.getElementById('jump-to-latest');
  dom.jumpToLatestBadge = document.getElementById('jump-to-latest-badge');
  dom.stopBtn = document.getElementById('stop');
  dom.sendBtn = document.getElementById('send');
  dom.sendIcon = document.getElementById('send-icon');
  dom.connDot = document.getElementById('conn-dot');
  dom.connLabel = document.getElementById('conn-label');
  dom.sessionNameEl = document.getElementById('session-name');
  dom.sessionIdEl = document.getElementById('session-id');
  dom.headerVersionEl = document.getElementById('app-version-header');
  dom.toggleToolsBtn = document.getElementById('toggle-tools-btn');
  dom.toggleReasoningBtn = document.getElementById('toggle-reasoning-btn');
  dom.usageBadge = document.getElementById('usage-badge');
  dom.toolDrawer = document.getElementById('tool-drawer');
  dom.toolDrawerBackdrop = document.getElementById('tool-drawer-backdrop');
  dom.toolDrawerTitle = document.getElementById('tool-drawer-title');
  dom.toolDrawerMeta = document.getElementById('tool-drawer-meta');
  dom.toolDrawerArgs = document.getElementById('tool-drawer-args');
  dom.toolDrawerResult = document.getElementById('tool-drawer-result');
  dom.toolDrawerResultSection = document.getElementById('tool-drawer-result-section');
  dom.attachBtn = document.getElementById('attach-btn');
  dom.imagePreviewBar = document.getElementById('image-preview-bar');
  dom.attachPopup = document.getElementById('attach-popup');
  dom.attachMenu = document.getElementById('attach-menu');
  dom.attachLocalBtn = document.getElementById('attach-local-btn');
  dom.attachUrlBtn = document.getElementById('attach-url-btn');
  dom.attachUrlInput = document.getElementById('attach-url-input');
  dom.imageUrlField = document.getElementById('image-url-field');
  dom.imageUrlAddBtn = document.getElementById('image-url-add');
  dom.attachUploadStatus = document.getElementById('attach-upload-status');
  dom.imageFileInput = document.getElementById('image-file-input');
}
