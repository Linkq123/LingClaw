import React, { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import type {
  AppConfig,
  ConfigApiResponse,
  DiscoveredAgentInfo,
  McpCatalogPrompt,
  McpCatalogResource,
  McpCatalogResponse,
  McpCatalogServer,
  McpCatalogTool,
  McpServerConfig,
  S3Config,
  SessionSkillInfo,
  SessionSkillsApiResponse,
} from '../types/config.js';
import {
  CONFIG_SAVED_EVENT,
  acceptComposerConfigRevision,
  getComposerConnectionGeneration,
  refreshComposerAvailability,
} from '../composerAvailability.js';
import {
  validateMcpCwdValue,
  validateMcpConfigDraftShape,
  buildModelOptions,
  isBuiltinProviderName,
} from '../settingsValidation.js';
import { normalizeModelsConfig } from './settingsModels.js';
import { subscribeLanguageChange, tr } from '../i18n.js';
import { iconHref } from '../icons.js';
import type { IconName } from '../icons.js';
import {
  createConsoleTransitionController,
  type ConsoleTransitionController,
} from './consoleTransition.js';
import { UsageView } from './UsagePage.js';
import { ModelsConsole } from './ModelsConsole.js';
import { resumeChatScrollTracking, suspendChatScrollTracking } from '../scroll.js';

// ── Module-level bridge (imperative open/close from main.ts) ──────────────────

let _open: (() => void) | null = null;
let _close: (() => void) | null = null;
// When the module is loaded lazily, the React component hasn't mounted yet
// the first time `openSettingsPage` is called. Remember the intent so the
// component can honour it as soon as its mount effect runs.
let pendingOpen = false;
export type ConsoleRoute =
  | { page: 'settings'; sessionId: string; section: SettingsSection }
  | { page: 'usage'; sessionId: string };
let pendingRoute: ConsoleRoute = {
  page: 'settings',
  sessionId: 'main',
  section: 'tab-general',
};

function routeSection(route: ConsoleRoute): ConsoleSection {
  return route.page === 'usage' ? 'tab-usage' : route.section;
}

export function openSettingsPage(sessionId?: string, initialSection?: SettingsSection): void {
  pendingRoute = {
    page: 'settings',
    sessionId: sessionId?.trim() || 'main',
    section: initialSection || 'tab-general',
  };
  if (_open) _open();
  else pendingOpen = true;
}

export function openUsageConsolePage(sessionId?: string): void {
  pendingRoute = { page: 'usage', sessionId: sessionId?.trim() || 'main' };
  if (_open) _open();
  else pendingOpen = true;
}

export function closeSettingsPage(): void {
  pendingOpen = false;
  _close?.();
}

export const closeConsolePage = closeSettingsPage;
// ── Helpers ───────────────────────────────────────────────────────────────────

type TriBool = boolean | undefined;

function hasActiveConsoleEscapeLayer(documentRef: Document): boolean {
  return Array.from(
    documentRef.querySelectorAll<HTMLElement>(
      '[aria-modal="true"], [data-console-escape-layer="true"]',
    ),
  ).some((modal) => {
    let current: HTMLElement | null = modal;
    while (current) {
      if (current.hidden || current.inert || current.getAttribute('aria-hidden') === 'true') {
        return false;
      }
      current = current.parentElement;
    }
    return true;
  });
}

async function fetchLatestConfigResponse(init: RequestInit = {}): Promise<ConfigApiResponse> {
  for (let attempt = 0; attempt < 2; attempt += 1) {
    const connectionGeneration = getComposerConnectionGeneration();
    const response = await fetch('/api/config', { ...init, cache: 'no-store' });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    const data: ConfigApiResponse = await response.json();
    if (connectionGeneration !== getComposerConnectionGeneration()) continue;
    if (acceptComposerConfigRevision(data.configRevision)) return data;
  }
  throw new Error(tr('settings.configChangedWhileLoading'));
}

function dispatchConfigSaved(config: AppConfig, data: ConfigApiResponse): void {
  window.dispatchEvent(
    new CustomEvent(CONFIG_SAVED_EVENT, {
      detail: {
        config,
        explicitPrimaryModelConfigured: data.explicitPrimaryModelConfigured === true,
        configuredModelsAvailable:
          typeof data.configuredModelsAvailable === 'boolean'
            ? data.configuredModelsAvailable
            : undefined,
        configRevision: data.configRevision,
      },
    }),
  );
}

function triStateToString(v: TriBool): string {
  if (v === true) return 'true';
  if (v === false) return 'false';
  return '';
}

function stringToTriBool(s: string): TriBool {
  if (s === 'true') return true;
  if (s === 'false') return false;
  return undefined;
}

function numInputToValue(s: string): number | undefined {
  const t = s.trim();
  if (t === '') return undefined;
  const n = parseInt(t, 10);
  return isNaN(n) ? undefined : n;
}

export type SettingsSection =
  | 'tab-general'
  | 'tab-skills'
  | 'tab-agents'
  | 'tab-models'
  | 'tab-mcp'
  | 'tab-s3';
export type ConsoleSection = SettingsSection | 'tab-usage';
type TabId = ConsoleSection;
type StatusType = 'idle' | 'loading' | 'success' | 'error';
type TabSaveMode = 'config' | 'skills' | 'none';

interface TabMeta {
  id: TabId;
  label: string;
  description: string;
  saveMode: TabSaveMode;
}

const SETTINGS_TAB_ICONS: Record<TabId, IconName> = {
  'tab-general': 'settings',
  'tab-skills': 'package',
  'tab-agents': 'users',
  'tab-models': 'reasoning',
  'tab-mcp': 'workflow',
  'tab-s3': 'database',
  'tab-usage': 'chart',
};

const SETTINGS_TAB_DEFS: ReadonlyArray<{
  id: TabId;
  labelKey: string;
  descriptionKey: string;
  saveMode: TabSaveMode;
}> = [
  {
    id: 'tab-general',
    labelKey: 'settings.tab.general',
    descriptionKey: 'settings.tab.generalDesc',
    saveMode: 'config',
  },
  {
    id: 'tab-models',
    labelKey: 'settings.tab.models',
    descriptionKey: 'settings.tab.modelsDesc',
    saveMode: 'config',
  },
  {
    id: 'tab-agents',
    labelKey: 'settings.tab.agents',
    descriptionKey: 'settings.tab.agentsDesc',
    saveMode: 'config',
  },
  {
    id: 'tab-skills',
    labelKey: 'settings.tab.skills',
    descriptionKey: 'settings.tab.skillsDesc',
    saveMode: 'skills',
  },
  {
    id: 'tab-mcp',
    labelKey: 'settings.tab.mcp',
    descriptionKey: 'settings.tab.mcpDesc',
    saveMode: 'config',
  },
  {
    id: 'tab-s3',
    labelKey: 'settings.tab.s3',
    descriptionKey: 'settings.tab.s3Desc',
    saveMode: 'config',
  },
  {
    id: 'tab-usage',
    labelKey: 'usage.title',
    descriptionKey: 'usage.consoleDescription',
    saveMode: 'none',
  },
];

function settingsTabs(): ReadonlyArray<TabMeta> {
  return SETTINGS_TAB_DEFS.map((tab) => ({
    id: tab.id,
    label: tr(tab.labelKey),
    description: tr(tab.descriptionKey),
    saveMode: tab.saveMode,
  }));
}

function useLanguageVersion(): number {
  const [version, setVersion] = useState(0);
  useEffect(() => subscribeLanguageChange(() => setVersion((current) => current + 1)), []);
  return version;
}

function normalizeConfigForSave(config: AppConfig): AppConfig {
  const finalConfig: AppConfig = {
    ...config,
    models: normalizeModelsConfig(config.models),
  };
  if (!finalConfig.models) delete finalConfig.models;

  const s3 = finalConfig.s3;
  if (!s3?.bucket && !s3?.endpoint) delete finalConfig.s3;

  return finalConfig;
}

function sortForStableSerialize(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(sortForStableSerialize);
  if (!value || typeof value !== 'object') return value;

  const sorted: Record<string, unknown> = {};
  for (const key of Object.keys(value as Record<string, unknown>).sort()) {
    const child = (value as Record<string, unknown>)[key];
    if (child !== undefined) sorted[key] = sortForStableSerialize(child);
  }
  return sorted;
}

function serializeConfigForDirty(config: AppConfig): string {
  return JSON.stringify(sortForStableSerialize(normalizeConfigForSave(config)));
}

function serializeModelsForDirty(models: AppConfig['models']): string {
  return JSON.stringify(sortForStableSerialize(normalizeModelsConfig(models) || null));
}

// Stable role list — extracted to module scope to preserve referential identity
// across AgentsTab renders (prevents unnecessary ModelSelect re-renders).
const AGENT_ROLES: ReadonlyArray<{ key: string; label: string }> = [
  { key: 'primary', label: 'Primary' },
  { key: 'fast', label: 'Fast' },
  { key: 'sub-agent', label: 'Sub-Agent' },
  { key: 'memory', label: 'Memory' },
  { key: 'reflection', label: 'Reflection' },
  { key: 'context', label: 'Context' },
];

const SUB_AGENT_OVERRIDE_PREFIX = 'sub-agent-';

function subAgentOverrideKey(agentName: string): string {
  return `${SUB_AGENT_OVERRIDE_PREFIX}${agentName}`;
}

function subAgentNameFromOverrideKey(key: string): string | null {
  if (!key.startsWith(SUB_AGENT_OVERRIDE_PREFIX) || key === 'sub-agent') return null;
  const agentName = key.slice(SUB_AGENT_OVERRIDE_PREFIX.length);
  return agentName.trim() ? agentName : null;
}

function SettingsRow({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="settings-row">
      <label>{label}</label>
      {children}
    </div>
  );
}

function TriSelect({ value, onChange }: { value: TriBool; onChange: (v: TriBool) => void }) {
  return (
    <select
      value={triStateToString(value)}
      onChange={(e) => onChange(stringToTriBool(e.target.value))}
    >
      <option value="">{tr('common.default')}</option>
      <option value="true">{tr('common.enabled')}</option>
      <option value="false">{tr('common.disabled')}</option>
    </select>
  );
}

const ModelSelect = React.memo(function ModelSelect({
  value,
  options,
  onChange,
}: {
  value: string | undefined;
  options: string[];
  onChange: (v: string) => void;
}) {
  useLanguageVersion();
  const v = value || '';
  const includesValue = v && options.includes(v);
  return (
    <select value={v} onChange={(e) => onChange(e.target.value)}>
      <option value="">-- {tr('common.none')} --</option>
      {options.map((opt) => (
        <option key={opt} value={opt}>
          {opt}
        </option>
      ))}
      {v && !includesValue && <option value={v}>{v} (custom)</option>}
    </select>
  );
});

// Per-role row wrapping ModelSelect. Memoized so editing one agent-model field
// doesn't force the other five rows to re-render. `handleChange` is stabilised
// via useCallback so ModelSelect's own memo can also bail out.
function AgentRoleRowInner({
  roleKey,
  label,
  value,
  options,
  onSetModel,
}: {
  roleKey: string;
  label: string;
  value: string | undefined;
  options: string[];
  onSetModel: (key: string, val: string) => void;
}) {
  const handleChange = useCallback(
    (val: string) => onSetModel(roleKey, val),
    [onSetModel, roleKey],
  );
  return (
    <SettingsRow label={label}>
      <ModelSelect value={value} options={options} onChange={handleChange} />
    </SettingsRow>
  );
}
const AgentRoleRow = React.memo(AgentRoleRowInner);

// ── General Tab ───────────────────────────────────────────────────────────────

function GeneralTab({ config, onChange }: { config: AppConfig; onChange: (c: AppConfig) => void }) {
  const s = config.settings || {};
  const set = (patch: Partial<typeof s>) => onChange({ ...config, settings: { ...s, ...patch } });

  return (
    <>
      <div className="settings-group">
        <div className="settings-group-title">{tr('settings.server')}</div>
        <SettingsRow label={tr('settings.field.port')}>
          <input
            type="number"
            value={s.port ?? ''}
            placeholder="18989"
            onChange={(e) => set({ port: numInputToValue(e.target.value) })}
          />
        </SettingsRow>
      </div>
      <div className="settings-group">
        <div className="settings-group-title">{tr('settings.timeouts')}</div>
        <SettingsRow label={tr('settings.field.execTimeout')}>
          <input
            type="number"
            value={s.execTimeout ?? ''}
            placeholder="30"
            onChange={(e) => set({ execTimeout: numInputToValue(e.target.value) })}
          />
        </SettingsRow>
        <SettingsRow label={tr('settings.field.toolTimeout')}>
          <input
            type="number"
            value={s.toolTimeout ?? ''}
            placeholder="30"
            onChange={(e) => set({ toolTimeout: numInputToValue(e.target.value) })}
          />
        </SettingsRow>
        <SettingsRow label={tr('settings.field.subagentTimeout')}>
          <input
            type="number"
            value={s.subAgentTimeout ?? ''}
            placeholder="300"
            onChange={(e) => set({ subAgentTimeout: numInputToValue(e.target.value) })}
          />
        </SettingsRow>
        <SettingsRow label={tr('settings.field.maxLlmRetries')}>
          <input
            type="number"
            value={s.maxLlmRetries ?? ''}
            placeholder="2"
            onChange={(e) => set({ maxLlmRetries: numInputToValue(e.target.value) })}
          />
        </SettingsRow>
      </div>
      <div className="settings-group">
        <div className="settings-group-title">{tr('settings.context')}</div>
        <SettingsRow label={tr('settings.field.maxContextTokens')}>
          <input
            type="number"
            value={s.maxContextTokens ?? ''}
            placeholder="32000"
            onChange={(e) => set({ maxContextTokens: numInputToValue(e.target.value) })}
          />
        </SettingsRow>
        <SettingsRow label={tr('settings.field.maxOutputBytes')}>
          <input
            type="number"
            value={s.maxOutputBytes ?? ''}
            placeholder="51200"
            onChange={(e) => set({ maxOutputBytes: numInputToValue(e.target.value) })}
          />
        </SettingsRow>
        <SettingsRow label={tr('settings.field.maxFileBytes')}>
          <input
            type="number"
            value={s.maxFileBytes ?? ''}
            placeholder="204800"
            onChange={(e) => set({ maxFileBytes: numInputToValue(e.target.value) })}
          />
        </SettingsRow>
      </div>
      <div className="settings-group">
        <div className="settings-group-title">{tr('settings.features')}</div>
        <SettingsRow label={tr('settings.field.structuredMemory')}>
          <TriSelect value={s.structuredMemory} onChange={(v) => set({ structuredMemory: v })} />
        </SettingsRow>
        <SettingsRow label={tr('settings.field.dailyReflection')}>
          <TriSelect value={s.dailyReflection} onChange={(v) => set({ dailyReflection: v })} />
        </SettingsRow>
        <SettingsRow label={tr('settings.field.stateDigest')}>
          <TriSelect value={s.enableStateDigest} onChange={(v) => set({ enableStateDigest: v })} />
        </SettingsRow>
        <SettingsRow label={tr('settings.field.taskPlan')}>
          <TriSelect value={s.enableTaskPlan} onChange={(v) => set({ enableTaskPlan: v })} />
        </SettingsRow>
        <SettingsRow label={tr('settings.field.enableS3')}>
          <TriSelect value={s.enableS3} onChange={(v) => set({ enableS3: v })} />
        </SettingsRow>
        <SettingsRow label={tr('settings.field.openaiStreamUsage')}>
          <TriSelect
            value={s.openaiStreamIncludeUsage}
            onChange={(v) => set({ openaiStreamIncludeUsage: v })}
          />
        </SettingsRow>
        <SettingsRow label={tr('settings.field.anthropicPromptCaching')}>
          <TriSelect
            value={s.anthropicPromptCaching}
            onChange={(v) => set({ anthropicPromptCaching: v })}
          />
        </SettingsRow>
      </div>
    </>
  );
}

// ── Agents Tab ────────────────────────────────────────────────────────────────

function AgentsTab({
  config,
  onChange,
  discoveredAgents,
}: {
  config: AppConfig;
  onChange: (c: AppConfig) => void;
  discoveredAgents: DiscoveredAgentInfo[];
}) {
  const modelRaw = config.agents?.defaults?.model;
  // Stabilise the model reference so downstream memoization deps are stable.
  const model = useMemo(() => (modelRaw || {}) as Record<string, string | undefined>, [modelRaw]);
  const providersRaw = config.models?.providers;
  // Stabilise the providers reference so downstream memoization deps are stable.
  const providers = useMemo(() => providersRaw || {}, [providersRaw]);
  // Memoize the flattened provider/model list so that typing into other
  // fields doesn't recompute this on every keystroke.
  const allModels = useMemo(() => buildModelOptions(providers), [providers]);
  const [selectedAgentName, setSelectedAgentName] = useState('');

  const setModelValue = useCallback(
    (key: string, val: string) => {
      const currentModel = (config.agents?.defaults?.model || {}) as Record<
        string,
        string | undefined
      >;
      const newModel = { ...currentModel };
      if (val) newModel[key] = val;
      else delete newModel[key];
      onChange({
        ...config,
        agents: {
          ...config.agents,
          defaults: { ...(config.agents?.defaults || {}), model: newModel },
        },
      });
    },
    [config, onChange],
  );

  const subAgentOverrides = useMemo(
    () =>
      Object.entries(model)
        .map(([key, value]) => {
          const agentName = subAgentNameFromOverrideKey(key);
          if (!agentName) return null;
          return { key, agentName, value };
        })
        .filter(
          (entry): entry is { key: string; agentName: string; value: string | undefined } =>
            entry !== null,
        )
        .sort((a, b) => a.agentName.localeCompare(b.agentName)),
    [model],
  );
  const discoveredAgentByName = useMemo(
    () => new Map(discoveredAgents.map((agent) => [agent.name, agent])),
    [discoveredAgents],
  );
  const availableAgentsToAdd = useMemo(() => {
    const existing = new Set(subAgentOverrides.map((entry) => entry.agentName));
    return discoveredAgents.filter((agent) => !existing.has(agent.name));
  }, [discoveredAgents, subAgentOverrides]);
  const defaultNewSubAgentModel = useMemo(
    () => model['sub-agent'] || model.primary || allModels[0] || '',
    [allModels, model],
  );

  useEffect(() => {
    setSelectedAgentName((current) => {
      if (current && availableAgentsToAdd.some((agent) => agent.name === current)) return current;
      return availableAgentsToAdd[0]?.name || '';
    });
  }, [availableAgentsToAdd]);

  // Stable callback so AgentRoleRow.memo can bail out when the config hasn't
  // changed. Reads model from config at call time to avoid stale captures.
  const setModel = useCallback(
    (key: string, val: string) => setModelValue(key, val),
    [setModelValue],
  );

  const setAllModels = useCallback(
    (val: string) => {
      if (!val) return;
      const currentModel = (config.agents?.defaults?.model || {}) as Record<
        string,
        string | undefined
      >;
      const keys = new Set([...Object.keys(currentModel), ...AGENT_ROLES.map(({ key }) => key)]);
      const newModel = Object.fromEntries(Array.from(keys, (key) => [key, val]));
      onChange({
        ...config,
        agents: {
          ...config.agents,
          defaults: { ...(config.agents?.defaults || {}), model: newModel },
        },
      });
    },
    [config, onChange],
  );

  const addSubAgentOverride = useCallback(() => {
    if (!selectedAgentName || !defaultNewSubAgentModel) return;
    setModelValue(subAgentOverrideKey(selectedAgentName), defaultNewSubAgentModel);
  }, [defaultNewSubAgentModel, selectedAgentName, setModelValue]);

  const removeSubAgentOverride = useCallback(
    (key: string) => setModelValue(key, ''),
    [setModelValue],
  );

  return (
    <div className="settings-group settings-agent-defaults">
      <div className="settings-group-title">{tr('settings.agentDefaults')}</div>
      <p className="settings-help-text">{tr('settings.agentDefaultsHelp')}</p>
      <p className="settings-help-text">{tr('settings.subAgentOrder')}</p>
      <div className="agent-model-bulk">
        <SettingsRow label={tr('settings.switchAllModels')}>
          <select
            value=""
            aria-label={tr('settings.switchAllModels')}
            disabled={allModels.length === 0}
            onChange={(event) => setAllModels(event.target.value)}
          >
            <option value="">{tr('settings.switchAllModelsPlaceholder')}</option>
            {allModels.map((option) => (
              <option key={option} value={option}>
                {option}
              </option>
            ))}
          </select>
        </SettingsRow>
      </div>
      <div className="agent-role-grid">
        {AGENT_ROLES.map(({ key, label }) => (
          <AgentRoleRow
            key={key}
            roleKey={key}
            label={label}
            value={(model as Record<string, string | undefined>)[key]}
            options={allModels}
            onSetModel={setModel}
          />
        ))}
      </div>
      <section className="agent-overrides">
        <div className="agent-overrides-title">{tr('settings.perSubAgentOverrides')}</div>
        <div className="agent-overrides-add">
          <select
            value={selectedAgentName}
            onChange={(e) => setSelectedAgentName(e.target.value)}
            disabled={availableAgentsToAdd.length === 0}
          >
            {availableAgentsToAdd.length === 0 ? (
              <option value="">{tr('settings.noDiscoveredAgents')}</option>
            ) : (
              availableAgentsToAdd.map((agent) => (
                <option key={agent.name} value={agent.name}>
                  {agent.name}
                  {agent.source ? ` (${agent.source})` : ''}
                </option>
              ))
            )}
          </select>
          <button
            className="btn-secondary"
            onClick={addSubAgentOverride}
            disabled={!selectedAgentName || !defaultNewSubAgentModel}
          >
            {tr('settings.addSubAgentOverride')}
          </button>
        </div>
        {!defaultNewSubAgentModel && (
          <div className="settings-help-text">{tr('settings.addModelFirst')}</div>
        )}
        {subAgentOverrides.length === 0 ? (
          <div className="settings-help-text">{tr('settings.noSubAgentOverrides')}</div>
        ) : (
          subAgentOverrides.map(({ key, agentName, value }) => {
            const discovered = discoveredAgentByName.get(agentName);
            const label = discovered?.source
              ? `${agentName} (${discovered.source})`
              : `${agentName} (${tr('settings.notDiscovered')})`;
            return (
              <div key={key} className="agent-override-row">
                <div className="agent-override-header">
                  <div className="agent-override-name">{label}</div>
                  <button
                    className="btn-danger-sm"
                    title={tr('settings.removeOverride', { agent: agentName })}
                    aria-label={tr('settings.removeOverride', { agent: agentName })}
                    onClick={() => removeSubAgentOverride(key)}
                  >
                    <svg className="icon" aria-hidden="true" focusable="false">
                      <use href={iconHref('trash')} />
                    </svg>
                  </button>
                </div>
                <ModelSelect
                  value={value}
                  options={allModels}
                  onChange={(val) => setModel(key, val)}
                />
              </div>
            );
          })
        )}
      </section>
    </div>
  );
}

// ── MCP Tab ───────────────────────────────────────────────────────────────────

function localizedTestLabel(
  testState: 'idle' | 'testing' | 'ok' | 'fail',
  fallbackLabel: string,
): string {
  if (testState === 'idle') return tr('settings.test');
  if (testState === 'testing') return tr('settings.testing');
  if (testState === 'fail') {
    return fallbackLabel === 'Error' ? tr('common.error') : tr('common.failed');
  }
  if (fallbackLabel === 'Connected') return tr('settings.connected');
  const toolsMatch = /^(\d+)\s+tools$/i.exec(fallbackLabel);
  if (toolsMatch) return tr('settings.toolsCount', { count: toolsMatch[1] });
  return fallbackLabel;
}

interface McpFormEntry extends McpServerConfig {
  _key: string;
  name: string;
  _argsText: string; // textarea, one per line
  _transportWasExplicit: boolean;
  _enabledWasExplicit: boolean;
  testState: 'idle' | 'testing' | 'ok' | 'fail';
  testLabel: string;
}

let mcpFormKeyCounter = 0;

function nextMcpFormKey(name: string): string {
  mcpFormKeyCounter += 1;
  return `${name}-${mcpFormKeyCounter}`;
}

function inferMcpTransportFromFields(
  command?: string,
  url?: string,
): NonNullable<McpServerConfig['transport']> {
  if (command?.trim()) return 'stdio';
  if (url?.trim()) return 'streamable-http';
  return 'stdio';
}

function inferMcpTransport(
  server: Pick<McpServerConfig, 'transport' | 'command' | 'url'>,
): NonNullable<McpServerConfig['transport']> {
  return server.transport || inferMcpTransportFromFields(server.command, server.url);
}

function newMcpForm(name: string, s: McpServerConfig = {}, previous?: McpFormEntry): McpFormEntry {
  return {
    _key: previous?._key || nextMcpFormKey(name),
    name,
    transport: inferMcpTransport(s),
    command: s.command || '',
    url: s.url || '',
    _argsText: (s.args || []).join('\n'),
    cwd: s.cwd || '',
    timeoutSecs: s.timeoutSecs,
    enabled: s.enabled !== false,
    _transportWasExplicit: s.transport !== undefined,
    _enabledWasExplicit: s.enabled !== undefined,
    env: { ...(s.env || {}) },
    headers: { ...(s.headers || {}) },
    auth: s.auth ? { ...s.auth } : undefined,
    testState: previous?.testState || 'idle',
    testLabel: previous?.testLabel || 'Test',
  };
}

function buildMcpForms(
  servers: Record<string, McpServerConfig> | undefined,
  previousForms: McpFormEntry[] = [],
): McpFormEntry[] {
  const previousByName = new Map(previousForms.map((server) => [server.name, server]));

  return Object.entries(servers || {})
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([name, server]) => newMcpForm(name, server, previousByName.get(name)));
}

function sanitizeMcpNameSegment(raw: string): string {
  let sanitized = '';
  let lastWasUnderscore = false;
  for (const ch of raw) {
    const mapped = /^[a-zA-Z0-9]$/.test(ch) ? ch.toLowerCase() : '_';
    if (mapped === '_') {
      if (lastWasUnderscore) continue;
      lastWasUnderscore = true;
    } else {
      lastWasUnderscore = false;
    }
    sanitized += mapped;
  }
  let output = sanitized.replace(/^_+|_+$/g, '') || 'tool';
  if (!/^[a-z]/.test(output)) output = `t_${output}`;
  return output;
}

function mcpToolMatchesServer(toolId: string, serverId: string): boolean {
  const prefix = 'mcp__';
  if (!toolId.startsWith(prefix)) return false;
  const rest = toolId.slice(prefix.length);
  const separator = rest.indexOf('__');
  if (separator < 0) return false;
  return rest.slice(0, separator) === sanitizeMcpNameSegment(serverId);
}

function mcpToolBelongsToServer(
  toolId: string,
  serverId: string,
  toolServers: Map<string, string>,
): boolean {
  const exactServerId = toolServers.get(toolId);
  return exactServerId ? exactServerId === serverId : mcpToolMatchesServer(toolId, serverId);
}

function mcpPromptKey(prompt: McpCatalogPrompt): string {
  return `${prompt.server}:${prompt.name}`;
}

function buildPromptArgumentsTemplate(argumentsMeta: unknown): string {
  if (Array.isArray(argumentsMeta)) {
    const template: Record<string, string> = {};
    for (const arg of argumentsMeta) {
      if (
        arg &&
        typeof arg === 'object' &&
        typeof (arg as Record<string, unknown>).name === 'string'
      ) {
        template[String((arg as Record<string, unknown>).name)] = '';
      }
    }
    return JSON.stringify(template, null, 2);
  }
  return '{}';
}

function McpServerCardInner({
  server,
  onChange,
  onDelete,
  onTest,
}: {
  server: McpFormEntry;
  onChange: (s: McpFormEntry) => void;
  onDelete: (rowKey: string) => void;
  onTest: (s: McpFormEntry) => void;
}) {
  useLanguageVersion();
  const [newEnvKey, setNewEnvKey] = useState('');
  const [newEnvVal, setNewEnvVal] = useState('');

  const addEnvVar = () => {
    if (!newEnvKey.trim()) return;
    onChange({ ...server, env: { ...(server.env || {}), [newEnvKey.trim()]: newEnvVal } });
    setNewEnvKey('');
    setNewEnvVal('');
  };

  const removeEnvVar = (key: string) => {
    const env = { ...(server.env || {}) };
    delete env[key];
    onChange({ ...server, env });
  };

  const testBtnClass =
    server.testState === 'ok'
      ? 'btn-test test-ok'
      : server.testState === 'fail'
        ? 'btn-test test-fail'
        : server.testState === 'testing'
          ? 'btn-test testing'
          : 'btn-test';
  const testLabel = localizedTestLabel(server.testState, server.testLabel);

  return (
    <div className="provider-card" data-mcp-name={server.name}>
      <div className="provider-card-header">
        <span className="provider-card-name">{server.name}</span>
        <div style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
          <label
            style={{
              fontSize: 11,
              display: 'flex',
              alignItems: 'center',
              gap: 3,
              color: 'var(--dim)',
            }}
          >
            <input
              type="checkbox"
              checked={server.enabled !== false}
              onChange={(e) => onChange({ ...server, enabled: e.target.checked })}
            />{' '}
            {tr('common.enabled')}
          </label>
          <button className={testBtnClass} onClick={() => onTest(server)}>
            {testLabel}
          </button>
          <button
            className="btn-danger-sm"
            title={tr('settings.deleteServer')}
            aria-label={tr('settings.deleteServer')}
            onClick={() => onDelete(server._key)}
          >
            <svg className="icon" aria-hidden="true" focusable="false">
              <use href={iconHref('trash')} />
            </svg>
          </button>
        </div>
      </div>
      <div className="provider-form" style={{ display: 'grid', gap: 8, marginTop: 8 }}>
        <SettingsRow label={tr('settings.field.transport')}>
          <select
            value={server.transport || 'stdio'}
            onChange={(e) =>
              onChange({ ...server, transport: e.target.value as McpServerConfig['transport'] })
            }
          >
            <option value="stdio">stdio</option>
            <option value="streamable-http">streamable-http</option>
          </select>
        </SettingsRow>
        <SettingsRow label={tr('settings.field.url')}>
          <input
            type="text"
            value={server.url || ''}
            placeholder="https://example.com/mcp"
            disabled={(server.transport || 'stdio') === 'stdio'}
            onChange={(e) => onChange({ ...server, url: e.target.value })}
          />
        </SettingsRow>
        <SettingsRow label={tr('settings.field.command')}>
          <input
            type="text"
            value={server.command || ''}
            placeholder="uvx"
            disabled={(server.transport || 'stdio') === 'streamable-http'}
            onChange={(e) => onChange({ ...server, command: e.target.value })}
          />
        </SettingsRow>
        <SettingsRow label={tr('settings.field.argsPerLine')}>
          <textarea
            value={server._argsText}
            rows={3}
            style={{ fontFamily: 'var(--font-mono)', fontSize: 12 }}
            placeholder="One argument per line"
            onChange={(e) => onChange({ ...server, _argsText: e.target.value })}
          />
        </SettingsRow>
        <SettingsRow label={tr('settings.field.cwd')}>
          <input
            type="text"
            value={server.cwd || ''}
            placeholder="Optional working directory"
            onChange={(e) => onChange({ ...server, cwd: e.target.value })}
          />
        </SettingsRow>
        <SettingsRow label={tr('settings.field.timeoutSeconds')}>
          <input
            type="number"
            value={server.timeoutSecs ?? ''}
            placeholder="Default"
            onChange={(e) => onChange({ ...server, timeoutSecs: numInputToValue(e.target.value) })}
          />
        </SettingsRow>
      </div>
      <div style={{ marginTop: 10 }}>
        <div style={{ fontSize: 12, fontWeight: 600, marginBottom: 6, color: 'var(--fg)' }}>
          Environment Variables
        </div>
        {Object.entries(server.env || {}).map(([k, v]) => (
          <div
            key={k}
            className="env-entry-form"
            style={{ display: 'flex', gap: 6, alignItems: 'center', marginBottom: 4 }}
          >
            <input
              type="text"
              value={k}
              style={{ flex: 1, minWidth: 80, fontSize: 12 }}
              onChange={(e) => {
                const env = { ...(server.env || {}) };
                const val = env[k];
                delete env[k];
                env[e.target.value] = val;
                onChange({ ...server, env });
              }}
            />
            <input
              type="text"
              value={v}
              style={{ flex: 2, fontSize: 12 }}
              onChange={(e) =>
                onChange({ ...server, env: { ...(server.env || {}), [k]: e.target.value } })
              }
            />
            <button
              className="btn-danger-sm"
              title={tr('common.remove')}
              aria-label={tr('common.remove')}
              onClick={() => removeEnvVar(k)}
            >
              <svg className="icon" aria-hidden="true" focusable="false">
                <use href={iconHref('trash')} />
              </svg>
            </button>
          </div>
        ))}
        <div style={{ display: 'flex', gap: 6, alignItems: 'center', marginTop: 4 }}>
          <input
            type="text"
            value={newEnvKey}
            placeholder="KEY"
            style={{ flex: 1, minWidth: 80, fontSize: 12 }}
            onChange={(e) => setNewEnvKey(e.target.value)}
          />
          <input
            type="text"
            value={newEnvVal}
            placeholder="value"
            style={{ flex: 2, fontSize: 12 }}
            onChange={(e) => setNewEnvVal(e.target.value)}
          />
          <button className="btn-secondary" style={{ fontSize: 11 }} onClick={addEnvVar}>
            + Add
          </button>
        </div>
      </div>
    </div>
  );
}

// Memoize so that editing one MCP card doesn't re-render all the others.
const McpServerCard = React.memo(McpServerCardInner);

function McpTab({
  config,
  sessionId,
  onChange,
  onStatus,
  onPolicyDirtyChange,
  onComposerInsert,
}: {
  config: AppConfig;
  sessionId: string;
  onChange: (c: AppConfig) => void;
  onStatus: (msg: string, type?: string) => void;
  onPolicyDirtyChange?: (dirty: boolean) => void;
  onComposerInsert: (input: HTMLTextAreaElement) => void;
}) {
  const [servers, setServers] = useState<McpFormEntry[]>(() => buildMcpForms(config.mcpServers));
  const [jsonText, setJsonText] = useState(() => JSON.stringify(config.mcpServers || {}, null, 2));
  const [jsonError, setJsonError] = useState('');
  const [jsonDirty, setJsonDirty] = useState(false);
  const [formDirty, setFormDirty] = useState(false);
  const [catalog, setCatalog] = useState<McpCatalogResponse | null>(null);
  const [policyDirty, setPolicyDirty] = useState(false);
  const [policySaving, setPolicySaving] = useState(false);
  const [enabledServers, setEnabledServers] = useState<Set<string>>(() => new Set());
  const [enabledTools, setEnabledTools] = useState<Set<string>>(() => new Set());
  const [confirmMutatingTools, setConfirmMutatingTools] = useState(false);
  const [clientCapabilities, setClientCapabilities] = useState<{
    roots?: boolean;
    sampling?: boolean;
    elicitation?: boolean;
  }>(() => ({}));
  const [promptArgumentText, setPromptArgumentText] = useState<Record<string, string>>({});
  const [catalogLoading, setCatalogLoading] = useState(false);
  const [authBusyServer, setAuthBusyServer] = useState<string | null>(null);
  const mcpResetTimersRef = useRef<Map<string, number>>(new Map());
  const policyDirtyRef = useRef(false);
  const policyRevisionRef = useRef(0);
  const catalogRequestSeqRef = useRef(0);

  const insertIntoComposer = (text: string): boolean => {
    const input = document.getElementById('input') as HTMLTextAreaElement | null;
    if (!input) return false;
    const start = input.selectionStart ?? input.value.length;
    const end = input.selectionEnd ?? input.value.length;
    const needsBreak = input.value.length > 0 && !input.value.endsWith('\n');
    input.setRangeText(`${needsBreak ? '\n\n' : ''}${text}`, start, end, 'end');
    input.dispatchEvent(new Event('input', { bubbles: true }));
    // The workspace is hidden and inert while the full-screen Console is
    // active. Let the shell return first, then focus the composer once it is
    // available again.
    onComposerInsert(input);
    return true;
  };

  useEffect(() => {
    const s = config.mcpServers || {};
    setServers((previousServers) => buildMcpForms(s, previousServers));
    setJsonText(JSON.stringify(config.mcpServers || {}, null, 2));
    setJsonError('');
    setJsonDirty(false);
    setFormDirty(false);
  }, [config.mcpServers]);

  useEffect(() => {
    const resetTimers = mcpResetTimersRef.current;
    return () => {
      for (const timeoutId of resetTimers.values()) {
        window.clearTimeout(timeoutId);
      }
      resetTimers.clear();
    };
  }, []);

  const markPolicyDirty = useCallback(() => {
    policyRevisionRef.current += 1;
    policyDirtyRef.current = true;
    setPolicyDirty(true);
  }, []);

  const loadCatalog = useCallback(
    async (options: { forceApply?: boolean; expectedPolicyRevision?: number } = {}) => {
      const requestId = catalogRequestSeqRef.current + 1;
      catalogRequestSeqRef.current = requestId;
      setCatalogLoading(true);
      try {
        const resp = await fetch(`/api/mcp/catalog?session=${encodeURIComponent(sessionId)}`);
        const data: McpCatalogResponse = await resp.json();
        if (!resp.ok || data.error) throw new Error(data.error || `HTTP ${resp.status}`);
        if (requestId !== catalogRequestSeqRef.current) return;
        if (
          options.expectedPolicyRevision !== undefined &&
          options.expectedPolicyRevision !== policyRevisionRef.current
        ) {
          onStatus(
            'MCP catalog refreshed but not applied because permissions changed during save.',
          );
          return;
        }
        if (policyDirtyRef.current && !options.forceApply) {
          onStatus(
            'MCP catalog refreshed but not applied because permissions have unsaved changes.',
          );
          return;
        }
        setCatalog(data);
        setPromptArgumentText((previous) => {
          const next: Record<string, string> = {};
          for (const prompt of data.prompts || []) {
            const key = mcpPromptKey(prompt);
            next[key] = previous[key] ?? buildPromptArgumentsTemplate(prompt.arguments);
          }
          return next;
        });
        const configuredServerIds = new Set(
          (data.servers || [])
            .filter((server) => server.configuredEnabled)
            .map((server) => server.id),
        );
        const nextEnabledServers = new Set(
          (data.policy?.enabledServers || []).filter((serverId) =>
            configuredServerIds.has(serverId),
          ),
        );
        const toolServers = new Map((data.tools || []).map((tool) => [tool.id, tool.server]));
        const erroredEnabledServerIds = new Set(
          (data.servers || [])
            .filter((server) => nextEnabledServers.has(server.id) && Boolean(server.error))
            .map((server) => server.id),
        );
        const nextEnabledToolIds = new Set(
          (data.policy?.enabledTools || []).filter((toolId) => {
            const exactServerId = toolServers.get(toolId);
            if (exactServerId) return nextEnabledServers.has(exactServerId);
            return Array.from(erroredEnabledServerIds).some((enabledServerId) =>
              mcpToolMatchesServer(toolId, enabledServerId),
            );
          }),
        );
        setEnabledServers(nextEnabledServers);
        setEnabledTools(nextEnabledToolIds);
        setConfirmMutatingTools(Boolean(data.policy?.confirmMutatingTools));
        setClientCapabilities(data.policy?.clientCapabilities || {});
        policyDirtyRef.current = false;
        setPolicyDirty(false);
      } catch (error: unknown) {
        onStatus(`MCP catalog failed: ${(error as Error).message}`, 'error');
      } finally {
        if (requestId === catalogRequestSeqRef.current) setCatalogLoading(false);
      }
    },
    [onStatus, sessionId],
  );

  useEffect(() => {
    void loadCatalog();
  }, [loadCatalog]);

  useEffect(() => {
    policyDirtyRef.current = policyDirty;
    onPolicyDirtyChange?.(policyDirty);
  }, [onPolicyDirtyChange, policyDirty]);

  useEffect(() => {
    return () => onPolicyDirtyChange?.(false);
  }, [onPolicyDirtyChange]);

  const clearMcpReset = useCallback((rowKey: string) => {
    const timeoutId = mcpResetTimersRef.current.get(rowKey);
    if (timeoutId !== undefined) {
      window.clearTimeout(timeoutId);
      mcpResetTimersRef.current.delete(rowKey);
    }
  }, []);

  const scheduleMcpReset = useCallback(
    (rowKey: string) => {
      clearMcpReset(rowKey);
      const timeoutId = window.setTimeout(() => {
        mcpResetTimersRef.current.delete(rowKey);
        setServers((prev) =>
          prev.map((s) => (s._key === rowKey ? { ...s, testState: 'idle', testLabel: 'Test' } : s)),
        );
      }, 4000);
      mcpResetTimersRef.current.set(rowKey, timeoutId);
    },
    [clearMcpReset],
  );

  const addServer = () => {
    const name = prompt('Enter MCP server name:');
    if (!name) return;
    const trimmed = name.trim();
    if (!trimmed || /[/\s]/.test(trimmed)) {
      onStatus('Server name cannot contain "/" or whitespace.', 'error');
      return;
    }
    if (!/^[a-zA-Z0-9._-]+$/.test(trimmed)) {
      onStatus('Server name may only contain letters, numbers, ".", "-" or "_".', 'error');
      return;
    }
    if (servers.some((s) => s.name === trimmed)) {
      onStatus(`Server "${trimmed}" already exists`, 'error');
      return;
    }
    setServers([
      ...servers,
      newMcpForm(trimmed, { command: '', args: [], env: {}, enabled: true }),
    ]);
    setFormDirty(true);
  };

  const updateServer = useCallback((s: McpFormEntry) => {
    setServers((prev) => prev.map((old) => (old._key === s._key ? s : old)));
    setFormDirty(true);
  }, []);

  const deleteServer = useCallback(
    (rowKey: string) => {
      clearMcpReset(rowKey);
      setServers((prev) => prev.filter((s) => s._key !== rowKey));
      setFormDirty(true);
    },
    [clearMcpReset],
  );

  const applyJson = () => {
    if (formDirty) {
      onStatus(
        'MCP form has unapplied changes. Save or discard them before applying Raw JSON.',
        'error',
      );
      return;
    }
    try {
      const text = jsonText.trim();
      const parsed = text === '' || text === '{}' ? {} : JSON.parse(text);
      validateMcpConfigDraftShape(parsed);
      const newConfig = {
        ...config,
        mcpServers: Object.keys(parsed).length > 0 ? parsed : undefined,
      };
      onChange(newConfig);
      setJsonError('');
      onStatus('Applied MCP JSON', 'success');
    } catch (e: unknown) {
      setJsonError((e as Error).message);
    }
  };

  const testServer = useCallback(
    async (sv: McpFormEntry) => {
      // Update by stable row key so delayed resets cannot hit a newly recreated
      // server with the same name.
      const applyState = (state: McpFormEntry['testState'], label: string) => {
        setServers((prev) =>
          prev.map((s) => (s._key === sv._key ? { ...s, testState: state, testLabel: label } : s)),
        );
      };
      clearMcpReset(sv._key);
      applyState('testing', 'Testing...');
      try {
        const args = sv._argsText
          .split('\n')
          .map((a) => a.trim())
          .filter(Boolean);
        if (sv.cwd) {
          try {
            validateMcpCwdValue(sv.cwd);
          } catch (e: unknown) {
            onStatus((e as Error).message, 'error');
            applyState('idle', 'Test');
            return;
          }
        }
        const resp = await fetch(`/api/config/test-mcp?session=${encodeURIComponent(sessionId)}`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({
            server: sv.name,
            transport: inferMcpTransport(sv),
            command: sv.command,
            url: sv.url || undefined,
            args,
            env: sv.env,
            headers: sv.headers,
            auth: sv.auth,
            cwd: sv.cwd || undefined,
            timeoutSecs: sv.timeoutSecs,
          }),
        });
        const data = await resp.json();
        if (data.ok) applyState('ok', `${data.tools} tools`);
        else {
          applyState('fail', 'Failed');
          if (data.error) onStatus(data.error, 'error');
        }
      } catch (e: unknown) {
        applyState('fail', 'Error');
        onStatus((e as Error).message, 'error');
      }
      scheduleMcpReset(sv._key);
    },
    [clearMcpReset, onStatus, scheduleMcpReset, sessionId],
  );

  // Propagate form state to parent config
  useEffect(() => {
    const mcpServers: Record<string, McpServerConfig> = {};
    for (const sv of servers) {
      const args = sv._argsText
        .split('\n')
        .map((a) => a.trim())
        .filter(Boolean);
      const inferredTransport = inferMcpTransportFromFields(sv.command, sv.url);
      const effectiveTransport = sv.transport || inferredTransport;
      const entry: McpServerConfig = {
        command: sv.command || undefined,
        url: sv.url || undefined,
        args: args.length > 0 ? args : undefined,
        cwd: sv.cwd || undefined,
        timeoutSecs: sv.timeoutSecs,
        env: sv.env && Object.keys(sv.env).length > 0 ? sv.env : undefined,
        headers: sv.headers && Object.keys(sv.headers).length > 0 ? sv.headers : undefined,
        auth: sv.auth,
      };
      if (sv._transportWasExplicit || effectiveTransport !== inferredTransport) {
        entry.transport = effectiveTransport;
      }
      if (sv._enabledWasExplicit || sv.enabled !== true) {
        entry.enabled = sv.enabled;
      }
      mcpServers[sv.name] = entry;
    }
    const newMcp = servers.length > 0 ? mcpServers : undefined;
    if (
      JSON.stringify(sortForStableSerialize(newMcp)) !==
      JSON.stringify(sortForStableSerialize(config.mcpServers))
    ) {
      onChange({ ...config, mcpServers: newMcp });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [servers]);

  const toggleServerPolicy = (server: McpCatalogServer, enabled: boolean) => {
    setEnabledServers((prev) => {
      const next = new Set(prev);
      if (enabled) next.add(server.id);
      else next.delete(server.id);
      return next;
    });
    if (!enabled) {
      const catalogToolServers = new Map(
        (catalog?.tools || []).map((tool) => [tool.id, tool.server]),
      );
      setEnabledTools((prev) => {
        const next = new Set(prev);
        for (const toolId of prev) {
          if (mcpToolBelongsToServer(toolId, server.id, catalogToolServers)) next.delete(toolId);
        }
        return next;
      });
    }
    markPolicyDirty();
  };

  const toggleToolPolicy = (tool: McpCatalogTool, enabled: boolean) => {
    setEnabledServers((prev) => {
      const next = new Set(prev);
      if (enabled) next.add(tool.server);
      return next;
    });
    setEnabledTools((prev) => {
      const next = new Set(prev);
      if (enabled) next.add(tool.id);
      else next.delete(tool.id);
      return next;
    });
    markPolicyDirty();
  };

  const saveMcpPolicy = async () => {
    const saveRevision = policyRevisionRef.current;
    setPolicySaving(true);
    try {
      const resp = await fetch(`/api/mcp/session-policy?session=${encodeURIComponent(sessionId)}`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          enabledServers: Array.from(enabledServers).sort(),
          enabledTools: Array.from(enabledTools).sort(),
          confirmMutatingTools,
          clientCapabilities,
        }),
      });
      const data = await resp.json();
      if (!resp.ok || data.error) throw new Error(data.error || `HTTP ${resp.status}`);
      if (saveRevision !== policyRevisionRef.current) {
        onStatus('Saved MCP session permissions; newer changes are still unsaved.', 'success');
        return;
      }
      onStatus('Saved MCP session permissions', 'success');
      policyDirtyRef.current = false;
      setPolicyDirty(false);
      await loadCatalog({ forceApply: true, expectedPolicyRevision: saveRevision });
    } catch (error: unknown) {
      onStatus(`Save MCP permissions failed: ${(error as Error).message}`, 'error');
    } finally {
      setPolicySaving(false);
    }
  };

  const connectMcpAuth = async (server: McpCatalogServer) => {
    setAuthBusyServer(server.id);
    let authWindow: Window | null = null;
    try {
      authWindow = window.open('about:blank', '_blank');
      if (authWindow) {
        try {
          authWindow.opener = null;
        } catch {
          // Some browsers expose opener as read-only; a valid popup handle is
          // still better than replacing the LingClaw tab with the OAuth URL.
        }
      }
    } catch {
      authWindow = null;
    }
    try {
      const resp = await fetch('/api/mcp/auth/start', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ server: server.id }),
      });
      const data = await resp.json();
      if (!resp.ok || data.error || data.ok === false) {
        throw new Error(data.error || `HTTP ${resp.status}`);
      }
      const authorizationUrl = String(data.authorizationUrl || '');
      if (!authorizationUrl) throw new Error('OAuth authorization URL was not returned');
      if (authWindow) {
        authWindow.location.href = authorizationUrl;
      } else {
        window.location.assign(authorizationUrl);
      }
      onStatus('Opened MCP OAuth authorization. Refresh catalog after completing it.', 'success');
    } catch (error: unknown) {
      if (authWindow && !authWindow.closed) authWindow.close();
      onStatus(`MCP OAuth start failed: ${(error as Error).message}`, 'error');
    } finally {
      setAuthBusyServer(null);
    }
  };

  const disconnectMcpAuth = async (server: McpCatalogServer) => {
    setAuthBusyServer(server.id);
    try {
      const resp = await fetch('/api/mcp/auth/disconnect', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ server: server.id }),
      });
      const data = await resp.json();
      if (!resp.ok || data.error || data.ok === false) {
        throw new Error(data.error || `HTTP ${resp.status}`);
      }
      onStatus(`Disconnected MCP auth for ${server.name}`, 'success');
      await loadCatalog();
    } catch (error: unknown) {
      onStatus(`MCP OAuth disconnect failed: ${(error as Error).message}`, 'error');
    } finally {
      setAuthBusyServer(null);
    }
  };

  const readResource = async (resource: McpCatalogResource) => {
    try {
      const resp = await fetch(`/api/mcp/resource/read?session=${encodeURIComponent(sessionId)}`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ server: resource.server, uri: resource.uri }),
      });
      const data = await resp.json();
      if (!resp.ok || data.error) throw new Error(data.error || `HTTP ${resp.status}`);
      const text = JSON.stringify(data.result, null, 2);
      if (insertIntoComposer(text)) onStatus('Resource inserted into input', 'success');
      else {
        await navigator.clipboard?.writeText(text);
        onStatus('Resource copied to clipboard', 'success');
      }
    } catch (error: unknown) {
      onStatus(`Read resource failed: ${(error as Error).message}`, 'error');
    }
  };

  const getPrompt = async (prompt: McpCatalogPrompt) => {
    try {
      const promptKey = mcpPromptKey(prompt);
      const argumentSource = (promptArgumentText[promptKey] || '{}').trim() || '{}';
      const parsedArguments = JSON.parse(argumentSource) as unknown;
      if (
        parsedArguments === null ||
        Array.isArray(parsedArguments) ||
        typeof parsedArguments !== 'object'
      ) {
        throw new Error('Prompt arguments must be a JSON object.');
      }
      const resp = await fetch(`/api/mcp/prompt/get?session=${encodeURIComponent(sessionId)}`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          server: prompt.server,
          name: prompt.name,
          arguments: parsedArguments,
        }),
      });
      const data = await resp.json();
      if (!resp.ok || data.error) throw new Error(data.error || `HTTP ${resp.status}`);
      const text = JSON.stringify(data.result, null, 2);
      if (insertIntoComposer(text)) onStatus('Prompt inserted into input', 'success');
      else {
        await navigator.clipboard?.writeText(text);
        onStatus('Prompt copied to clipboard', 'success');
      }
    } catch (error: unknown) {
      onStatus(`Get prompt failed: ${(error as Error).message}`, 'error');
    }
  };

  const catalogEnabledServers = new Set(
    (catalog?.servers || [])
      .filter((server) => server.configuredEnabled && server.enabled)
      .map((server) => server.id),
  );
  const sessionResources = (catalog?.resources || []).filter((resource) =>
    catalogEnabledServers.has(resource.server),
  );
  const sessionPrompts = (catalog?.prompts || []).filter((prompt) =>
    catalogEnabledServers.has(prompt.server),
  );

  return (
    <>
      {servers.map((sv) => (
        <McpServerCard
          key={sv._key}
          server={sv}
          onChange={updateServer}
          onDelete={deleteServer}
          onTest={testServer}
        />
      ))}
      <button className="btn-secondary" style={{ marginTop: 10 }} onClick={addServer}>
        {tr('settings.addMcpServer')}
      </button>
      <div className="settings-card" style={{ marginTop: 16 }}>
        <div className="settings-card-title">
          {tr('settings.sessionMcpPermissions')}
          <span style={{ color: 'var(--dim)', fontWeight: 400 }}> · {sessionId}</span>
        </div>
        <div className="settings-card-description">{tr('settings.mcpPermissionsDesc')}</div>
        <div style={{ display: 'flex', gap: 8, alignItems: 'center', margin: '10px 0' }}>
          <button className="btn-secondary" type="button" onClick={() => void loadCatalog()}>
            {catalogLoading ? tr('settings.loading') : tr('settings.refreshCatalog')}
          </button>
          <button
            className="btn-primary"
            type="button"
            disabled={!policyDirty || policySaving}
            onClick={() => void saveMcpPolicy()}
          >
            {policySaving ? tr('settings.saving') : tr('settings.saveMcpPermissions')}
          </button>
          {policyDirty && (
            <span style={{ color: 'var(--warn)', fontSize: 12 }}>
              {tr('settings.unsavedShort')}
            </span>
          )}
        </div>
        <label style={{ display: 'flex', gap: 6, alignItems: 'center', fontSize: 12 }}>
          <input
            type="checkbox"
            checked={confirmMutatingTools}
            onChange={(e) => {
              setConfirmMutatingTools(e.target.checked);
              markPolicyDirty();
            }}
          />
          {tr('settings.confirmMutatingTools')}
        </label>
        <label
          style={{
            display: 'flex',
            gap: 6,
            alignItems: 'center',
            fontSize: 12,
            marginTop: 8,
          }}
        >
          <input
            type="checkbox"
            checked={Boolean(clientCapabilities.roots)}
            onChange={(e) => {
              setClientCapabilities((prev) => ({ ...prev, roots: e.target.checked }));
              markPolicyDirty();
            }}
          />
          {tr('settings.exposeWorkspaceRoot')}
        </label>
        {(catalog?.servers || []).map((server) => (
          <div className="provider-card" key={server.id} style={{ marginTop: 10 }}>
            <div className="provider-card-header">
              <span className="provider-card-name">
                {server.name} · {server.transport}
              </span>
              <div style={{ display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap' }}>
                {server.transport === 'streamable-http' && (
                  <button
                    className="btn-secondary"
                    type="button"
                    disabled={
                      authBusyServer === server.id ||
                      (!server.configuredEnabled && !server.authenticated)
                    }
                    onClick={() =>
                      void (server.authenticated
                        ? disconnectMcpAuth(server)
                        : connectMcpAuth(server))
                    }
                  >
                    {authBusyServer === server.id
                      ? 'Working...'
                      : server.authenticated
                        ? 'Disconnect'
                        : 'Connect'}
                  </button>
                )}
                <label style={{ display: 'flex', gap: 6, alignItems: 'center', fontSize: 12 }}>
                  <input
                    type="checkbox"
                    checked={enabledServers.has(server.id)}
                    disabled={!server.configuredEnabled}
                    onChange={(e) => toggleServerPolicy(server, e.target.checked)}
                  />
                  {tr('settings.enabledForSession')}
                </label>
              </div>
            </div>
            {server.error && <div className="json-editor-error">{server.error}</div>}
            <div style={{ color: 'var(--dim)', fontSize: 12, marginTop: 4 }}>
              {server.toolCount || 0} tools · {server.resourceCount || 0} resources ·{' '}
              {server.promptCount || 0} prompts
              {server.transport === 'streamable-http'
                ? ` · ${server.authenticated ? 'authenticated' : 'not authenticated'}`
                : ''}
            </div>
            {(catalog?.tools || [])
              .filter((tool) => tool.server === server.id)
              .map((tool) => (
                <label
                  key={tool.id}
                  style={{
                    display: 'grid',
                    gridTemplateColumns: 'auto 1fr auto',
                    gap: 8,
                    alignItems: 'center',
                    marginTop: 8,
                    fontSize: 12,
                  }}
                >
                  <input
                    type="checkbox"
                    checked={enabledTools.has(tool.id)}
                    onChange={(e) => toggleToolPolicy(tool, e.target.checked)}
                  />
                  <span>
                    <strong>{tool.rawName}</strong>
                    {tool.description && (
                      <span style={{ color: 'var(--dim)' }}> · {tool.description}</span>
                    )}
                  </span>
                  <span style={{ color: tool.readOnly ? 'var(--ok)' : 'var(--warn)' }}>
                    {tool.readOnly ? 'read' : 'mutating'}
                  </span>
                </label>
              ))}
          </div>
        ))}
      </div>
      {Boolean(sessionResources.length || sessionPrompts.length) && (
        <div className="settings-card" style={{ marginTop: 16 }}>
          <div className="settings-card-title">{tr('settings.resourcesAndPrompts')}</div>
          <div className="settings-card-description">{tr('settings.resourcesAndPromptsDesc')}</div>
          {sessionResources.map((resource) => (
            <div className="provider-card" key={`${resource.server}:${resource.uri}`}>
              <div className="provider-card-header">
                <span className="provider-card-name">{resource.name || resource.uri}</span>
                <button className="btn-secondary" onClick={() => void readResource(resource)}>
                  {tr('settings.readResource')}
                </button>
              </div>
              <div style={{ color: 'var(--dim)', fontSize: 12 }}>{resource.uri}</div>
            </div>
          ))}
          {sessionPrompts.map((prompt) => {
            const promptKey = mcpPromptKey(prompt);
            return (
              <div className="provider-card" key={promptKey}>
                <div className="provider-card-header">
                  <span className="provider-card-name">{prompt.name}</span>
                  <button className="btn-secondary" onClick={() => void getPrompt(prompt)}>
                    {tr('settings.getPrompt')}
                  </button>
                </div>
                {prompt.description && (
                  <div style={{ color: 'var(--dim)', fontSize: 12 }}>{prompt.description}</div>
                )}
                <textarea
                  aria-label={`Arguments for ${prompt.name}`}
                  className="json-editor"
                  style={{ minHeight: 72, marginTop: 8 }}
                  spellCheck={false}
                  value={
                    promptArgumentText[promptKey] ?? buildPromptArgumentsTemplate(prompt.arguments)
                  }
                  onChange={(event) =>
                    setPromptArgumentText((previous) => ({
                      ...previous,
                      [promptKey]: event.target.value,
                    }))
                  }
                />
                {prompt.arguments !== undefined && (
                  <details style={{ marginTop: 6 }}>
                    <summary style={{ color: 'var(--dim)', cursor: 'pointer', fontSize: 12 }}>
                      {tr('settings.argumentsSchema')}
                    </summary>
                    <pre className="json-editor" style={{ marginTop: 6 }}>
                      {JSON.stringify(prompt.arguments, null, 2)}
                    </pre>
                  </details>
                )}
              </div>
            );
          })}
        </div>
      )}
      <details style={{ marginTop: 16 }}>
        <summary style={{ fontSize: 12, color: 'var(--dim)', cursor: 'pointer' }}>
          {tr('settings.advancedRawJson')}
        </summary>
        <div className="json-editor-wrap" style={{ marginTop: 8 }}>
          <textarea
            className={`json-editor${jsonDirty && jsonError ? ' has-error' : ''}`}
            spellCheck={false}
            value={jsonText}
            onChange={(e) => {
              setJsonText(e.target.value);
              setJsonDirty(true);
              setJsonError('');
            }}
          />
          {jsonError && <div className="json-editor-error">{jsonError}</div>}
          <button className="btn-secondary" style={{ marginTop: 6 }} onClick={applyJson}>
            {tr('settings.applyJson')}
          </button>
        </div>
      </details>
    </>
  );
}

// ── Skills Tab ───────────────────────────────────────────────────────────────

function normalizeSessionSkill(skill: SessionSkillInfo): SessionSkillInfo {
  return {
    id: String(skill.id || ''),
    name: String(skill.name || skill.id || ''),
    description: skill.description ? String(skill.description) : '',
    path: String(skill.path || ''),
    group: skill.group ? String(skill.group) : '',
    enabled: skill.enabled !== false,
  };
}

function sortedEnabledIds(skills: SessionSkillInfo[]): string[] {
  return skills
    .filter((skill) => skill.enabled)
    .map((skill) => skill.id)
    .sort();
}

function sortedSkillIds(skills: SessionSkillInfo[]): string[] {
  return skills.map((skill) => skill.id).sort();
}

function sameStringList(a: string[], b: string[]): boolean {
  if (a.length !== b.length) return false;
  return a.every((value, index) => value === b[index]);
}

function SkillsTab({
  sessionId,
  onDirtyChange,
}: {
  sessionId: string;
  onDirtyChange?: (dirty: boolean) => void;
}) {
  const targetSessionId = sessionId || 'main';
  const [skills, setSkills] = useState<SessionSkillInfo[]>([]);
  const [savedEnabledIds, setSavedEnabledIds] = useState<string[]>([]);
  const [knownSkillIds, setKnownSkillIds] = useState<string[]>([]);
  const [sessionLabel, setSessionLabel] = useState(targetSessionId);
  const [filterText, setFilterText] = useState('');
  const [status, setStatus] = useState<{ message: string; type: StatusType }>({
    message: '',
    type: 'idle',
  });
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);

  const loadSkills = useCallback(
    async (signal?: AbortSignal) => {
      setLoading(true);
      setStatus({ message: tr('settings.loadingSkills'), type: 'loading' });
      try {
        const response = await fetch(
          `/api/session-skills?session=${encodeURIComponent(targetSessionId)}`,
          { cache: 'no-store', signal },
        );
        const data: SessionSkillsApiResponse = await response.json();
        if (!response.ok || data.error) {
          throw new Error(data.error || `HTTP ${response.status}`);
        }
        const nextSkills = (data.skills || []).map(normalizeSessionSkill);
        const nextEnabled = sortedEnabledIds(nextSkills);
        setSkills(nextSkills);
        setSavedEnabledIds(nextEnabled);
        setKnownSkillIds(sortedSkillIds(nextSkills));
        setSessionLabel(data.session?.name || data.session?.id || targetSessionId);
        setStatus({ message: tr('settings.skillsLoaded'), type: 'success' });
      } catch (error: unknown) {
        if ((error as Error).name === 'AbortError') return;
        setStatus({
          message: tr('settings.skillsLoadFailed', { error: (error as Error).message }),
          type: 'error',
        });
      } finally {
        setLoading(false);
      }
    },
    [targetSessionId],
  );

  useEffect(() => {
    const controller = new AbortController();
    void loadSkills(controller.signal);
    return () => controller.abort();
  }, [loadSkills]);

  const currentEnabledIds = useMemo(() => sortedEnabledIds(skills), [skills]);
  const dirty = !sameStringList(currentEnabledIds, savedEnabledIds);
  const enabledCount = currentEnabledIds.length;
  const query = filterText.trim().toLowerCase();
  const visibleSkills = useMemo(
    () =>
      query
        ? skills.filter((skill) =>
            [skill.id, skill.name, skill.description || '', skill.path, skill.group || '']
              .join(' ')
              .toLowerCase()
              .includes(query),
          )
        : skills,
    [query, skills],
  );

  useEffect(() => {
    onDirtyChange?.(dirty);
  }, [dirty, onDirtyChange]);

  useEffect(() => {
    return () => onDirtyChange?.(false);
  }, [onDirtyChange]);

  const setSkillEnabled = useCallback((skillId: string, enabled: boolean) => {
    setSkills((current) =>
      current.map((skill) => (skill.id === skillId ? { ...skill, enabled } : skill)),
    );
  }, []);

  const setAllEnabled = useCallback((enabled: boolean) => {
    setSkills((current) => current.map((skill) => ({ ...skill, enabled })));
  }, []);

  const revertSkills = useCallback(() => {
    const saved = new Set(savedEnabledIds);
    setSkills((current) => current.map((skill) => ({ ...skill, enabled: saved.has(skill.id) })));
    setStatus({ message: tr('settings.skillsReverted'), type: 'idle' });
  }, [savedEnabledIds]);

  const saveSkills = useCallback(async () => {
    setSaving(true);
    setStatus({ message: tr('settings.skillsSaving'), type: 'loading' });
    try {
      const response = await fetch(
        `/api/session-skills?session=${encodeURIComponent(targetSessionId)}`,
        {
          method: 'PUT',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({
            enabledSystemSkills: currentEnabledIds,
            knownSystemSkills: knownSkillIds,
          }),
        },
      );
      const data: SessionSkillsApiResponse = await response.json();
      if (!response.ok || data.error) {
        throw new Error(data.error || `HTTP ${response.status}`);
      }
      const nextSkills = (data.skills || skills).map(normalizeSessionSkill);
      const nextEnabled = sortedEnabledIds(nextSkills);
      setSkills(nextSkills);
      setSavedEnabledIds(nextEnabled);
      setKnownSkillIds(sortedSkillIds(nextSkills));
      setSessionLabel(data.session?.name || data.session?.id || sessionLabel);
      setStatus({ message: tr('settings.skillsSaved'), type: 'success' });
    } catch (error: unknown) {
      setStatus({
        message: tr('settings.skillsSaveFailed', { error: (error as Error).message }),
        type: 'error',
      });
    } finally {
      setSaving(false);
    }
  }, [currentEnabledIds, knownSkillIds, sessionLabel, skills, targetSessionId]);

  const statusClass =
    status.type === 'success'
      ? 'settings-status success'
      : status.type === 'error'
        ? 'settings-status error'
        : 'settings-status';

  return (
    <div className="settings-group skills-settings">
      <div className="settings-group-title">{tr('settings.systemSkills')}</div>
      <div className="skills-toolbar">
        <div className="skills-session-label">
          {tr('settings.sessionLabel')} <code>{sessionLabel}</code>
        </div>
        <span className={statusClass}>{status.message}</span>
      </div>
      <div className="skills-toolbar">
        <input
          type="search"
          value={filterText}
          placeholder={tr('settings.searchSkills')}
          aria-label={tr('settings.searchSkills')}
          onChange={(event) => setFilterText(event.target.value)}
        />
        <button
          className="btn-secondary"
          onClick={() => setAllEnabled(true)}
          disabled={loading || saving}
        >
          {tr('settings.enableAll')}
        </button>
        <button
          className="btn-secondary"
          onClick={() => setAllEnabled(false)}
          disabled={loading || saving}
        >
          {tr('settings.disableAll')}
        </button>
        <button
          className="btn-secondary"
          onClick={revertSkills}
          disabled={!dirty || loading || saving}
        >
          {tr('settings.revert')}
        </button>
        <button className="btn-primary" onClick={saveSkills} disabled={!dirty || loading || saving}>
          {tr('settings.saveSkills')}
        </button>
      </div>
      <div className="skills-summary">
        {tr('settings.skillsSummary', { enabled: enabledCount, total: skills.length })}
      </div>
      <div className="skills-list">
        {visibleSkills.length === 0 ? (
          <div className="skills-empty">
            {loading ? tr('settings.loadingSkills') : tr('settings.noMatchingSkills')}
          </div>
        ) : (
          visibleSkills.map((skill) => (
            <label key={skill.id} className="skill-row">
              <input
                type="checkbox"
                checked={skill.enabled}
                disabled={loading || saving}
                onChange={(event) => setSkillEnabled(skill.id, event.target.checked)}
              />
              <span className="skill-row-body">
                <span className="skill-row-main">
                  <span className="skill-row-name">{skill.name}</span>
                  <code>{skill.id}</code>
                </span>
                {skill.description && (
                  <span className="skill-row-description">{skill.description}</span>
                )}
                <span className="skill-row-path">{skill.path}</span>
              </span>
            </label>
          ))
        )}
      </div>
    </div>
  );
}

// ── S3 Tab ────────────────────────────────────────────────────────────────────

function S3Tab({ config, onChange }: { config: AppConfig; onChange: (c: AppConfig) => void }) {
  const s3 = config.s3 || {};
  const set = (patch: Partial<S3Config>) => onChange({ ...config, s3: { ...s3, ...patch } });

  return (
    <div className="settings-group">
      <div className="settings-group-title">{tr('settings.s3Title')}</div>
      <SettingsRow label={tr('settings.field.endpoint')}>
        <input
          type="text"
          value={s3.endpoint || ''}
          placeholder="https://s3.us-east-1.amazonaws.com"
          onChange={(e) => set({ endpoint: e.target.value || undefined })}
        />
      </SettingsRow>
      <SettingsRow label={tr('settings.field.region')}>
        <input
          type="text"
          value={s3.region || ''}
          placeholder="us-east-1"
          onChange={(e) => set({ region: e.target.value || undefined })}
        />
      </SettingsRow>
      <SettingsRow label={tr('settings.field.bucket')}>
        <input
          type="text"
          value={s3.bucket || ''}
          onChange={(e) => set({ bucket: e.target.value || undefined })}
        />
      </SettingsRow>
      <SettingsRow label={tr('settings.field.accessKey')}>
        <input
          type="text"
          value={s3.accessKey || ''}
          onChange={(e) => set({ accessKey: e.target.value || undefined })}
        />
      </SettingsRow>
      <SettingsRow label={tr('settings.field.secretKey')}>
        <input
          type="password"
          value={s3.secretKey || ''}
          onChange={(e) => set({ secretKey: e.target.value || undefined })}
        />
      </SettingsRow>
      <SettingsRow label={tr('settings.field.prefix')}>
        <input
          type="text"
          value={s3.prefix || ''}
          placeholder="lingclaw/images/"
          onChange={(e) => set({ prefix: e.target.value || undefined })}
        />
      </SettingsRow>
      <SettingsRow label={tr('settings.field.urlExpirySeconds')}>
        <input
          type="number"
          value={s3.urlExpirySecs ?? ''}
          placeholder="604800"
          onChange={(e) => set({ urlExpirySecs: numInputToValue(e.target.value) })}
        />
      </SettingsRow>
      <SettingsRow label={tr('settings.field.lifecycleDays')}>
        <input
          type="number"
          value={s3.lifecycleDays ?? ''}
          placeholder="14"
          onChange={(e) => set({ lifecycleDays: numInputToValue(e.target.value) })}
        />
      </SettingsRow>
    </div>
  );
}

// ── Corrupt config recovery view ─────────────────────────────────────────────

function CorruptConfigView({
  data,
  conflict,
  onDirtyChange,
  onStatus,
  onConflict,
  onReload,
  onReloaded,
}: {
  data: ConfigApiResponse;
  conflict: boolean;
  onDirtyChange: (dirty: boolean) => void;
  onStatus: (msg: string, type?: string) => void;
  onConflict: () => void;
  onReload: () => void;
  onReloaded: (data: ConfigApiResponse) => void;
}) {
  const [rawText, setRawText] = useState(data.raw || '');
  const [hasError, setHasError] = useState(true);
  const [errorMsg, setErrorMsg] = useState(data.parse_error || '');
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    setRawText(data.raw || '');
    setHasError(true);
    setErrorMsg(data.parse_error || '');
    onDirtyChange(false);
  }, [data, onDirtyChange]);

  const save = async () => {
    if (saving || conflict) return;
    if (!rawText.trim()) {
      onStatus('Config is empty', 'error');
      return;
    }
    let parsed: unknown;
    try {
      parsed = JSON.parse(rawText);
      setHasError(false);
      setErrorMsg('');
    } catch (e: unknown) {
      setHasError(true);
      setErrorMsg((e as Error).message);
      onStatus('Fix JSON syntax errors first', 'error');
      return;
    }
    onStatus(tr('settings.saving'));
    setSaving(true);
    try {
      const connectionGeneration = getComposerConnectionGeneration();
      const resp = await fetch('/api/config', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          config: parsed,
          ...(typeof data.configFileEtag === 'string'
            ? { baseConfigFileEtag: data.configFileEtag }
            : {}),
        }),
      });
      const result: ConfigApiResponse = await resp.json();
      if (connectionGeneration !== getComposerConnectionGeneration()) {
        onConflict();
        return;
      }
      if (resp.status === 409) {
        onConflict();
        return;
      }
      if (!resp.ok || result.error) {
        onStatus(result.error || tr('settings.saveFailed'), 'error');
        return;
      }
      onStatus('Saved! Reloading...', 'success');
      setTimeout(() => {
        fetchLatestConfigResponse()
          .then((reloaded) => onReloaded(reloaded))
          .catch(() => {
            // Reload failed (network error or non-2xx response); config was
            // saved but the UI may be stale.
            onStatus('Save succeeded but reload failed. Close and reopen Settings.', 'error');
          });
      }, 600);
    } catch (e: unknown) {
      onStatus(`Save failed: ${(e as Error).message}`, 'error');
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="settings-group">
      <div className="settings-group-title" style={{ color: 'var(--accent-error)' }}>
        {tr('settings.configFileErrorTitle')}
      </div>
      <p style={{ color: 'var(--dim)' }}>{tr('settings.configFileErrorBody')}</p>
      <p style={{ fontSize: 12, color: 'var(--dim)' }}>
        {tr('settings.file')} <code>{data.path}</code>
      </p>
      <div className="json-editor-wrap">
        <textarea
          className={`json-editor${hasError ? ' has-error' : ''}`}
          spellCheck={false}
          style={{ minHeight: 300 }}
          value={rawText}
          onChange={(e) => {
            const nextRawText = e.target.value;
            setRawText(nextRawText);
            onDirtyChange(nextRawText !== (data.raw || ''));
            setHasError(false);
            setErrorMsg('');
          }}
        />
        {errorMsg && <div className="json-editor-error">{errorMsg}</div>}
      </div>
      <div className="settings-footer-actions" style={{ marginTop: 10 }}>
        {conflict && (
          <button className="btn-secondary" type="button" onClick={onReload} disabled={saving}>
            {tr('settings.reloadLatest')}
          </button>
        )}
        <button className="btn-primary" type="button" onClick={save} disabled={saving || conflict}>
          {tr('settings.saveRecover')}
        </button>
      </div>
    </div>
  );
}

// ── Settings shell ───────────────────────────────────────────────────────────

function SettingsShell({
  activeTab,
  tabs,
  status,
  configDirty,
  modelsDraftDirty,
  configConflict,
  skillsDirty,
  mcpDirty,
  corrupt,
  showDiscardConfirm,
  onTabChange,
  onSaveConfig,
  onReloadConfig,
  onRequestClose,
  onCancelDiscard,
  onDiscardChanges,
  children,
}: {
  activeTab: TabId;
  tabs: ReadonlyArray<TabMeta>;
  status: { message: string; type: StatusType };
  configDirty: boolean;
  modelsDraftDirty: boolean;
  configConflict: boolean;
  skillsDirty: boolean;
  mcpDirty: boolean;
  corrupt: boolean;
  showDiscardConfirm: boolean;
  onTabChange: (tab: TabId) => void;
  onSaveConfig: () => void;
  onReloadConfig: () => void;
  onRequestClose: () => void;
  onCancelDiscard: () => void;
  onDiscardChanges: () => void;
  children: React.ReactNode;
}) {
  const activeMeta = tabs.find((tab) => tab.id === activeTab) || tabs[0];
  const titleRef = useRef<HTMLHeadingElement | null>(null);
  const tabRefs = useRef<Partial<Record<TabId, HTMLButtonElement | null>>>({});
  const statusClass =
    status.type === 'success'
      ? 'settings-status success'
      : status.type === 'error'
        ? 'settings-status error'
        : 'settings-status';

  useEffect(() => {
    const timeoutId = window.setTimeout(() => titleRef.current?.focus(), 0);
    return () => window.clearTimeout(timeoutId);
  }, []);

  const focusTab = (tabId: TabId) => {
    window.setTimeout(() => tabRefs.current[tabId]?.focus(), 0);
  };

  const isTabDisabled = (tabId: TabId) =>
    corrupt && tabId !== 'tab-general' && tabId !== 'tab-usage';

  const changeTab = (tabId: TabId) => {
    if (isTabDisabled(tabId)) return;
    onTabChange(tabId);
  };

  const handleTabKeyDown = (event: React.KeyboardEvent<HTMLButtonElement>, tabId: TabId) => {
    const enabledTabs = tabs.filter((tab) => !isTabDisabled(tab.id));
    const currentIndex = enabledTabs.findIndex((tab) => tab.id === tabId);
    if (currentIndex < 0) return;

    let nextIndex = currentIndex;
    if (event.key === 'ArrowRight' || event.key === 'ArrowDown') {
      nextIndex = (currentIndex + 1) % enabledTabs.length;
    } else if (event.key === 'ArrowLeft' || event.key === 'ArrowUp') {
      nextIndex = (currentIndex - 1 + enabledTabs.length) % enabledTabs.length;
    } else if (event.key === 'Home') {
      nextIndex = 0;
    } else if (event.key === 'End') {
      nextIndex = enabledTabs.length - 1;
    } else {
      return;
    }

    event.preventDefault();
    const nextTab = enabledTabs[nextIndex].id;
    changeTab(nextTab);
    focusTab(nextTab);
  };

  const canSaveConfig = !corrupt && activeMeta.saveMode === 'config';
  const hasUnsavedChanges = configDirty || modelsDraftDirty || skillsDirty || mcpDirty;
  const isUsage = activeTab === 'tab-usage';
  const showConfigError = corrupt && !isUsage;
  const dirtySections = [
    configDirty ? 'Config' : '',
    modelsDraftDirty && !configDirty ? tr('settings.tab.models') : '',
    skillsDirty ? tr('settings.tab.skills') : '',
    mcpDirty ? 'MCP' : '',
  ].filter(Boolean);

  return (
    <main className="console-page-surface" aria-labelledby="settings-dialog-title">
      <div className="settings-shell console-shell">
        <aside className="settings-sidebar" aria-label={tr('settings.sectionsAria')}>
          <div className="settings-sidebar-head">
            <div className="console-brand-row">
              <span className="console-brand-mark" aria-hidden="true">
                <img src="/branding/logo-mark.png" alt="" />
              </span>
              <span className="console-brand-copy">
                <strong>LingClaw</strong>
                <span>{tr('console.title')}</span>
              </span>
            </div>
            <button
              className="console-return-button"
              type="button"
              title={tr('console.backToWorkspace')}
              aria-label={tr('console.backToWorkspace')}
              onClick={onRequestClose}
            >
              <svg className="icon" aria-hidden="true">
                <use href="#icon-chevron-left" />
              </svg>
              <span>{tr('console.backToWorkspace')}</span>
            </button>
          </div>
          <label className="settings-mobile-section-picker">
            <span>{tr('settings.sectionPicker')}</span>
            <select value={activeTab} onChange={(event) => changeTab(event.target.value as TabId)}>
              {tabs.map((tab) => (
                <option
                  key={tab.id}
                  value={tab.id}
                  disabled={corrupt && tab.id !== 'tab-general' && tab.id !== 'tab-usage'}
                >
                  {tab.label}
                </option>
              ))}
            </select>
          </label>
          <div id="settings-tabs" className="page-tabs settings-nav" role="tablist">
            {tabs.map((tab, index) => (
              <React.Fragment key={tab.id}>
                {index === 0 || tab.id === 'tab-usage' ? (
                  <span
                    className={`console-nav-group-label${
                      tab.id === 'tab-usage' ? ' is-observe' : ''
                    }`}
                    role="presentation"
                  >
                    {tr(tab.id === 'tab-usage' ? 'console.observability' : 'console.configuration')}
                  </span>
                ) : null}
                <button
                  ref={(node) => {
                    tabRefs.current[tab.id] = node;
                  }}
                  id={`${tab.id}-button`}
                  role="tab"
                  type="button"
                  className={`page-tab settings-nav-item${
                    tab.id === 'tab-usage' ? ' console-nav-observe' : ''
                  }${activeTab === tab.id ? ' active' : ''}`}
                  data-tab={tab.id}
                  aria-selected={activeTab === tab.id}
                  aria-controls={`${tab.id}-panel`}
                  aria-label={tab.label}
                  title={tab.label}
                  disabled={isTabDisabled(tab.id)}
                  onClick={() => changeTab(tab.id)}
                  onKeyDown={(event) => handleTabKeyDown(event, tab.id)}
                >
                  <span className="settings-nav-icon" aria-hidden="true">
                    <svg className="icon">
                      <use href={iconHref(SETTINGS_TAB_ICONS[tab.id])} />
                    </svg>
                  </span>
                  <span className="settings-nav-label">{tab.label}</span>
                  <span className="settings-nav-description">{tab.description}</span>
                </button>
              </React.Fragment>
            ))}
          </div>
        </aside>

        <section className="settings-main">
          <div className="settings-topbar">
            <button
              className="console-mobile-back"
              type="button"
              title={tr('console.backToWorkspace')}
              aria-label={tr('console.backToWorkspace')}
              onClick={onRequestClose}
            >
              <svg className="icon" aria-hidden="true">
                <use href="#icon-chevron-left" />
              </svg>
            </button>
            <div className="settings-title-block">
              <h2 id="settings-dialog-title" ref={titleRef} tabIndex={-1}>
                {showConfigError ? tr('settings.configError') : activeMeta.label}
              </h2>
              <p>{showConfigError ? tr('settings.configErrorSubtitle') : activeMeta.description}</p>
            </div>
            <div className="settings-topbar-actions">
              {!isUsage && (
                <span className={statusClass} id="settings-status" title={status.message}>
                  {status.message}
                </span>
              )}
              {canSaveConfig && configConflict && (
                <button
                  className="btn-secondary console-reload-button"
                  type="button"
                  onClick={onReloadConfig}
                  disabled={status.type === 'loading'}
                >
                  {tr('settings.reloadLatest')}
                </button>
              )}
              {canSaveConfig && (
                <button
                  className="btn-primary console-save-button"
                  id="settings-save-btn"
                  onClick={onSaveConfig}
                  disabled={!configDirty || configConflict || status.type === 'loading'}
                >
                  {tr('settings.save')}
                </button>
              )}
            </div>
          </div>

          {showDiscardConfirm && (
            <div className="settings-discard-dialog" role="alertdialog" aria-live="assertive">
              <div>
                <strong>{tr('settings.discardTitle')}</strong>
                <span>
                  {` ${tr('settings.discardBody', {
                    sections: dirtySections.join(', '),
                    verb: dirtySections.length === 1 ? 'has' : 'have',
                  })}`}
                </span>
              </div>
              <div className="settings-discard-actions">
                <button className="btn-secondary" type="button" onClick={onCancelDiscard}>
                  {tr('settings.keepEditing')}
                </button>
                <button className="btn-primary btn-danger" type="button" onClick={onDiscardChanges}>
                  {tr('settings.discardChanges')}
                </button>
              </div>
            </div>
          )}

          <div className="page-body settings-body" id="settings-body">
            {children}
          </div>

          {!isUsage && (
            <div className="settings-footer">
              <div className="settings-footer-note">
                {hasUnsavedChanges
                  ? tr('settings.unsaved')
                  : activeMeta.saveMode === 'skills'
                    ? tr('settings.skillsIndependent')
                    : tr('settings.noUnsaved')}
              </div>
              {canSaveConfig && (
                <div className="settings-footer-actions">
                  {configConflict && (
                    <button
                      className="btn-secondary settings-mobile-reload"
                      type="button"
                      onClick={onReloadConfig}
                      disabled={status.type === 'loading'}
                    >
                      {tr('settings.reloadLatest')}
                    </button>
                  )}
                  <button
                    className="btn-primary settings-mobile-save"
                    onClick={onSaveConfig}
                    disabled={!configDirty || configConflict || status.type === 'loading'}
                  >
                    {tr('settings.save')}
                  </button>
                </div>
              )}
            </div>
          )}
        </section>
      </div>
    </main>
  );
}

// ── Main SettingsPage component ───────────────────────────────────────────────

export function SettingsPage() {
  useLanguageVersion();
  const [visible, setVisible] = useState(false);
  const [consoleRendered, setConsoleRendered] = useState(false);
  const [config, setConfig] = useState<AppConfig>({});
  const [savedConfig, setSavedConfig] = useState<AppConfig>({});
  const [configBaseline, setConfigBaseline] = useState(() => serializeConfigForDirty({}));
  const [loadedConfigFileEtag, setLoadedConfigFileEtag] = useState<string>();
  const [configConflict, setConfigConflict] = useState(false);
  const [activeTab, setActiveTab] = useState<TabId>('tab-general');
  const [visitedTabs, setVisitedTabs] = useState<ReadonlySet<TabId>>(
    () => new Set(['tab-general']),
  );
  const [status, setStatus] = useState({ message: '', type: 'idle' as StatusType });
  const [corruptData, setCorruptData] = useState<ConfigApiResponse | null>(null);
  const [discoveredAgents, setDiscoveredAgents] = useState<DiscoveredAgentInfo[]>([]);
  const [settingsSessionId, setSettingsSessionId] = useState('main');
  const [skillsDirty, setSkillsDirty] = useState(false);
  const [mcpDirty, setMcpDirty] = useState(false);
  const [modelsDraftDirty, setModelsDraftDirty] = useState(false);
  const [corruptDraftDirty, setCorruptDraftDirty] = useState(false);
  const [modelsBaselineRevision, setModelsBaselineRevision] = useState(0);
  const [consoleInstanceRevision, setConsoleInstanceRevision] = useState(0);
  const [showDiscardConfirm, setShowDiscardConfirm] = useState(false);
  const requestCloseRef = useRef<() => void>(() => setVisible(false));
  const visibleRef = useRef(false);
  const settingsSessionIdRef = useRef('main');
  const hasUnsavedChangesRef = useRef(false);
  const configRef = useRef<AppConfig>({});
  const modelsDraftDirtyRef = useRef(false);
  const saveInFlightRef = useRef(false);
  const resetConsoleChildrenOnOpenRef = useRef(false);
  const transitionControllerRef = useRef<ConsoleTransitionController | null>(null);
  const workspaceRestoreTargetRef = useRef<HTMLElement | null>(null);

  const updateConsoleVisibility = useCallback((nextVisible: boolean) => {
    // Keep the imperative intent in sync before React flushes the state update.
    // This prevents an older close transition from unmounting a lazily opened Console.
    visibleRef.current = nextVisible;
    setVisible(nextVisible);
  }, []);

  const updateModelsDraftDirty = useCallback((dirty: boolean) => {
    // ModelsConsole can hold blank cards or unapplied JSON that are not represented
    // by configRef yet, so save/load conflict checks must see this immediately.
    modelsDraftDirtyRef.current = dirty;
    setModelsDraftDirty(dirty);
  }, []);

  const configDirty = useMemo(
    () => serializeConfigForDirty(config) !== configBaseline,
    [config, configBaseline],
  );
  const hasUnsavedChanges =
    configDirty || modelsDraftDirty || corruptDraftDirty || skillsDirty || mcpDirty;

  const closeWithoutPrompt = useCallback(() => {
    // The Console remains mounted during its exit transition. Rebuild its child
    // tree on the next open so a rapid reopen cannot revive discarded local drafts.
    resetConsoleChildrenOnOpenRef.current = true;
    setShowDiscardConfirm(false);
    setConfig(savedConfig);
    setConfigBaseline(serializeConfigForDirty(savedConfig));
    setConfigConflict(false);
    updateModelsDraftDirty(false);
    setCorruptDraftDirty(false);
    setModelsBaselineRevision((revision) => revision + 1);
    updateConsoleVisibility(false);
    setSkillsDirty(false);
    setMcpDirty(false);
  }, [savedConfig, updateConsoleVisibility, updateModelsDraftDirty]);

  const requestClose = useCallback(() => {
    if (hasUnsavedChanges) {
      setShowDiscardConfirm(true);
      return;
    }
    closeWithoutPrompt();
  }, [closeWithoutPrompt, hasUnsavedChanges]);

  useEffect(() => {
    requestCloseRef.current = requestClose;
  }, [requestClose]);

  useEffect(() => {
    visibleRef.current = visible;
  }, [visible]);

  useEffect(() => {
    settingsSessionIdRef.current = settingsSessionId;
  }, [settingsSessionId]);

  useEffect(() => {
    hasUnsavedChangesRef.current = hasUnsavedChanges;
  }, [hasUnsavedChanges]);

  useEffect(() => {
    configRef.current = config;
  }, [config]);

  useEffect(() => {
    modelsDraftDirtyRef.current = modelsDraftDirty;
  }, [modelsDraftDirty]);

  // Register bridge functions
  useEffect(() => {
    _open = () => {
      const nextSessionId = pendingRoute.sessionId || 'main';
      if (
        visibleRef.current &&
        hasUnsavedChangesRef.current &&
        nextSessionId !== settingsSessionIdRef.current
      ) {
        setShowDiscardConfirm(true);
        return;
      }
      if (resetConsoleChildrenOnOpenRef.current) {
        resetConsoleChildrenOnOpenRef.current = false;
        setConsoleInstanceRevision((revision) => revision + 1);
        setVisitedTabs(new Set([routeSection(pendingRoute)]));
      }
      setSettingsSessionId(nextSessionId);
      setActiveTab(routeSection(pendingRoute));
      if (!hasUnsavedChangesRef.current) setShowDiscardConfirm(false);
      setConsoleRendered(true);
      updateConsoleVisibility(true);
    };
    _close = () => requestCloseRef.current();
    // Honour any open request that arrived before the lazy chunk finished loading.
    if (pendingOpen) {
      pendingOpen = false;
      setSettingsSessionId(pendingRoute.sessionId || 'main');
      setActiveTab(routeSection(pendingRoute));
      setShowDiscardConfirm(false);
      setConsoleRendered(true);
      updateConsoleVisibility(true);
    }
    return () => {
      _open = null;
      _close = null;
    };
  }, [updateConsoleVisibility]);

  useEffect(() => {
    const workspace = document.getElementById('app-workspace');
    const consolePage = document.getElementById('console-page');
    if (!workspace || !consolePage) return;
    const chat = document.getElementById('chat');
    const controller = createConsoleTransitionController({
      workspace,
      consolePage,
      workspacePortalRoot: document.getElementById('workspace-portal-root'),
      scrollTargets: chat instanceof HTMLElement ? [chat] : [],
      onBeforeWorkspaceHide: suspendChatScrollTracking,
      onAfterWorkspaceShow: resumeChatScrollTracking,
    });
    transitionControllerRef.current = controller;
    return () => {
      controller.dispose();
      transitionControllerRef.current = null;
    };
  }, []);

  // The workspace remains mounted while the controller swaps accessibility,
  // focus, and visual state between the two top-level surfaces.
  useEffect(() => {
    const controller = transitionControllerRef.current;
    const legacyHost = document.getElementById('settings-page');
    if (!controller) {
      if (legacyHost) legacyHost.hidden = !visible;
      if (!visible && !visibleRef.current) setConsoleRendered(false);
      return;
    }
    if (visible) {
      document.body.classList.add('console-view-open');
      const focusTarget = document.getElementById('settings-dialog-title');
      void controller.showConsole({
        focusTarget: focusTarget instanceof HTMLElement ? focusTarget : undefined,
      });
      return;
    }
    document.body.classList.remove('console-view-open');
    // On the first lazy mount the open bridge may already have queued
    // visible=true while this effect still observes the initial false render.
    // The controller starts on Workspace, so asking it to "restore" that same
    // surface would move focus to its fallback before showConsole can capture
    // the real Settings/Usage opener.
    if (controller.surface === 'workspace' && controller.desiredSurface === 'workspace') return;
    const restoreTarget = workspaceRestoreTargetRef.current;
    workspaceRestoreTargetRef.current = null;
    void controller.showWorkspace({ restoreTarget }).then((completed) => {
      if (completed && !visibleRef.current) setConsoleRendered(false);
    });
  }, [visible]);

  useEffect(() => {
    if (!visible) return;
    setVisitedTabs((current) => {
      if (current.has(activeTab)) return current;
      return new Set(current).add(activeTab);
    });
  }, [activeTab, visible]);

  useEffect(() => {
    if (!visible) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return;
      if (hasActiveConsoleEscapeLayer(document)) return;
      event.preventDefault();
      event.stopPropagation();
      if (showDiscardConfirm) setShowDiscardConfirm(false);
      else requestClose();
    };
    window.addEventListener('keydown', onKeyDown, true);
    return () => window.removeEventListener('keydown', onKeyDown, true);
  }, [requestClose, showDiscardConfirm, visible]);

  useEffect(() => {
    if (!hasUnsavedChanges) setShowDiscardConfirm(false);
  }, [hasUnsavedChanges]);

  const applyLoadedConfigResponse = useCallback(
    (data: ConfigApiResponse): boolean => {
      setDiscoveredAgents(data.discoveredAgents || []);
      setLoadedConfigFileEtag(
        typeof data.configFileEtag === 'string' ? data.configFileEtag : undefined,
      );
      setConfigConflict(false);
      if (data.parse_error) {
        setCorruptData(data);
        setActiveTab((current) => (current === 'tab-usage' ? current : 'tab-general'));
        setConfig({});
        setSavedConfig({});
        setConfigBaseline(serializeConfigForDirty({}));
        updateModelsDraftDirty(false);
        setCorruptDraftDirty(false);
        setModelsBaselineRevision((revision) => revision + 1);
        setStatus({ message: tr('settings.syntaxErrors'), type: 'error' });
        return false;
      }
      const nextConfig = data.config || {};
      setCorruptData(null);
      setConfig(nextConfig);
      setSavedConfig(nextConfig);
      setConfigBaseline(serializeConfigForDirty(nextConfig));
      updateModelsDraftDirty(false);
      setCorruptDraftDirty(false);
      setModelsBaselineRevision((revision) => revision + 1);
      if (!hasUnsavedChangesRef.current) setShowDiscardConfirm(false);
      setStatus({ message: tr('settings.loadedFrom', { path: data.path }), type: 'success' });
      dispatchConfigSaved(nextConfig, data);
      return true;
    },
    [updateModelsDraftDirty],
  );

  const reloadLatestConfig = useCallback(async () => {
    setStatus({ message: tr('settings.loading'), type: 'loading' });
    try {
      const latest = await fetchLatestConfigResponse();
      applyLoadedConfigResponse(latest);
    } catch (error: unknown) {
      setStatus({
        message: tr('settings.loadFailed', { error: (error as Error).message }),
        type: 'error',
      });
    }
  }, [applyLoadedConfigResponse]);

  // Load config when opened
  useEffect(() => {
    if (!visible) return;
    const controller = new AbortController();
    const configSnapshotAtRequest = serializeConfigForDirty(configRef.current);
    (async () => {
      setStatus({ message: tr('settings.loading'), type: 'loading' });
      try {
        const data = await fetchLatestConfigResponse({ signal: controller.signal });
        if (
          serializeConfigForDirty(configRef.current) !== configSnapshotAtRequest ||
          modelsDraftDirtyRef.current
        ) {
          setConfigConflict(true);
          setStatus({ message: tr('settings.configConflict'), type: 'error' });
          return;
        }
        applyLoadedConfigResponse(data);
      } catch (e: unknown) {
        if ((e as Error).name === 'AbortError') return;
        setStatus({
          message: tr('settings.loadFailed', { error: (e as Error).message }),
          type: 'error',
        });
      }
    })();
    return () => controller.abort();
  }, [applyLoadedConfigResponse, visible]);

  const handleStatus = useCallback((message: string, type = 'idle') => {
    setStatus({ message, type: type as StatusType });
  }, []);

  const validateAgentModels = (cfg: AppConfig): void => {
    const model = cfg.agents?.defaults?.model || {};
    const providers = cfg.models?.providers || {};
    const hasConfiguredProviders = Object.keys(providers).length > 0;

    for (const [key, val] of Object.entries(model)) {
      if (!val) continue;
      const modelRef = String(val).trim();
      if (!modelRef) {
        throw new Error(`Agent model "${key}": model id cannot be empty.`);
      }
      if (modelRef.includes('/')) {
        const [provName, ...rest] = modelRef.split('/');
        const modelId = rest.join('/');
        if (!modelId || !modelId.trim()) {
          throw new Error(`Agent model "${key}": model id cannot be empty after provider prefix.`);
        }
        if (hasConfiguredProviders && !providers[provName]) {
          throw new Error(
            `Agent model "${key}" references unknown provider "${provName}". Add it in Models tab first.`,
          );
        }
        if (!hasConfiguredProviders && !isBuiltinProviderName(provName)) {
          throw new Error(
            `Agent model "${key}" references unsupported provider prefix "${provName}".`,
          );
        }
        if (hasConfiguredProviders && providers[provName]) {
          const models = providers[provName].models || [];
          if (models.length > 0 && modelId && !models.some((m) => m.id === modelId)) {
            throw new Error(
              `Agent model "${key}" references unknown model "${modelId}" for provider "${provName}".`,
            );
          }
        }
      } else if (hasConfiguredProviders) {
        const matchingProviders = Object.entries(providers)
          .filter(([, provider]) =>
            (provider.models || []).some((candidate) => candidate.id === modelRef),
          )
          .map(([providerName]) => providerName)
          .sort();
        if (matchingProviders.length === 0) {
          throw new Error(
            `Agent model "${key}" references unknown model "${modelRef}". Add it in Models first.`,
          );
        }
        if (matchingProviders.length > 1) {
          throw new Error(
            `Agent model "${key}" is ambiguous. Use one of: ${matchingProviders
              .map((providerName) => `${providerName}/${modelRef}`)
              .join(', ')}.`,
          );
        }
      }
    }
  };

  const saveConfig = async () => {
    if (saveInFlightRef.current || configConflict) return;
    const requestConfigSnapshot = serializeConfigForDirty(config);
    const requestModelsSnapshot = serializeModelsForDirty(config.models);
    const finalConfig = normalizeConfigForSave(config);

    try {
      validateAgentModels(finalConfig);
    } catch (e: unknown) {
      setStatus({ message: (e as Error).message, type: 'error' });
      return;
    }

    setStatus({ message: tr('settings.saving'), type: 'loading' });
    saveInFlightRef.current = true;
    try {
      const connectionGeneration = getComposerConnectionGeneration();
      const resp = await fetch('/api/config', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          config: finalConfig,
          ...(loadedConfigFileEtag ? { baseConfigFileEtag: loadedConfigFileEtag } : {}),
        }),
      });
      const data: ConfigApiResponse = await resp.json();
      if (connectionGeneration !== getComposerConnectionGeneration()) {
        setConfigConflict(true);
        setStatus({ message: tr('settings.configConflict'), type: 'error' });
        return;
      }
      if (resp.status === 409) {
        setConfigConflict(true);
        setStatus({ message: tr('settings.configConflict'), type: 'error' });
        return;
      }
      if (!resp.ok || data.error) {
        setStatus({ message: data.error || tr('settings.saveFailed'), type: 'error' });
        return;
      }
      const composerRevisionAccepted = acceptComposerConfigRevision(data.configRevision);
      let latestComposerResponse: ConfigApiResponse | undefined;
      if (!composerRevisionAccepted) {
        const latest = await fetchLatestConfigResponse();
        const sameSavedFile =
          typeof data.configFileEtag === 'string' && data.configFileEtag === latest.configFileEtag;
        if (!sameSavedFile) {
          if (
            serializeConfigForDirty(configRef.current) === requestConfigSnapshot &&
            !modelsDraftDirtyRef.current
          ) {
            applyLoadedConfigResponse(latest);
          } else {
            setConfigConflict(true);
            setStatus({ message: tr('settings.configConflict'), type: 'error' });
          }
          return;
        }
        latestComposerResponse = latest;
      }
      setStatus({
        message: tr('settings.saved'),
        type: 'success',
      });
      setConfig((current) =>
        serializeConfigForDirty(current) === requestConfigSnapshot ? finalConfig : current,
      );
      setSavedConfig(finalConfig);
      setConfigBaseline(serializeConfigForDirty(finalConfig));
      if (
        serializeModelsForDirty(configRef.current.models) === requestModelsSnapshot &&
        !modelsDraftDirtyRef.current
      ) {
        updateModelsDraftDirty(false);
        setModelsBaselineRevision((revision) => revision + 1);
      }
      setLoadedConfigFileEtag(
        typeof data.configFileEtag === 'string' ? data.configFileEtag : undefined,
      );
      setConfigConflict(false);
      if (composerRevisionAccepted) {
        dispatchConfigSaved(finalConfig, data);
      } else if (latestComposerResponse?.config && !latestComposerResponse.parse_error) {
        dispatchConfigSaved(latestComposerResponse.config, latestComposerResponse);
      } else {
        void refreshComposerAvailability();
      }
    } catch (e: unknown) {
      setStatus({
        message: tr('settings.saveFailedWithError', { error: (e as Error).message }),
        type: 'error',
      });
    } finally {
      saveInFlightRef.current = false;
    }
  };

  const selectTab = useCallback((tab: TabId) => {
    setShowDiscardConfirm(false);
    setActiveTab(tab);
    setVisitedTabs((current) => {
      if (current.has(tab)) return current;
      return new Set(current).add(tab);
    });
  }, []);

  const returnToComposerAfterInsert = useCallback((input: HTMLTextAreaElement) => {
    workspaceRestoreTargetRef.current = input;
    requestCloseRef.current();
  }, []);

  if (!consoleRendered) return null;

  // Visited panels stay mounted inside the full-screen Console so local drafts survive navigation.
  return (
    <SettingsShell
      key={consoleInstanceRevision}
      activeTab={activeTab}
      tabs={settingsTabs()}
      status={status}
      configDirty={configDirty || corruptDraftDirty}
      modelsDraftDirty={modelsDraftDirty}
      configConflict={configConflict}
      skillsDirty={skillsDirty}
      mcpDirty={mcpDirty}
      corrupt={!!corruptData}
      showDiscardConfirm={showDiscardConfirm}
      onTabChange={selectTab}
      onSaveConfig={saveConfig}
      onReloadConfig={() => void reloadLatestConfig()}
      onRequestClose={requestClose}
      onCancelDiscard={() => setShowDiscardConfirm(false)}
      onDiscardChanges={closeWithoutPrompt}
    >
      {corruptData ? (
        <>
          <section
            id="tab-general-panel"
            className="settings-corrupt-panel"
            role="tabpanel"
            aria-labelledby="tab-general-button"
            hidden={activeTab !== 'tab-general'}
          >
            <CorruptConfigView
              data={corruptData}
              conflict={configConflict}
              onDirtyChange={setCorruptDraftDirty}
              onStatus={handleStatus}
              onConflict={() => {
                setConfigConflict(true);
                setStatus({ message: tr('settings.configConflict'), type: 'error' });
              }}
              onReload={() => void reloadLatestConfig()}
              onReloaded={(d) => {
                applyLoadedConfigResponse(d);
              }}
            />
          </section>
          {visitedTabs.has('tab-usage') && (
            <section
              id="tab-usage-panel"
              role="tabpanel"
              aria-labelledby="tab-usage-button"
              hidden={activeTab !== 'tab-usage'}
            >
              <UsageView
                sessionId={settingsSessionId}
                active={visible && activeTab === 'tab-usage'}
                className="is-embedded"
              />
            </section>
          )}
        </>
      ) : (
        <>
          {visitedTabs.has('tab-general') && (
            <section
              id="tab-general-panel"
              role="tabpanel"
              aria-labelledby="tab-general-button"
              hidden={activeTab !== 'tab-general'}
            >
              <GeneralTab config={config} onChange={setConfig} />
            </section>
          )}
          {visitedTabs.has('tab-skills') && (
            <section
              id="tab-skills-panel"
              role="tabpanel"
              aria-labelledby="tab-skills-button"
              hidden={activeTab !== 'tab-skills'}
            >
              <SkillsTab sessionId={settingsSessionId} onDirtyChange={setSkillsDirty} />
            </section>
          )}
          {visitedTabs.has('tab-agents') && (
            <section
              id="tab-agents-panel"
              role="tabpanel"
              aria-labelledby="tab-agents-button"
              hidden={activeTab !== 'tab-agents'}
            >
              <AgentsTab config={config} onChange={setConfig} discoveredAgents={discoveredAgents} />
            </section>
          )}
          {visitedTabs.has('tab-models') && (
            <section
              id="tab-models-panel"
              role="tabpanel"
              aria-labelledby="tab-models-button"
              hidden={activeTab !== 'tab-models'}
            >
              <ModelsConsole
                config={config}
                onChange={setConfig}
                onStatus={handleStatus}
                baselineRevision={modelsBaselineRevision}
                onDraftDirtyChange={updateModelsDraftDirty}
              />
            </section>
          )}
          {visitedTabs.has('tab-mcp') && (
            <section
              id="tab-mcp-panel"
              role="tabpanel"
              aria-labelledby="tab-mcp-button"
              hidden={activeTab !== 'tab-mcp'}
            >
              <McpTab
                config={config}
                sessionId={settingsSessionId}
                onChange={setConfig}
                onStatus={handleStatus}
                onPolicyDirtyChange={setMcpDirty}
                onComposerInsert={returnToComposerAfterInsert}
              />
            </section>
          )}
          {visitedTabs.has('tab-s3') && (
            <section
              id="tab-s3-panel"
              role="tabpanel"
              aria-labelledby="tab-s3-button"
              hidden={activeTab !== 'tab-s3'}
            >
              <S3Tab config={config} onChange={setConfig} />
            </section>
          )}
          {visitedTabs.has('tab-usage') && (
            <section
              id="tab-usage-panel"
              role="tabpanel"
              aria-labelledby="tab-usage-button"
              hidden={activeTab !== 'tab-usage'}
            >
              <UsageView
                sessionId={settingsSessionId}
                active={visible && activeTab === 'tab-usage'}
                className="is-embedded"
              />
            </section>
          )}
        </>
      )}
    </SettingsShell>
  );
}
