import React, { useState, useEffect, useCallback } from 'react';
import type { AppConfig, McpServerConfig, S3Config } from '../types/config.js';
import type { ConfigApiResponse } from '../types/config.js';
import {
  validateProviderName,
  validateMcpCwdValue,
  validateModelsConfigDraftShape,
  validateMcpConfigDraftShape,
  buildModelOptions,
  isBuiltinProviderName,
} from '../settingsValidation.js';
import {
  buildProviderForms,
  createModelFormEntry,
  createProviderForm,
  serializeProviderForms,
} from './settingsModels.js';
import type { ModelFormEntry, ProviderFormData } from './settingsModels.js';

// ── Module-level bridge (imperative open/close from main.ts) ──────────────────

let _open: (() => void) | null = null;
let _close: (() => void) | null = null;

export function openSettingsPage(): void {
  _open?.();
}
export function closeSettingsPage(): void {
  _close?.();
}
export function initSettingsListeners(): void {
  /* no-op: React handles all listeners */
}

// ── Helpers ───────────────────────────────────────────────────────────────────

type TriBool = boolean | undefined;

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
      <option value="">Default</option>
      <option value="true">Enabled</option>
      <option value="false">Disabled</option>
    </select>
  );
}

function ModelSelect({
  value,
  options,
  onChange,
}: {
  value: string | undefined;
  options: string[];
  onChange: (v: string) => void;
}) {
  const v = value || '';
  const includesValue = v && options.includes(v);
  return (
    <select value={v} onChange={(e) => onChange(e.target.value)}>
      <option value="">-- none --</option>
      {options.map((opt) => (
        <option key={opt} value={opt}>
          {opt}
        </option>
      ))}
      {v && !includesValue && <option value={v}>{v} (custom)</option>}
    </select>
  );
}

// ── General Tab ───────────────────────────────────────────────────────────────

function GeneralTab({ config, onChange }: { config: AppConfig; onChange: (c: AppConfig) => void }) {
  const s = config.settings || {};
  const set = (patch: Partial<typeof s>) => onChange({ ...config, settings: { ...s, ...patch } });

  return (
    <>
      <div className="settings-group">
        <div className="settings-group-title">Server</div>
        <SettingsRow label="Port">
          <input
            type="number"
            value={s.port ?? ''}
            placeholder="18989"
            onChange={(e) => set({ port: numInputToValue(e.target.value) })}
          />
        </SettingsRow>
      </div>
      <div className="settings-group">
        <div className="settings-group-title">Timeouts (seconds)</div>
        <SettingsRow label="Exec Timeout">
          <input
            type="number"
            value={s.execTimeout ?? ''}
            placeholder="30"
            onChange={(e) => set({ execTimeout: numInputToValue(e.target.value) })}
          />
        </SettingsRow>
        <SettingsRow label="Tool Timeout">
          <input
            type="number"
            value={s.toolTimeout ?? ''}
            placeholder="30"
            onChange={(e) => set({ toolTimeout: numInputToValue(e.target.value) })}
          />
        </SettingsRow>
        <SettingsRow label="Sub-Agent Timeout">
          <input
            type="number"
            value={s.subAgentTimeout ?? ''}
            placeholder="300"
            onChange={(e) => set({ subAgentTimeout: numInputToValue(e.target.value) })}
          />
        </SettingsRow>
        <SettingsRow label="Max LLM Retries">
          <input
            type="number"
            value={s.maxLlmRetries ?? ''}
            placeholder="2"
            onChange={(e) => set({ maxLlmRetries: numInputToValue(e.target.value) })}
          />
        </SettingsRow>
      </div>
      <div className="settings-group">
        <div className="settings-group-title">Context</div>
        <SettingsRow label="Max Context Tokens">
          <input
            type="number"
            value={s.maxContextTokens ?? ''}
            placeholder="32000"
            onChange={(e) => set({ maxContextTokens: numInputToValue(e.target.value) })}
          />
        </SettingsRow>
        <SettingsRow label="Max Output Bytes">
          <input
            type="number"
            value={s.maxOutputBytes ?? ''}
            placeholder="51200"
            onChange={(e) => set({ maxOutputBytes: numInputToValue(e.target.value) })}
          />
        </SettingsRow>
        <SettingsRow label="Max File Bytes">
          <input
            type="number"
            value={s.maxFileBytes ?? ''}
            placeholder="204800"
            onChange={(e) => set({ maxFileBytes: numInputToValue(e.target.value) })}
          />
        </SettingsRow>
      </div>
      <div className="settings-group">
        <div className="settings-group-title">Features</div>
        <SettingsRow label="Structured Memory">
          <TriSelect value={s.structuredMemory} onChange={(v) => set({ structuredMemory: v })} />
        </SettingsRow>
        <SettingsRow label="Daily Reflection">
          <TriSelect value={s.dailyReflection} onChange={(v) => set({ dailyReflection: v })} />
        </SettingsRow>
        <SettingsRow label="Enable S3">
          <TriSelect value={s.enableS3} onChange={(v) => set({ enableS3: v })} />
        </SettingsRow>
        <SettingsRow label="OpenAI Stream Usage">
          <TriSelect
            value={s.openaiStreamIncludeUsage}
            onChange={(v) => set({ openaiStreamIncludeUsage: v })}
          />
        </SettingsRow>
        <SettingsRow label="Anthropic Prompt Caching">
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

function AgentsTab({ config, onChange }: { config: AppConfig; onChange: (c: AppConfig) => void }) {
  const model = config.agents?.defaults?.model || {};
  const providers = config.models?.providers || {};
  const allModels = buildModelOptions(providers);

  const setModel = (key: string, val: string) => {
    const newModel = { ...model, [key]: val || undefined };
    onChange({
      ...config,
      agents: {
        ...config.agents,
        defaults: { ...(config.agents?.defaults || {}), model: newModel },
      },
    });
  };

  const roles: Array<{ key: string; label: string }> = [
    { key: 'primary', label: 'Primary' },
    { key: 'fast', label: 'Fast' },
    { key: 'sub-agent', label: 'Sub-Agent' },
    { key: 'memory', label: 'Memory' },
    { key: 'reflection', label: 'Reflection' },
    { key: 'context', label: 'Context' },
  ];

  return (
    <div className="settings-group">
      <div className="settings-group-title">Agent Default Models</div>
      <p style={{ fontSize: 12, color: 'var(--dim)', marginBottom: 12 }}>
        Models must reference a provider configured in the Models tab (format:{' '}
        <code>provider/model-id</code>).
      </p>
      {roles.map(({ key, label }) => (
        <SettingsRow key={key} label={label}>
          <ModelSelect
            value={(model as Record<string, string | undefined>)[key]}
            options={allModels}
            onChange={(val) => setModel(key, val)}
          />
        </SettingsRow>
      ))}
    </div>
  );
}

// ── Models Tab ────────────────────────────────────────────────────────────────

function ModelEntryRow({
  model,
  onChange,
  onDelete,
}: {
  model: ModelFormEntry;
  onChange: (m: ModelFormEntry) => void;
  onDelete: () => void;
}) {
  const inputArr = Array.isArray(model.input) ? model.input : ['text'];
  const hasText = inputArr.includes('text');
  const hasImage = inputArr.includes('image');

  const setInput = (text: boolean, image: boolean) => {
    const arr: string[] = [];
    if (text) arr.push('text');
    if (image) arr.push('image');
    onChange({ ...model, input: arr.length > 0 ? arr : undefined });
  };

  return (
    <div
      className="model-entry-form"
      style={{
        border: '1px solid var(--border)',
        borderRadius: 6,
        padding: 8,
        marginBottom: 6,
        background: 'var(--bg)',
      }}
    >
      <div style={{ display: 'flex', gap: 6, alignItems: 'center', flexWrap: 'wrap' }}>
        <input
          type="text"
          value={model.id || ''}
          placeholder="model-id"
          style={{ flex: 1, minWidth: 120 }}
          onChange={(e) => onChange({ ...model, id: e.target.value })}
        />
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
            checked={!!model.reasoning}
            onChange={(e) => onChange({ ...model, reasoning: e.target.checked || undefined })}
          />{' '}
          Reasoning
        </label>
        <button className="btn-danger-sm" title="Remove model" onClick={onDelete}>
          ✕
        </button>
      </div>
      <div
        style={{ display: 'flex', gap: 8, marginTop: 6, flexWrap: 'wrap', alignItems: 'center' }}
      >
        <label
          style={{
            fontSize: 11,
            color: 'var(--dim)',
            display: 'flex',
            alignItems: 'center',
            gap: 4,
          }}
        >
          Context Window
          <input
            type="number"
            value={model.contextWindow ?? ''}
            placeholder="128000"
            style={{ width: 90 }}
            onChange={(e) => onChange({ ...model, contextWindow: numInputToValue(e.target.value) })}
          />
        </label>
        <label
          style={{
            fontSize: 11,
            color: 'var(--dim)',
            display: 'flex',
            alignItems: 'center',
            gap: 4,
          }}
        >
          Max Tokens
          <input
            type="number"
            value={model.maxTokens ?? ''}
            placeholder="16384"
            style={{ width: 90 }}
            onChange={(e) => onChange({ ...model, maxTokens: numInputToValue(e.target.value) })}
          />
        </label>
        <span
          style={{
            fontSize: 11,
            color: 'var(--dim)',
            display: 'flex',
            alignItems: 'center',
            gap: 6,
            marginLeft: 4,
          }}
        >
          Input:
          <label style={{ display: 'flex', alignItems: 'center', gap: 2 }}>
            <input
              type="checkbox"
              checked={hasText}
              onChange={(e) => setInput(e.target.checked, hasImage)}
            />{' '}
            Text
          </label>
          <label style={{ display: 'flex', alignItems: 'center', gap: 2 }}>
            <input
              type="checkbox"
              checked={hasImage}
              onChange={(e) => setInput(hasText, e.target.checked)}
            />{' '}
            Image
          </label>
        </span>
      </div>
    </div>
  );
}

function ProviderCard({
  prov,
  onChange,
  onDelete,
  onTest,
}: {
  prov: ProviderFormData;
  onChange: (p: ProviderFormData) => void;
  onDelete: () => void;
  onTest: (p: ProviderFormData, modelId: string) => void;
}) {
  const addModel = () => {
    onChange({
      ...prov,
      models: [...prov.models, createModelFormEntry(prov.name, { id: '', input: ['text'] })],
    });
  };
  const updateModel = (i: number, m: ModelFormEntry) => {
    const models = [...prov.models];
    models[i] = m;
    onChange({ ...prov, models });
  };
  const deleteModel = (i: number) => {
    onChange({ ...prov, models: prov.models.filter((_, j) => j !== i) });
  };

  const testBtnClass =
    prov.testState === 'ok'
      ? 'btn-test test-ok'
      : prov.testState === 'fail'
        ? 'btn-test test-fail'
        : prov.testState === 'testing'
          ? 'btn-test testing'
          : 'btn-test';

  return (
    <div className="provider-card" data-provider-name={prov.name}>
      <div className="provider-card-header">
        <span className="provider-card-name">{prov.name}</span>
        <div
          style={{
            display: 'flex',
            gap: 6,
            alignItems: 'center',
            flexWrap: 'wrap',
            justifyContent: 'flex-end',
          }}
        >
          {prov.models.length > 0 && (
            <select
              value={prov.selectedTestModel}
              style={{ maxWidth: 190, padding: '5px 8px' }}
              onChange={(e) => onChange({ ...prov, selectedTestModel: e.target.value })}
            >
              {prov.models.map(
                (m) =>
                  m.id && (
                    <option key={m._key} value={m.id}>
                      {m.id}
                    </option>
                  ),
              )}
            </select>
          )}
          <button className={testBtnClass} onClick={() => onTest(prov, prov.selectedTestModel)}>
            {prov.testLabel}
          </button>
          <button className="btn-danger-sm" title="Delete provider" onClick={onDelete}>
            ✕
          </button>
        </div>
      </div>
      <div className="provider-form" style={{ display: 'grid', gap: 8, marginTop: 8 }}>
        <SettingsRow label="API Type">
          <select value={prov.api} onChange={(e) => onChange({ ...prov, api: e.target.value })}>
            <option value="openai-completions">OpenAI Completions</option>
            <option value="anthropic">Anthropic</option>
            <option value="ollama">Ollama</option>
          </select>
        </SettingsRow>
        <SettingsRow label="Base URL">
          <input
            type="text"
            value={prov.baseUrl}
            placeholder="https://api.openai.com/v1"
            onChange={(e) => onChange({ ...prov, baseUrl: e.target.value })}
          />
        </SettingsRow>
        <SettingsRow label="API Key">
          <input
            type="password"
            value={prov.apiKey}
            onChange={(e) => onChange({ ...prov, apiKey: e.target.value })}
          />
        </SettingsRow>
      </div>
      <div style={{ marginTop: 10 }}>
        <div style={{ fontSize: 12, fontWeight: 600, marginBottom: 6, color: 'var(--fg)' }}>
          Models
        </div>
        {prov.models.map((m, i) => (
          <ModelEntryRow
            key={m._key}
            model={m}
            onChange={(nm) => updateModel(i, nm)}
            onDelete={() => deleteModel(i)}
          />
        ))}
        <button className="btn-secondary" style={{ marginTop: 6, fontSize: 11 }} onClick={addModel}>
          + Add Model
        </button>
      </div>
    </div>
  );
}

function ModelsTab({
  config,
  onChange,
  onStatus,
}: {
  config: AppConfig;
  onChange: (c: AppConfig) => void;
  onStatus: (msg: string, type?: string) => void;
}) {
  const initialProviders = config.models?.providers || {};
  const [providers, setProviders] = useState<ProviderFormData[]>(() =>
    buildProviderForms(initialProviders),
  );
  const [jsonText, setJsonText] = useState<string>(() =>
    JSON.stringify(config.models || { providers: {} }, null, 2),
  );
  const [jsonError, setJsonError] = useState('');
  const [jsonDirty, setJsonDirty] = useState(false);
  const [formDirty, setFormDirty] = useState(false);

  // When external config changes (e.g. JSON apply), re-sync form state
  useEffect(() => {
    const p = config.models?.providers || {};
    setProviders((previousProviders) => buildProviderForms(p, previousProviders));
    setJsonText(JSON.stringify(config.models || { providers: {} }, null, 2));
    setJsonError('');
    setJsonDirty(false);
    setFormDirty(false);
  }, [config.models]);

  const updateProvider = (i: number, p: ProviderFormData) => {
    const next = [...providers];
    next[i] = p;
    setProviders(next);
    setFormDirty(true);
  };

  const deleteProvider = (i: number) => {
    setProviders(providers.filter((_, j) => j !== i));
    setFormDirty(true);
  };

  const addProvider = () => {
    const name = prompt('Enter provider name:');
    if (!name) return;
    try {
      const existing = Object.fromEntries(providers.map((p) => [p.name, true]));
      const trimmed = validateProviderName(name, existing);
      setProviders([...providers, createProviderForm(trimmed)]);
      setFormDirty(true);
    } catch (e: unknown) {
      onStatus((e as Error).message, 'error');
    }
  };

  const applyJson = () => {
    if (formDirty) {
      onStatus(
        'Models form has unapplied changes. Save or discard them before applying Raw JSON.',
        'error',
      );
      return;
    }
    try {
      const parsed = JSON.parse(jsonText.trim() || '{}');
      validateModelsConfigDraftShape(parsed);
      const newConfig = { ...config, models: parsed };
      onChange(newConfig);
      setJsonError('');
      onStatus('Applied Models JSON', 'success');
    } catch (e: unknown) {
      setJsonError((e as Error).message);
    }
  };

  const testProvider = async (prov: ProviderFormData, modelId: string) => {
    if (!modelId) {
      onStatus('No model selected', 'error');
      return;
    }
    const i = providers.indexOf(prov);
    const setTest = (state: string, label: string) => {
      const next = [...providers];
      next[i] = { ...prov, testState: state as ProviderFormData['testState'], testLabel: label };
      setProviders(next);
    };
    setTest('testing', 'Testing...');
    try {
      const resp = await fetch('/api/config/test-model', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          baseUrl: prov.baseUrl,
          apiKey: prov.apiKey,
          api: prov.api || 'openai-completions',
          modelId,
        }),
      });
      const data = await resp.json();
      if (data.ok) setTest('ok', '✓ Connected');
      else {
        setTest('fail', '✗ Failed');
        onStatus(data.error || 'Connection failed', 'error');
      }
    } catch (e: unknown) {
      setTest('fail', '✗ Error');
      onStatus((e as Error).message, 'error');
    }
    setTimeout(() => {
      setProviders((prev) =>
        prev.map((p, j) => (j === i ? { ...p, testState: 'idle', testLabel: 'Test' } : p)),
      );
    }, 4000);
  };

  // Expose providers → parent config on mount and whenever providers change
  // (parent calls collectModels on save)
  useEffect(() => {
    const newModels = serializeProviderForms(providers);
    // Only propagate if we actually changed (avoid loops)
    if (JSON.stringify(newModels) !== JSON.stringify(config.models)) {
      onChange({ ...config, models: newModels });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [providers]);

  return (
    <>
      {providers.map((prov, i) => (
        <ProviderCard
          key={prov.name}
          prov={prov}
          onChange={(p) => updateProvider(i, p)}
          onDelete={() => deleteProvider(i)}
          onTest={testProvider}
        />
      ))}
      <button className="btn-secondary" style={{ marginTop: 10 }} onClick={addProvider}>
        + Add Provider
      </button>
      <details style={{ marginTop: 16 }}>
        <summary style={{ fontSize: 12, color: 'var(--dim)', cursor: 'pointer' }}>
          Advanced: Raw JSON
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
            Apply JSON
          </button>
        </div>
      </details>
    </>
  );
}

// ── MCP Tab ───────────────────────────────────────────────────────────────────

interface McpFormEntry extends McpServerConfig {
  name: string;
  _argsText: string; // textarea, one per line
  testState: 'idle' | 'testing' | 'ok' | 'fail';
  testLabel: string;
}

function newMcpForm(name: string, s: McpServerConfig = {}): McpFormEntry {
  return {
    name,
    command: s.command || '',
    _argsText: (s.args || []).join('\n'),
    cwd: s.cwd || '',
    timeoutSecs: s.timeoutSecs,
    enabled: s.enabled !== false,
    env: { ...(s.env || {}) },
    testState: 'idle',
    testLabel: 'Test',
  };
}

function McpServerCard({
  server,
  onChange,
  onDelete,
  onTest,
}: {
  server: McpFormEntry;
  onChange: (s: McpFormEntry) => void;
  onDelete: () => void;
  onTest: (s: McpFormEntry) => void;
}) {
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
            Enabled
          </label>
          <button className={testBtnClass} onClick={() => onTest(server)}>
            {server.testLabel}
          </button>
          <button className="btn-danger-sm" title="Delete server" onClick={onDelete}>
            ✕
          </button>
        </div>
      </div>
      <div className="provider-form" style={{ display: 'grid', gap: 8, marginTop: 8 }}>
        <SettingsRow label="Command">
          <input
            type="text"
            value={server.command || ''}
            placeholder="uvx"
            onChange={(e) => onChange({ ...server, command: e.target.value })}
          />
        </SettingsRow>
        <SettingsRow label="Args (one per line)">
          <textarea
            value={server._argsText}
            rows={3}
            style={{ fontFamily: 'var(--font-mono)', fontSize: 12 }}
            placeholder="One argument per line"
            onChange={(e) => onChange({ ...server, _argsText: e.target.value })}
          />
        </SettingsRow>
        <SettingsRow label="CWD">
          <input
            type="text"
            value={server.cwd || ''}
            placeholder="Optional working directory"
            onChange={(e) => onChange({ ...server, cwd: e.target.value })}
          />
        </SettingsRow>
        <SettingsRow label="Timeout (s)">
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
            <button className="btn-danger-sm" title="Remove" onClick={() => removeEnvVar(k)}>
              ✕
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

function McpTab({
  config,
  onChange,
  onStatus,
}: {
  config: AppConfig;
  onChange: (c: AppConfig) => void;
  onStatus: (msg: string, type?: string) => void;
}) {
  const [servers, setServers] = useState<McpFormEntry[]>(() => {
    const s = config.mcpServers || {};
    return Object.entries(s)
      .sort(([a], [b]) => a.localeCompare(b))
      .map(([n, sv]) => newMcpForm(n, sv));
  });
  const [jsonText, setJsonText] = useState(() => JSON.stringify(config.mcpServers || {}, null, 2));
  const [jsonError, setJsonError] = useState('');
  const [jsonDirty, setJsonDirty] = useState(false);
  const [formDirty, setFormDirty] = useState(false);

  useEffect(() => {
    const s = config.mcpServers || {};
    setServers(
      Object.entries(s)
        .sort(([a], [b]) => a.localeCompare(b))
        .map(([n, sv]) => newMcpForm(n, sv)),
    );
    setJsonText(JSON.stringify(config.mcpServers || {}, null, 2));
    setJsonError('');
    setJsonDirty(false);
    setFormDirty(false);
  }, [config.mcpServers]);

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

  const updateServer = (i: number, s: McpFormEntry) => {
    const next = [...servers];
    next[i] = s;
    setServers(next);
    setFormDirty(true);
  };

  const deleteServer = (i: number) => {
    setServers(servers.filter((_, j) => j !== i));
    setFormDirty(true);
  };

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

  const testServer = async (sv: McpFormEntry) => {
    const i = servers.indexOf(sv);
    const setTest = (state: string, label: string) => {
      const next = [...servers];
      next[i] = { ...sv, testState: state as McpFormEntry['testState'], testLabel: label };
      setServers(next);
    };
    setTest('testing', 'Testing...');
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
          setTest('idle', 'Test');
          return;
        }
      }
      const resp = await fetch('/api/config/test-mcp', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          command: sv.command,
          args,
          env: sv.env,
          cwd: sv.cwd || undefined,
          timeoutSecs: sv.timeoutSecs,
        }),
      });
      const data = await resp.json();
      if (data.ok) setTest('ok', `✓ ${data.tools} tools`);
      else {
        setTest('fail', '✗ Failed');
        if (data.error) onStatus(data.error, 'error');
      }
    } catch (e: unknown) {
      setTest('fail', '✗ Error');
      onStatus((e as Error).message, 'error');
    }
    setTimeout(() => {
      setServers((prev) =>
        prev.map((s, j) => (j === i ? { ...s, testState: 'idle', testLabel: 'Test' } : s)),
      );
    }, 4000);
  };

  // Propagate form state to parent config
  useEffect(() => {
    const mcpServers: Record<string, McpServerConfig> = {};
    for (const sv of servers) {
      const args = sv._argsText
        .split('\n')
        .map((a) => a.trim())
        .filter(Boolean);
      const entry: McpServerConfig = {
        command: sv.command || undefined,
        args: args.length > 0 ? args : undefined,
        cwd: sv.cwd || undefined,
        timeoutSecs: sv.timeoutSecs,
        enabled: sv.enabled,
        env: sv.env && Object.keys(sv.env).length > 0 ? sv.env : undefined,
      };
      mcpServers[sv.name] = entry;
    }
    const newMcp = servers.length > 0 ? mcpServers : undefined;
    if (JSON.stringify(newMcp) !== JSON.stringify(config.mcpServers)) {
      onChange({ ...config, mcpServers: newMcp });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [servers]);

  return (
    <>
      {servers.map((sv, i) => (
        <McpServerCard
          key={sv.name}
          server={sv}
          onChange={(s) => updateServer(i, s)}
          onDelete={() => deleteServer(i)}
          onTest={testServer}
        />
      ))}
      <button className="btn-secondary" style={{ marginTop: 10 }} onClick={addServer}>
        + Add MCP Server
      </button>
      <details style={{ marginTop: 16 }}>
        <summary style={{ fontSize: 12, color: 'var(--dim)', cursor: 'pointer' }}>
          Advanced: Raw JSON
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
            Apply JSON
          </button>
        </div>
      </details>
    </>
  );
}

// ── S3 Tab ────────────────────────────────────────────────────────────────────

function S3Tab({ config, onChange }: { config: AppConfig; onChange: (c: AppConfig) => void }) {
  const s3 = config.s3 || {};
  const set = (patch: Partial<S3Config>) => onChange({ ...config, s3: { ...s3, ...patch } });

  return (
    <div className="settings-group">
      <div className="settings-group-title">S3-Compatible File Storage</div>
      <SettingsRow label="Endpoint">
        <input
          type="text"
          value={s3.endpoint || ''}
          placeholder="https://s3.us-east-1.amazonaws.com"
          onChange={(e) => set({ endpoint: e.target.value || undefined })}
        />
      </SettingsRow>
      <SettingsRow label="Region">
        <input
          type="text"
          value={s3.region || ''}
          placeholder="us-east-1"
          onChange={(e) => set({ region: e.target.value || undefined })}
        />
      </SettingsRow>
      <SettingsRow label="Bucket">
        <input
          type="text"
          value={s3.bucket || ''}
          onChange={(e) => set({ bucket: e.target.value || undefined })}
        />
      </SettingsRow>
      <SettingsRow label="Access Key">
        <input
          type="text"
          value={s3.accessKey || ''}
          onChange={(e) => set({ accessKey: e.target.value || undefined })}
        />
      </SettingsRow>
      <SettingsRow label="Secret Key">
        <input
          type="password"
          value={s3.secretKey || ''}
          onChange={(e) => set({ secretKey: e.target.value || undefined })}
        />
      </SettingsRow>
      <SettingsRow label="Prefix">
        <input
          type="text"
          value={s3.prefix || ''}
          placeholder="lingclaw/images/"
          onChange={(e) => set({ prefix: e.target.value || undefined })}
        />
      </SettingsRow>
      <SettingsRow label="URL Expiry (s)">
        <input
          type="number"
          value={s3.urlExpirySecs ?? ''}
          placeholder="604800"
          onChange={(e) => set({ urlExpirySecs: numInputToValue(e.target.value) })}
        />
      </SettingsRow>
      <SettingsRow label="Lifecycle (days)">
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
  onStatus,
  onReloaded,
}: {
  data: ConfigApiResponse;
  onStatus: (msg: string, type?: string) => void;
  onReloaded: (data: ConfigApiResponse) => void;
}) {
  const [rawText, setRawText] = useState(data.raw || '');
  const [hasError, setHasError] = useState(true);
  const [errorMsg, setErrorMsg] = useState(data.parse_error || '');

  const save = async () => {
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
    onStatus('Saving...');
    try {
      const resp = await fetch('/api/config', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ config: parsed }),
      });
      const result = await resp.json();
      if (!resp.ok || result.error) {
        onStatus(result.error || 'Save failed', 'error');
        return;
      }
      onStatus('Saved! Reloading...', 'success');
      setTimeout(async () => {
        const r2 = await fetch('/api/config');
        onReloaded(await r2.json());
      }, 600);
    } catch (e: unknown) {
      onStatus(`Save failed: ${(e as Error).message}`, 'error');
    }
  };

  return (
    <div className="settings-group">
      <div className="settings-group-title" style={{ color: 'var(--accent-error)' }}>
        Config File Error
      </div>
      <p style={{ color: 'var(--dim)' }}>
        The config file has a JSON syntax error. Fix it below and save, or edit the file manually.
      </p>
      <p style={{ fontSize: 12, color: 'var(--dim)' }}>
        File: <code>{data.path}</code>
      </p>
      <div className="json-editor-wrap">
        <textarea
          className={`json-editor${hasError ? ' has-error' : ''}`}
          spellCheck={false}
          style={{ minHeight: 300 }}
          value={rawText}
          onChange={(e) => {
            setRawText(e.target.value);
            setHasError(false);
            setErrorMsg('');
          }}
        />
        {errorMsg && <div className="json-editor-error">{errorMsg}</div>}
      </div>
      <button className="btn-primary" style={{ marginTop: 10 }} onClick={save}>
        Save & Recover
      </button>
    </div>
  );
}

// ── Main SettingsPage component ───────────────────────────────────────────────

type TabId = 'tab-general' | 'tab-agents' | 'tab-models' | 'tab-mcp' | 'tab-s3';
type StatusType = 'idle' | 'loading' | 'success' | 'error';

export function SettingsPage() {
  const [visible, setVisible] = useState(false);
  const [config, setConfig] = useState<AppConfig>({});
  const [activeTab, setActiveTab] = useState<TabId>('tab-general');
  const [status, setStatus] = useState({ message: '', type: 'idle' as StatusType });
  const [corruptData, setCorruptData] = useState<ConfigApiResponse | null>(null);

  // Register bridge functions
  useEffect(() => {
    _open = () => setVisible(true);
    _close = () => setVisible(false);
    return () => {
      _open = null;
      _close = null;
    };
  }, []);

  // Toggle the container element's hidden attribute (React is mounted inside #settings-page)
  useEffect(() => {
    const el = document.getElementById('settings-page');
    if (el) el.hidden = !visible;
  }, [visible]);

  // Load config when opened
  useEffect(() => {
    if (!visible) return;
    const controller = new AbortController();
    (async () => {
      setStatus({ message: 'Loading...', type: 'loading' });
      try {
        const resp = await fetch('/api/config', { signal: controller.signal });
        if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
        const data: ConfigApiResponse = await resp.json();
        if (data.parse_error) {
          setCorruptData(data);
          setStatus({ message: 'Config file has syntax errors', type: 'error' });
          return;
        }
        setCorruptData(null);
        setConfig(data.config || {});
        setStatus({ message: `Loaded from ${data.path}`, type: 'success' });
      } catch (e: unknown) {
        if ((e as Error).name === 'AbortError') return;
        setStatus({ message: `Load failed: ${(e as Error).message}`, type: 'error' });
      }
    })();
    return () => controller.abort();
  }, [visible]);

  const handleStatus = useCallback((message: string, type = 'idle') => {
    setStatus({ message, type: type as StatusType });
  }, []);

  const validateAgentModels = (cfg: AppConfig): void => {
    const model = cfg.agents?.defaults?.model || {};
    const providers = cfg.models?.providers || {};
    const hasConfiguredProviders = Object.keys(providers).length > 0;

    for (const [key, val] of Object.entries(model)) {
      if (!val) continue;
      if ((val as string).includes('/')) {
        const [provName, ...rest] = (val as string).split('/');
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
      }
    }
  };

  const saveConfig = async () => {
    try {
      validateAgentModels(config);
    } catch (e: unknown) {
      setStatus({ message: (e as Error).message, type: 'error' });
      return;
    }

    // Clean up s3 if empty
    const s3 = config.s3;
    const finalConfig = { ...config };
    if (!s3?.bucket && !s3?.endpoint) delete finalConfig.s3;

    setStatus({ message: 'Saving...', type: 'loading' });
    try {
      const resp = await fetch('/api/config', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ config: finalConfig }),
      });
      const data = await resp.json();
      if (!resp.ok || data.error) {
        setStatus({ message: data.error || 'Save failed', type: 'error' });
        return;
      }
      setStatus({
        message: 'Saved successfully! Restart LingClaw to apply changes.',
        type: 'success',
      });
    } catch (e: unknown) {
      setStatus({ message: `Save failed: ${(e as Error).message}`, type: 'error' });
    }
  };

  if (!visible) return null;

  const tabs: Array<{ id: TabId; label: string }> = [
    { id: 'tab-general', label: 'General' },
    { id: 'tab-agents', label: 'Agents' },
    { id: 'tab-models', label: 'Models' },
    { id: 'tab-mcp', label: 'MCP' },
    { id: 'tab-s3', label: 'S3' },
  ];

  const statusClass =
    status.type === 'success'
      ? 'settings-status success'
      : status.type === 'error'
        ? 'settings-status error'
        : 'settings-status';

  // Render the inner page-panel; the outer #settings-page overlay is managed via useEffect above
  return (
    <div className="page-panel">
      <div className="page-header">
        <h2>Settings</h2>
        <div style={{ display: 'flex', alignItems: 'center', gap: 12 }}>
          <span className={statusClass} id="settings-status">
            {status.message}
          </span>
          {!corruptData && (
            <button className="btn-primary" id="settings-save-btn" onClick={saveConfig}>
              Save
            </button>
          )}
          <button
            className="page-close"
            title="Close"
            aria-label="Close"
            onClick={() => setVisible(false)}
          >
            ×
          </button>
        </div>
      </div>

      {corruptData ? (
        <div className="page-body" id="settings-body">
          <CorruptConfigView
            data={corruptData}
            onStatus={handleStatus}
            onReloaded={(d) => {
              if (!d.parse_error) {
                setCorruptData(null);
                setConfig(d.config || {});
                setStatus({ message: 'Loaded', type: 'success' });
              } else {
                setCorruptData(d);
              }
            }}
          />
        </div>
      ) : (
        <>
          <div id="settings-tabs" className="page-tabs">
            {tabs.map((t) => (
              <button
                key={t.id}
                className={`page-tab${activeTab === t.id ? ' active' : ''}`}
                data-tab={t.id}
                onClick={() => setActiveTab(t.id)}
              >
                {t.label}
              </button>
            ))}
          </div>
          <div className="page-body" id="settings-body">
            {activeTab === 'tab-general' && <GeneralTab config={config} onChange={setConfig} />}
            {activeTab === 'tab-agents' && <AgentsTab config={config} onChange={setConfig} />}
            {activeTab === 'tab-models' && (
              <ModelsTab config={config} onChange={setConfig} onStatus={handleStatus} />
            )}
            {activeTab === 'tab-mcp' && (
              <McpTab config={config} onChange={setConfig} onStatus={handleStatus} />
            )}
            {activeTab === 'tab-s3' && <S3Tab config={config} onChange={setConfig} />}
          </div>
        </>
      )}
    </div>
  );
}
