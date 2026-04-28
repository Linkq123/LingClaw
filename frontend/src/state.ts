import type { ImageAttachment, HistoryMessage, ReactPhase } from './types.js';

// ── DOM refs ──

export interface DomRefs {
  chat: HTMLElement | null;
  input: HTMLTextAreaElement | null;
  inputArea: HTMLElement | null;
  jumpToLatestBtn: HTMLButtonElement | null;
  jumpToLatestBadge: HTMLElement | null;
  stopBtn: HTMLButtonElement | null;
  sendBtn: HTMLButtonElement | null;
  sendIcon: HTMLElement | null;
  connDot: HTMLElement | null;
  connLabel: HTMLElement | null;
  sessionNameEl: HTMLElement | null;
  sessionIdEl: HTMLElement | null;
  headerVersionEl: HTMLElement | null;
  toggleToolsBtn: HTMLButtonElement | null;
  toggleReasoningBtn: HTMLButtonElement | null;
  usageBadge: HTMLElement | null;
  toolDrawer: HTMLElement | null;
  toolDrawerBackdrop: HTMLElement | null;
  toolDrawerTitle: HTMLElement | null;
  toolDrawerMeta: HTMLElement | null;
  toolDrawerArgs: HTMLElement | null;
  toolDrawerResult: HTMLElement | null;
  toolDrawerResultSection: HTMLElement | null;
  attachBtn: HTMLButtonElement | null;
  imagePreviewBar: HTMLElement | null;
  attachPopup: HTMLElement | null;
  attachMenu: HTMLElement | null;
  attachLocalBtn: HTMLButtonElement | null;
  attachUrlBtn: HTMLButtonElement | null;
  attachUrlInput: HTMLElement | null;
  imageUrlField: HTMLInputElement | null;
  imageUrlAddBtn: HTMLButtonElement | null;
  attachUploadStatus: HTMLElement | null;
  imageFileInput: HTMLInputElement | null;
  [key: string]: HTMLElement | null;
}

export const dom: DomRefs = {} as DomRefs;

// ── App state ──

export interface AppState {
  ws: WebSocket | null;
  currentMsg: HTMLElement | null;
  busy: boolean;
  currentSessionId: string;
  reasoningPanel: HTMLElement | null;
  reactStatusRow: HTMLElement | null;
  reactStatusPhase: ReactPhase;
  reactStatusCycle: number | null;
  reactStatusToolName: string;
  reactStatusElapsedMs: number;
  reactPhaseShownAt: number;
  reactPhaseTimer: number;
  reactPhaseQueue: { phase: ReactPhase; cycle: number }[];
  reactPendingClear: boolean;
  reconnectDelay: number;
  reconnectAttempts: number;
  pendingAssistantText: string;
  pendingReasoningText: string;
  flushHandle: number;
  deferredHistory: HistoryMessage[];
  activeToolPanel: HTMLElement | null;
  showTools: boolean;
  showReasoning: boolean;
  autoFollowChat: boolean;
  hasBufferedChatUpdates: boolean;
  unreadMessageCount: number;
  bulkRenderingChat: boolean;
  suppressScrollTracking: boolean;
  currentAppVersion: string;
  imageCapable: boolean;
  s3Capable: boolean;
  uploadToken: string;
  uploadTokenPromise: Promise<string> | null;
  uploadTokenRequestSeq: number;
  pendingImages: ImageAttachment[];
  inputHistory: string[];
  inputHistoryIndex: number;
  inputHistoryDraft: string;
  markdownRenderQueue: HTMLElement[];
  markdownQueueHandle: number;
  activeSubagentPanels: Map<string, HTMLElement>;
  activeOrchestrations: Map<
    string,
    {
      panel: HTMLElement;
      taskRows: Map<string, HTMLElement>;
      taskPanels: Map<string, HTMLElement>;
      taskLayer: Map<string, number>;
      layerCount: number;
      live: boolean;
    }
  >;
  dailyInputTokens: number;
  dailyOutputTokens: number;
  totalInputTokens: number;
  totalOutputTokens: number;
  currentRoundStartedAt: number;
  currentRoundFirstTokenAt: number;
  _historyTaskIds: Map<string, { task_id: string; agent: string }> | null;
  _historyOrchestrateIds: Map<string, string> | null;
}

export const state: AppState = {
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
  uploadTokenRequestSeq: 0,
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
  currentRoundStartedAt: 0,
  currentRoundFirstTokenAt: 0,
  _historyTaskIds: null,
  _historyOrchestrateIds: null,
};

/** Populate dom refs from the live document. Call once after DOMContentLoaded. */
export function initDomRefs() {
  dom.chat = document.getElementById('chat');
  dom.input = document.getElementById('input') as HTMLTextAreaElement | null;
  dom.inputArea = document.getElementById('input-area');
  dom.jumpToLatestBtn = document.getElementById('jump-to-latest') as HTMLButtonElement | null;
  dom.jumpToLatestBadge = document.getElementById('jump-to-latest-badge');
  dom.stopBtn = document.getElementById('stop') as HTMLButtonElement | null;
  dom.sendBtn = document.getElementById('send') as HTMLButtonElement | null;
  dom.sendIcon = document.getElementById('send-icon');
  dom.connDot = document.getElementById('conn-dot');
  dom.connLabel = document.getElementById('conn-label');
  dom.sessionNameEl = document.getElementById('session-name');
  dom.sessionIdEl = document.getElementById('session-id');
  dom.headerVersionEl = document.getElementById('app-version-header');
  dom.toggleToolsBtn = document.getElementById('toggle-tools-btn') as HTMLButtonElement | null;
  dom.toggleReasoningBtn = document.getElementById(
    'toggle-reasoning-btn',
  ) as HTMLButtonElement | null;
  dom.usageBadge = document.getElementById('usage-badge');
  dom.toolDrawer = document.getElementById('tool-drawer');
  dom.toolDrawerBackdrop = document.getElementById('tool-drawer-backdrop');
  dom.toolDrawerTitle = document.getElementById('tool-drawer-title');
  dom.toolDrawerMeta = document.getElementById('tool-drawer-meta');
  dom.toolDrawerArgs = document.getElementById('tool-drawer-args');
  dom.toolDrawerResult = document.getElementById('tool-drawer-result');
  dom.toolDrawerResultSection = document.getElementById('tool-drawer-result-section');
  dom.attachBtn = document.getElementById('attach-btn') as HTMLButtonElement | null;
  dom.imagePreviewBar = document.getElementById('image-preview-bar');
  dom.attachPopup = document.getElementById('attach-popup');
  dom.attachMenu = document.getElementById('attach-menu');
  dom.attachLocalBtn = document.getElementById('attach-local-btn') as HTMLButtonElement | null;
  dom.attachUrlBtn = document.getElementById('attach-url-btn') as HTMLButtonElement | null;
  dom.attachUrlInput = document.getElementById('attach-url-input');
  dom.imageUrlField = document.getElementById('image-url-field') as HTMLInputElement | null;
  dom.imageUrlAddBtn = document.getElementById('image-url-add') as HTMLButtonElement | null;
  dom.attachUploadStatus = document.getElementById('attach-upload-status');
  dom.imageFileInput = document.getElementById('image-file-input') as HTMLInputElement | null;
}
