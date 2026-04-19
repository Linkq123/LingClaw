import { escHtml } from './utils.js';

let currentConfig = null;
let currentConfigPath = '';

const draftDirtyState = {
  modelsForm: false,
  modelsJson: false,
  mcpForm: false,
  mcpJson: false,
};

function resetDraftDirtyState() {
  draftDirtyState.modelsForm = false;
  draftDirtyState.modelsJson = false;
  draftDirtyState.mcpForm = false;
  draftDirtyState.mcpJson = false;
}

function resetModelsDraftState() {
  draftDirtyState.modelsForm = false;
  draftDirtyState.modelsJson = false;
}

function resetMcpDraftState() {
  draftDirtyState.mcpForm = false;
  draftDirtyState.mcpJson = false;
}

function cloneJsonValue(value) {
  return value === undefined ? undefined : JSON.parse(JSON.stringify(value));
}

function ensureModelsJsonAppliedForFormAction() {
  if (draftDirtyState.modelsJson) {
    throw new Error('Models Raw JSON has unapplied changes. Apply JSON before using form actions.');
  }
}

function ensureMcpJsonAppliedForFormAction() {
  if (draftDirtyState.mcpJson) {
    throw new Error('MCP Raw JSON has unapplied changes. Apply JSON before using form actions.');
  }
}

function isPlainObject(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function isAbsent(value) {
  return value === undefined || value === null;
}

function ensureOptionalString(value, label) {
  if (!isAbsent(value) && typeof value !== 'string') {
    throw new Error(`${label} must be a string.`);
  }
}

function ensureOptionalBoolean(value, label) {
  if (!isAbsent(value) && typeof value !== 'boolean') {
    throw new Error(`${label} must be a boolean.`);
  }
}

function ensureOptionalInteger(value, label) {
  if (!isAbsent(value) && (!Number.isInteger(value) || value < 0)) {
    throw new Error(`${label} must be a non-negative integer.`);
  }
}

function ensureStringArray(value, label) {
  if (!Array.isArray(value) || value.some(item => typeof item !== 'string')) {
    throw new Error(`${label} must be an array of strings.`);
  }
}

function ensureOptionalStringArray(value, label) {
  if (!isAbsent(value)) {
    ensureStringArray(value, label);
  }
}

function ensureStringRecord(value, label) {
  if (!isPlainObject(value)) {
    throw new Error(`${label} must be an object.`);
  }
  for (const [key, entry] of Object.entries(value)) {
    if (typeof entry !== 'string') {
      throw new Error(`${label}.${key} must be a string.`);
    }
  }
}

function parseNonNegativeIntegerField(rawValue, label) {
  const trimmed = rawValue.trim();
  if (trimmed === '') return undefined;
  if (!/^\d+$/.test(trimmed)) {
    throw new Error(`${label} must be a non-negative integer.`);
  }
  return parseInt(trimmed, 10);
}

function isBuiltinProviderName(name) {
  return ['openai', 'anthropic', 'ollama'].includes((name || '').toLowerCase());
}

function isSupportedProviderApiKind(value) {
  return ['openai-completions', 'anthropic', 'ollama'].includes((value || '').trim().toLowerCase());
}

function parsePositiveIntegerField(rawValue, label) {
  const parsed = parseNonNegativeIntegerField(rawValue, label);
  if (parsed === 0) {
    throw new Error(`${label} must be greater than 0.`);
  }
  return parsed;
}

function validateModelsConfigDraftShape(parsed) {
  if (!isPlainObject(parsed)) {
    throw new Error('Models JSON must be an object.');
  }
  if (parsed.providers !== undefined && !isPlainObject(parsed.providers)) {
    throw new Error('Models JSON field "providers" must be an object.');
  }
  if (!isPlainObject(parsed.providers || {})) {
    return;
  }
  for (const [name, provider] of Object.entries(parsed.providers || {})) {
    validateProviderName(name);
    if (!isPlainObject(provider)) {
      throw new Error(`Models JSON field "providers.${name}" must be an object.`);
    }
    ensureOptionalString(provider.api, `Models JSON field "providers.${name}.api"`);
    if (!isAbsent(provider.api) && !isSupportedProviderApiKind(provider.api)) {
      throw new Error(`Models JSON field "providers.${name}.api" must be one of: openai-completions, anthropic, ollama.`);
    }
    ensureOptionalString(provider.baseUrl, `Models JSON field "providers.${name}.baseUrl"`);
    ensureOptionalString(provider.apiKey, `Models JSON field "providers.${name}.apiKey"`);
    if (provider.models !== undefined && !Array.isArray(provider.models)) {
      throw new Error(`Models JSON field "providers.${name}.models" must be an array.`);
    }
    if (Array.isArray(provider.models)) {
      provider.models.forEach((model, index) => {
        if (!isPlainObject(model)) {
          throw new Error(`Models JSON field "providers.${name}.models[${index}]" must be an object.`);
        }
        if (typeof model.id !== 'string' || model.id.trim() === '') {
          throw new Error(`Models JSON field "providers.${name}.models[${index}].id" must be a non-empty string.`);
        }
        ensureOptionalString(model.name, `Models JSON field "providers.${name}.models[${index}].name"`);
        ensureOptionalBoolean(model.reasoning, `Models JSON field "providers.${name}.models[${index}].reasoning"`);
        ensureOptionalStringArray(model.input, `Models JSON field "providers.${name}.models[${index}].input"`);
        ensureOptionalInteger(model.contextWindow, `Models JSON field "providers.${name}.models[${index}].contextWindow"`);
        ensureOptionalInteger(model.maxTokens, `Models JSON field "providers.${name}.models[${index}].maxTokens"`);
      });
    }
  }
}

function validateMcpConfigDraftShape(parsed) {
  if (!isPlainObject(parsed)) {
    throw new Error('MCP JSON must be an object.');
  }
  for (const [name, server] of Object.entries(parsed)) {
    if (!isPlainObject(server)) {
      throw new Error(`MCP JSON field "${name}" must be an object.`);
    }
    ensureOptionalString(server.command, `MCP JSON field "${name}.command"`);
    if (!isAbsent(server.command) && server.command.trim() === '') {
      throw new Error(`MCP JSON field "${name}.command" cannot be empty.`);
    }
    ensureOptionalString(server.cwd, `MCP JSON field "${name}.cwd"`);
    if (!isAbsent(server.cwd)) {
      validateMcpCwdValue(server.cwd, `MCP JSON field "${name}.cwd"`);
    }
    if (!isAbsent(server.timeoutSecs) && server.timeoutSecs === 0) {
      throw new Error(`MCP JSON field "${name}.timeoutSecs" must be greater than 0.`);
    }
    ensureOptionalInteger(server.timeoutSecs, `MCP JSON field "${name}.timeoutSecs"`);
    ensureOptionalBoolean(server.enabled, `MCP JSON field "${name}.enabled"`);
    if (!isAbsent(server.args)) {
      ensureStringArray(server.args, `MCP JSON field "${name}.args"`);
    }
    if (!isAbsent(server.env)) {
      ensureStringRecord(server.env, `MCP JSON field "${name}.env"`);
    }
  }
}

function validateMcpCwdValue(value, fieldLabel = 'MCP cwd') {
  const raw = String(value || '').trim();
  if (!raw) return;
  const parts = raw.split(/[\\/]+/).filter(Boolean);
  if (parts.includes('.lingclaw-bootstrap')) {
    throw new Error(`${fieldLabel} targets protected internal workspace data.`);
  }
  const isAbsolute = /^[a-zA-Z]:[\\/]/.test(raw) || raw.startsWith('/') || raw.startsWith('\\');
  if (isAbsolute) {
    return;
  }
  let depth = 0;
  for (const part of parts) {
    if (part === '.') continue;
    if (part === '..') {
      if (depth === 0) {
        throw new Error(`${fieldLabel} must stay inside the session workspace.`);
      }
      depth -= 1;
      continue;
    }
    depth += 1;
  }
}

function syncModelsJsonEditorFromCurrentConfig() {
  const editor = document.getElementById('models-json-editor');
  if (!editor) return;
  editor.value = JSON.stringify(currentConfig.models || { providers: {} }, null, 2);
  editor.classList.remove('has-error');
  const errEl = document.getElementById('models-json-error');
  if (errEl) errEl.textContent = '';
  draftDirtyState.modelsJson = false;
}

function syncMcpJsonEditorFromCurrentConfig() {
  const editor = document.getElementById('mcp-json-editor');
  if (!editor) return;
  editor.value = JSON.stringify(currentConfig.mcpServers || {}, null, 2);
  editor.classList.remove('has-error');
  const errEl = document.getElementById('mcp-json-error');
  if (errEl) errEl.textContent = '';
  draftDirtyState.mcpJson = false;
}

const SETTINGS_TAB_CONTENTS = `
  <div class="tab-content active" id="tab-general"></div>
  <div class="tab-content" id="tab-agents"></div>
  <div class="tab-content" id="tab-models"></div>
  <div class="tab-content" id="tab-mcp"></div>
  <div class="tab-content" id="tab-s3"></div>`;

export function openSettingsPage() {
  const page = document.getElementById('settings-page');
  if (page) page.hidden = false;
  loadConfig();
}

export function closeSettingsPage() {
  const page = document.getElementById('settings-page');
  if (page) page.hidden = true;
}

export function initSettingsListeners() {
  const tabs = document.getElementById('settings-tabs');
  if (tabs) {
    tabs.addEventListener('click', e => {
      const btn = e.target.closest('.page-tab');
      if (!btn) return;
      tabs.querySelectorAll('.page-tab').forEach(t => t.classList.remove('active'));
      btn.classList.add('active');
      const tabId = btn.dataset.tab;
      document.querySelectorAll('#settings-body .tab-content').forEach(c => {
        c.classList.toggle('active', c.id === tabId);
      });
      if (tabId === 'tab-agents') {
        syncAgentModelDraftFromInputs();
        renderAgentsTab();
      }
    });
  }

  const saveBtn = document.getElementById('settings-save-btn');
  if (saveBtn) {
    saveBtn.addEventListener('click', saveConfig);
  }
}

async function loadConfig() {
  setStatus('Loading...');
  try {
    const resp = await fetch('/api/config');
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    const data = await resp.json();
    if (data.parse_error) {
      currentConfig = null;
      currentConfigPath = data.path || '';
      showCorruptedConfigView(data);
      setStatus('Config file has syntax errors', 'error');
      return;
    }
    currentConfig = data.config || {};
    currentConfigPath = data.path || '';
    resetDraftDirtyState();
    showNormalSettingsView();
    renderAllTabs();
    setStatus(`Loaded from ${currentConfigPath}`, 'success');
  } catch (e) {
    setStatus(`Load failed: ${e.message}`, 'error');
  }
}

async function saveConfig() {
  // Handle raw-editor recovery mode (config file had parse errors).
  const rawEditor = document.getElementById('raw-config-editor');
  if (rawEditor) return saveRawConfig(rawEditor);
  if (!currentConfig) return;

  // Collect values from each tab
  try {
    collectGeneralTab();
    collectModelsTab();
    collectAgentsTab();
    collectMcpTab();
    collectS3Tab();
  } catch (e) {
    setStatus(e.message, 'error');
    return;
  }

  setStatus('Saving...');
  try {
    const resp = await fetch('/api/config', {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ config: currentConfig }),
    });
    const data = await resp.json();
    if (!resp.ok || data.error) {
      const msg = data.error || 'Save failed';
      setStatus(msg, 'error');
      if (data.line || data.column) {
        highlightJsonError(data.line, data.column);
      }
      return;
    }
    setStatus('Saved successfully! Restart LingClaw to apply changes.', 'success');
  } catch (e) {
    setStatus(`Save failed: ${e.message}`, 'error');
  }
}

function setStatus(msg, type) {
  const el = document.getElementById('settings-status');
  if (!el) return;
  el.textContent = msg;
  el.className = 'settings-status' + (type ? ` ${type}` : '');
}

function highlightJsonError(_line, _column) {
  // Find any visible json-editor and mark it
  const editors = document.querySelectorAll('.json-editor');
  editors.forEach(ed => ed.classList.add('has-error'));
}

function showCorruptedConfigView(data) {
  const tabs = document.getElementById('settings-tabs');
  if (tabs) tabs.hidden = true;

  const body = document.getElementById('settings-body');
  if (!body) return;
  body.innerHTML = `<div class="settings-group">
    <div class="settings-group-title" style="color:var(--accent-error)">Config File Error</div>
    <p style="color:var(--dim)">The config file has a JSON syntax error. Fix it below and save, or edit the file manually.</p>
    <p style="font-size:12px;color:var(--dim)">File: <code>${escHtml(currentConfigPath)}</code></p>
    <div class="json-editor-wrap">
      <textarea class="json-editor has-error" id="raw-config-editor" spellcheck="false" style="min-height:300px">${escHtml(data.raw || '')}</textarea>
      <div class="json-editor-error">${escHtml(data.parse_error)}</div>
    </div>
  </div>`;
}

function showNormalSettingsView() {
  const tabs = document.getElementById('settings-tabs');
  if (tabs) tabs.hidden = false;

  const body = document.getElementById('settings-body');
  if (!body) return;

  const hasAllTabContainers = ['tab-general', 'tab-models', 'tab-agents', 'tab-mcp', 'tab-s3']
    .every(id => body.querySelector(`#${id}`));
  if (!hasAllTabContainers) {
    body.innerHTML = SETTINGS_TAB_CONTENTS;
    const activeTabId = tabs?.querySelector('.page-tab.active')?.dataset.tab || 'tab-general';
    body.querySelectorAll('.tab-content').forEach(content => {
      content.classList.toggle('active', content.id === activeTabId);
    });
  }
}

// ── Render tabs ──

function renderAllTabs() {
  showNormalSettingsView();
  renderGeneralTab();
  renderModelsTab();
  renderAgentsTab();
  renderMcpTab();
  renderS3Tab();
}

function renderGeneralTab() {
  const container = document.getElementById('tab-general');
  if (!container) return;
  const s = currentConfig.settings || {};
  container.innerHTML = `
    <div class="settings-group">
      <div class="settings-group-title">Server</div>
      ${row('Port', inputNum('cfg-port', s.port, 18989))}
    </div>
    <div class="settings-group">
      <div class="settings-group-title">Timeouts (seconds)</div>
      ${row('Exec Timeout', inputNum('cfg-exec-timeout', s.execTimeout, 30))}
      ${row('Tool Timeout', inputNum('cfg-tool-timeout', s.toolTimeout, 30))}
      ${row('Sub-Agent Timeout', inputNum('cfg-sub-agent-timeout', s.subAgentTimeout, 300))}
      ${row('Max LLM Retries', inputNum('cfg-max-retries', s.maxLlmRetries, 2))}
    </div>
    <div class="settings-group">
      <div class="settings-group-title">Context</div>
      ${row('Max Context Tokens', inputNum('cfg-max-context', s.maxContextTokens, 32000))}
      ${row('Max Output Bytes', inputNum('cfg-max-output', s.maxOutputBytes, 51200))}
      ${row('Max File Bytes', inputNum('cfg-max-file', s.maxFileBytes, 204800))}
    </div>
    <div class="settings-group">
      <div class="settings-group-title">Features</div>
      ${row('Structured Memory', triState('cfg-structured-memory', s.structuredMemory))}
      ${row('Daily Reflection', triState('cfg-daily-reflection', s.dailyReflection))}
      ${row('Enable S3', triState('cfg-enable-s3', s.enableS3))}
      ${row('OpenAI Stream Usage', triState('cfg-openai-stream-usage', s.openaiStreamIncludeUsage))}
      ${row('Anthropic Prompt Caching', triState('cfg-anthropic-cache', s.anthropicPromptCaching))}
    </div>
  `;
}

function collectGeneralTab() {
  if (!currentConfig.settings) currentConfig.settings = {};
  const s = currentConfig.settings;
  s.port = numVal('cfg-port');
  s.execTimeout = numVal('cfg-exec-timeout');
  s.toolTimeout = numVal('cfg-tool-timeout');
  s.subAgentTimeout = numVal('cfg-sub-agent-timeout');
  s.maxLlmRetries = numVal('cfg-max-retries');
  s.maxContextTokens = numVal('cfg-max-context');
  s.maxOutputBytes = numVal('cfg-max-output');
  s.maxFileBytes = numVal('cfg-max-file');
  s.structuredMemory = triVal('cfg-structured-memory');
  s.dailyReflection = triVal('cfg-daily-reflection');
  s.enableS3 = triVal('cfg-enable-s3');
  s.openaiStreamIncludeUsage = triVal('cfg-openai-stream-usage');
  s.anthropicPromptCaching = triVal('cfg-anthropic-cache');
}

function renderModelsTab() {
  const container = document.getElementById('tab-models');
  if (!container) return;
  resetModelsDraftState();
  const providers = currentConfig.models?.providers || {};
  const names = Object.keys(providers).sort();

  let html = '<div id="models-provider-list">';
  for (const name of names) {
    html += renderProviderForm(name, providers[name]);
  }
  html += '</div>';
  html += `<button class="btn-secondary" id="add-provider-btn" style="margin-top:10px">+ Add Provider</button>`;
  html += `<details style="margin-top:16px"><summary style="font-size:12px;color:var(--dim);cursor:pointer">Advanced: Raw JSON</summary>
    <div class="json-editor-wrap" style="margin-top:8px">
      <textarea class="json-editor" id="models-json-editor" spellcheck="false">${escHtml(JSON.stringify(currentConfig.models || { providers: {} }, null, 2))}</textarea>
      <div class="json-editor-error" id="models-json-error"></div>
      <button class="btn-secondary" style="margin-top:6px" id="models-json-apply">Apply JSON</button>
    </div>
  </details>`;
  container.innerHTML = html;

  bindModelsTabInteractions(container);
}

function renderProviderForm(name, provider) {
  const models = provider.models || [];
  const apiVal = (provider.api || 'openai-completions').trim().toLowerCase();
  const selectedModelId = preferredTestModelId(name, models);
  const modelSelectHtml = models.length > 0
    ? `<select data-provider-model-select="${escHtml(name)}" style="max-width:190px;padding:5px 8px">
        ${models.map(m => {
          const sel = m.id === selectedModelId ? ' selected' : '';
          return `<option value="${escHtml(m.id)}"${sel}>${escHtml(m.id)}</option>`;
        }).join('')}
      </select>`
    : '';

  let modelsHtml = '';
  for (let i = 0; i < models.length; i++) {
    modelsHtml += renderModelEntryForm(name, i, models[i]);
  }

  return `
    <div class="provider-card" data-provider-name="${escHtml(name)}">
      <div class="provider-card-header">
        <span class="provider-card-name">${escHtml(name)}</span>
        <div style="display:flex;gap:6px;align-items:center;flex-wrap:wrap;justify-content:flex-end">
          ${modelSelectHtml}
          <button class="btn-test" data-test-provider="${escHtml(name)}">Test</button>
          <button class="btn-danger-sm" data-delete-provider="${escHtml(name)}" title="Delete provider">✕</button>
        </div>
      </div>
      <div class="provider-form" style="display:grid;gap:8px;margin-top:8px">
        ${row('API Type', `<select data-prov-field="api" data-prov="${escHtml(name)}">
          <option value="openai-completions"${apiVal === 'openai-completions' ? ' selected' : ''}>OpenAI Completions</option>
          <option value="anthropic"${apiVal === 'anthropic' ? ' selected' : ''}>Anthropic</option>
          <option value="ollama"${apiVal === 'ollama' ? ' selected' : ''}>Ollama</option>
        </select>`)}
        ${row('Base URL', `<input type="text" data-prov-field="baseUrl" data-prov="${escHtml(name)}" value="${escHtml(provider.baseUrl || '')}" placeholder="https://api.openai.com/v1">`)}
        ${row('API Key', `<input type="password" data-prov-field="apiKey" data-prov="${escHtml(name)}" value="${escHtml(provider.apiKey || '')}">`)}
      </div>
      <div style="margin-top:10px">
        <div style="font-size:12px;font-weight:600;margin-bottom:6px;color:var(--fg)">Models</div>
        <div class="provider-models-forms" data-prov-models="${escHtml(name)}">${modelsHtml}</div>
        <button class="btn-secondary" data-add-model="${escHtml(name)}" style="margin-top:6px;font-size:11px">+ Add Model</button>
      </div>
    </div>`;
}

function renderModelEntryForm(provName, idx, model) {
  const inputArr = Array.isArray(model.input) ? model.input : ['text'];
  const hasText = inputArr.includes('text');
  const hasImage = inputArr.includes('image');
  return `
    <div class="model-entry-form" data-model-idx="${idx}" style="border:1px solid var(--border);border-radius:6px;padding:8px;margin-bottom:6px;background:var(--bg)">
      <div style="display:flex;gap:6px;align-items:center;flex-wrap:wrap">
        <input type="text" data-model-field="id" data-prov="${escHtml(provName)}" data-idx="${idx}" value="${escHtml(model.id || '')}" placeholder="model-id" style="flex:1;min-width:120px">
        <label style="font-size:11px;display:flex;align-items:center;gap:3px;color:var(--dim)">
          <input type="checkbox" data-model-field="reasoning" data-prov="${escHtml(provName)}" data-idx="${idx}" ${model.reasoning ? 'checked' : ''}> Reasoning
        </label>
        <button class="btn-danger-sm" data-delete-model="${escHtml(provName)}" data-model-idx="${idx}" title="Remove model">✕</button>
      </div>
      <div style="display:flex;gap:8px;margin-top:6px;flex-wrap:wrap;align-items:center">
        <label style="font-size:11px;color:var(--dim);display:flex;align-items:center;gap:4px">
          Context Window <input type="number" data-model-field="contextWindow" data-prov="${escHtml(provName)}" data-idx="${idx}" value="${model.contextWindow ?? ''}" placeholder="128000" style="width:90px">
        </label>
        <label style="font-size:11px;color:var(--dim);display:flex;align-items:center;gap:4px">
          Max Tokens <input type="number" data-model-field="maxTokens" data-prov="${escHtml(provName)}" data-idx="${idx}" value="${model.maxTokens ?? ''}" placeholder="16384" style="width:90px">
        </label>
        <span style="font-size:11px;color:var(--dim);display:flex;align-items:center;gap:6px;margin-left:4px">
          Input:
          <label style="display:flex;align-items:center;gap:2px">
            <input type="checkbox" data-model-field="input-text" data-prov="${escHtml(provName)}" data-idx="${idx}" ${hasText ? 'checked' : ''}> Text
          </label>
          <label style="display:flex;align-items:center;gap:2px">
            <input type="checkbox" data-model-field="input-image" data-prov="${escHtml(provName)}" data-idx="${idx}" ${hasImage ? 'checked' : ''}> Image
          </label>
        </span>
      </div>
    </div>`;
}

function collectModelsTab() {
  if (draftDirtyState.modelsJson && draftDirtyState.modelsForm) {
    throw new Error('Models form and Raw JSON both have unapplied changes. Apply the JSON first, or discard one side before saving.');
  }
  if (draftDirtyState.modelsJson) {
    const parsed = readModelsConfigFromEditor();
    if (parsed === undefined) delete currentConfig.models;
    else currentConfig.models = parsed;
    draftDirtyState.modelsJson = false;
    return;
  }
  collectModelsFromForms({
    preserveBlankExistingEntries: false,
    syncEditor: true,
    clearDirty: true,
  });
}

function collectModelsFromForms(options = {}) {
  ensureModelsJsonAppliedForFormAction();
  const {
    preserveBlankExistingEntries = true,
    syncEditor = false,
    clearDirty = false,
  } = options;
  const existingModels = currentConfig.models;
  const existingProviders = existingModels?.providers || {};
  const providers = {};

  // Collect all provider cards
  document.querySelectorAll('#models-provider-list .provider-card').forEach(card => {
    const name = card.dataset.providerName;
    if (!name) return;
    const existingProvider = existingProviders[name] || {};
    const prov = cloneJsonValue(existingProvider) || {};

    const apiEl = card.querySelector('[data-prov-field="api"]');
    if (apiEl) prov.api = apiEl.value;
    const baseUrlEl = card.querySelector('[data-prov-field="baseUrl"]');
    if (baseUrlEl) prov.baseUrl = baseUrlEl.value.trim();
    const apiKeyEl = card.querySelector('[data-prov-field="apiKey"]');
    if (apiKeyEl) prov.apiKey = apiKeyEl.value.trim();

    // Collect models
    const models = [];
    card.querySelectorAll('.model-entry-form').forEach(mForm => {
      const idx = parseInt(mForm.dataset.modelIdx || '-1', 10);
      const existingModel = idx >= 0 ? existingProvider.models?.[idx] : undefined;
      const idEl = mForm.querySelector('[data-model-field="id"]');
      const id = idEl ? idEl.value.trim() : '';
      const reasoningEl = mForm.querySelector('[data-model-field="reasoning"]');
      const cwEl = mForm.querySelector('[data-model-field="contextWindow"]');
      const mtEl = mForm.querySelector('[data-model-field="maxTokens"]');
      const inputTextEl = mForm.querySelector('[data-model-field="input-text"]');
      const inputImageEl = mForm.querySelector('[data-model-field="input-image"]');
      if (!id) {
        const hasEditableInput = Boolean(
          (reasoningEl && reasoningEl.checked)
          || (cwEl && cwEl.value !== '')
          || (mtEl && mtEl.value !== '')
          || (inputImageEl && inputImageEl.checked)
        );
        if (hasEditableInput || (!preserveBlankExistingEntries && existingModel)) {
          throw new Error('Model id cannot be empty.');
        }
        if (preserveBlankExistingEntries && existingModel) {
          models.push(cloneJsonValue(existingModel));
        }
        return;
      }
      const entry = cloneJsonValue(existingModel) || {};
      entry.id = id;
      if (reasoningEl && reasoningEl.checked) entry.reasoning = true;
      else delete entry.reasoning;
      const parsedContextWindow = cwEl
        ? parseNonNegativeIntegerField(cwEl.value, 'Context Window')
        : undefined;
      if (parsedContextWindow === undefined) delete entry.contextWindow;
      else entry.contextWindow = parsedContextWindow;
      const parsedMaxTokens = mtEl
        ? parseNonNegativeIntegerField(mtEl.value, 'Max Tokens')
        : undefined;
      if (parsedMaxTokens === undefined) delete entry.maxTokens;
      else entry.maxTokens = parsedMaxTokens;
      const inputArr = [];
      if (inputTextEl && inputTextEl.checked) inputArr.push('text');
      if (inputImageEl && inputImageEl.checked) inputArr.push('image');
      if (inputArr.length > 0) entry.input = inputArr;
      else delete entry.input;
      models.push(entry);
    });
    prov.models = models;
    providers[name] = prov;
  });

  if (Object.keys(providers).length > 0) {
    if (!currentConfig.models || !isPlainObject(currentConfig.models)) currentConfig.models = {};
    currentConfig.models.providers = providers;
  } else if (isPlainObject(currentConfig.models)) {
    delete currentConfig.models.providers;
    if (Object.keys(currentConfig.models).length === 0) {
      delete currentConfig.models;
    }
  } else if (existingModels === undefined) {
    delete currentConfig.models;
  }
  if (syncEditor) syncModelsJsonEditorFromCurrentConfig();
  if (clearDirty) draftDirtyState.modelsForm = false;
}

function renderAgentsTab() {
  const container = document.getElementById('tab-agents');
  if (!container) return;
  syncAgentModelDraftFromInputs();
  const model = currentConfig.agents?.defaults?.model || {};
  const providers = getModelsProvidersForUi();
  const allModels = buildModelOptions(providers);

  container.innerHTML = `
    <div class="settings-group">
      <div class="settings-group-title">Agent Default Models</div>
      <p style="font-size:12px;color:var(--dim);margin-bottom:12px">Models must reference a provider configured in the Models tab (format: <code>provider/model-id</code>).</p>
      ${row('Primary', modelSelect('cfg-agent-primary', model.primary, allModels))}
      ${row('Fast', modelSelect('cfg-agent-fast', model.fast, allModels))}
      ${row('Sub-Agent', modelSelect('cfg-agent-sub-agent', model['sub-agent'], allModels))}
      ${row('Memory', modelSelect('cfg-agent-memory', model.memory, allModels))}
      ${row('Reflection', modelSelect('cfg-agent-reflection', model.reflection, allModels))}
      ${row('Context', modelSelect('cfg-agent-context', model.context, allModels))}
    </div>`;
}

function collectAgentsTab() {
  syncAgentModelDraftFromInputs();
  const m = currentConfig.agents.defaults.model;

  // Validate against providers
  const providers = currentConfig.models?.providers || {};
  const hasConfiguredProviders = Object.keys(providers).length > 0;
  for (const [key, val] of Object.entries(m)) {
    if (!val) continue;
    if (val.includes('/')) {
      const [provName, ...rest] = val.split('/');
      const modelId = rest.join('/');
      if (!modelId || !modelId.trim()) {
        throw new Error(`Agent model "${key}": model id cannot be empty after provider prefix.`);
      }
      if (hasConfiguredProviders && !providers[provName]) {
        throw new Error(`Agent model "${key}" references unknown provider "${provName}". Add it in Models tab first.`);
      }
      if (!hasConfiguredProviders && !isBuiltinProviderName(provName)) {
        throw new Error(`Agent model "${key}" references unsupported provider prefix "${provName}". Use openai, anthropic, ollama, or configure it in Models tab first.`);
      }
      if (hasConfiguredProviders && providers[provName]) {
        const models = providers[provName].models || [];
        if (models.length > 0 && modelId && !models.some(m => m.id === modelId)) {
          throw new Error(`Agent model "${key}" references unknown model "${modelId}" for provider "${provName}". Add it in Models tab first.`);
        }
      }
    }
  }
}

function renderMcpTab() {
  const container = document.getElementById('tab-mcp');
  if (!container) return;
  resetMcpDraftState();
  const servers = currentConfig.mcpServers || {};
  const names = Object.keys(servers).sort();

  let html = '<div id="mcp-server-list">';
  for (const name of names) {
    html += renderMcpServerForm(name, servers[name]);
  }
  html += '</div>';
  html += `<button class="btn-secondary" id="add-mcp-btn" style="margin-top:10px">+ Add MCP Server</button>`;
  html += `<details style="margin-top:16px"><summary style="font-size:12px;color:var(--dim);cursor:pointer">Advanced: Raw JSON</summary>
    <div class="json-editor-wrap" style="margin-top:8px">
      <textarea class="json-editor" id="mcp-json-editor" spellcheck="false">${escHtml(JSON.stringify(currentConfig.mcpServers || {}, null, 2))}</textarea>
      <div class="json-editor-error" id="mcp-json-error"></div>
      <button class="btn-secondary" style="margin-top:6px" id="mcp-json-apply">Apply JSON</button>
    </div>
  </details>`;
  container.innerHTML = html;

  bindMcpTabInteractions(container);
}

function renderMcpServerForm(name, server) {
  const argsStr = (server.args || []).join('\n');
  const envEntries = Object.entries(server.env || {});
  let envHtml = '';
  for (const [k, v] of envEntries) {
    envHtml += renderEnvEntryForm(name, k, v);
  }

  return `
    <div class="provider-card" data-mcp-name="${escHtml(name)}">
      <div class="provider-card-header">
        <span class="provider-card-name">${escHtml(name)}</span>
        <div style="display:flex;gap:6px;align-items:center">
          <label style="font-size:11px;display:flex;align-items:center;gap:3px;color:var(--dim)">
            <input type="checkbox" data-mcp-field="enabled" data-mcp="${escHtml(name)}" ${server.enabled !== false ? 'checked' : ''}> Enabled
          </label>
          <button class="btn-test" data-test-mcp="${escHtml(name)}">Test</button>
          <button class="btn-danger-sm" data-delete-mcp="${escHtml(name)}" title="Delete server">✕</button>
        </div>
      </div>
      <div class="provider-form" style="display:grid;gap:8px;margin-top:8px">
        ${row('Command', `<input type="text" data-mcp-field="command" data-mcp="${escHtml(name)}" value="${escHtml(server.command || '')}" placeholder="uvx">`)}
        ${row('Args (one per line)', `<textarea data-mcp-field="args" data-mcp="${escHtml(name)}" rows="3" style="font-family:var(--font-mono);font-size:12px" placeholder="One argument per line">${escHtml(argsStr)}</textarea>`)}
        ${row('CWD', `<input type="text" data-mcp-field="cwd" data-mcp="${escHtml(name)}" value="${escHtml(server.cwd || '')}" placeholder="Optional working directory">`)}
        ${row('Timeout (s)', `<input type="number" data-mcp-field="timeoutSecs" data-mcp="${escHtml(name)}" value="${server.timeoutSecs ?? ''}" placeholder="Default">`)}
      </div>
      <div style="margin-top:10px">
        <div style="font-size:12px;font-weight:600;margin-bottom:6px;color:var(--fg)">Environment Variables</div>
        <div class="mcp-env-entries" data-mcp-env="${escHtml(name)}">${envHtml}</div>
        <button class="btn-secondary" data-add-env="${escHtml(name)}" style="margin-top:6px;font-size:11px">+ Add Env Var</button>
      </div>
    </div>`;
}

function renderEnvEntryForm(mcpName, key, value) {
  return `
    <div class="env-entry-form" style="display:flex;gap:6px;align-items:center;margin-bottom:4px">
      <input type="text" data-env-key data-mcp="${escHtml(mcpName)}" value="${escHtml(key)}" placeholder="KEY" style="flex:1;min-width:80px;font-size:12px">
      <input type="text" data-env-val data-mcp="${escHtml(mcpName)}" value="${escHtml(value)}" placeholder="value" style="flex:2;font-size:12px">
      <button class="btn-danger-sm" data-delete-env="${escHtml(mcpName)}" title="Remove">✕</button>
    </div>`;
}

function bindMcpTabInteractions(container) {
  if (!container.dataset.draftTrackingBound) {
    container.addEventListener('input', e => {
      if (e.target instanceof Element && e.target.id === 'mcp-json-editor') {
        draftDirtyState.mcpJson = true;
        return;
      }
      if (e.target instanceof Element && e.target.matches('[data-mcp-field], [data-env-key], [data-env-val]')) {
        draftDirtyState.mcpForm = true;
      }
    });
    container.addEventListener('change', e => {
      if (e.target instanceof Element && e.target.matches('[data-mcp-field], [data-env-key], [data-env-val]')) {
        draftDirtyState.mcpForm = true;
      }
    });
    container.dataset.draftTrackingBound = 'true';
  }
  // Test buttons
  container.querySelectorAll('[data-test-mcp]').forEach(btn => {
    btn.addEventListener('click', () => testMcpFromForm(btn, btn.dataset.testMcp));
  });
  // Delete server
  container.querySelectorAll('[data-delete-mcp]').forEach(btn => {
    btn.addEventListener('click', () => {
      try {
        collectMcpFromForms();
      } catch (e) {
        setStatus(e.message, 'error');
        return;
      }
      const name = btn.dataset.deleteMcp;
      if (currentConfig.mcpServers) delete currentConfig.mcpServers[name];
      renderMcpTab();
    });
  });
  // Add server
  const addBtn = container.querySelector('#add-mcp-btn');
  if (addBtn) {
    addBtn.addEventListener('click', () => {
      try {
        collectMcpFromForms();
      } catch (e) {
        setStatus(e.message, 'error');
        return;
      }
      const name = prompt('Enter MCP server name:');
      if (!name || !name.trim()) return;
      const trimmed = name.trim();
      if (/[/\s]/.test(trimmed)) {
        setStatus('Server name cannot contain "/" or whitespace.', 'error');
        return;
      }
      if (!/^[a-zA-Z0-9._-]+$/.test(trimmed)) {
        setStatus('Server name may only contain letters, numbers, ".", "-" or "_".', 'error');
        return;
      }
      if (!currentConfig.mcpServers) currentConfig.mcpServers = {};
      if (currentConfig.mcpServers[trimmed]) {
        setStatus(`Server "${trimmed}" already exists`, 'error');
        return;
      }
      currentConfig.mcpServers[trimmed] = { command: '', args: [], env: {}, enabled: true };
      renderMcpTab();
    });
  }
  // Add env var
  container.querySelectorAll('[data-add-env]').forEach(btn => {
    btn.addEventListener('click', () => {
      const mcpName = btn.dataset.addEnv;
      const card = btn.closest('.provider-card');
      const envContainer = card ? card.querySelector('.mcp-env-entries') : null;
      if (envContainer) {
        envContainer.insertAdjacentHTML('beforeend', renderEnvEntryForm(mcpName, '', ''));
        bindEnvDeleteButtons(envContainer);
      }
    });
  });
  // Delete env var
  container.querySelectorAll('.mcp-env-entries').forEach(envC => bindEnvDeleteButtons(envC));
  // Apply JSON
  const applyBtn = container.querySelector('#mcp-json-apply');
  if (applyBtn) {
    applyBtn.addEventListener('click', () => {
      if (draftDirtyState.mcpForm) {
        setStatus('MCP form has unapplied changes. Save or discard them before applying Raw JSON.', 'error');
        return;
      }
      try {
        const parsed = readMcpConfigFromEditor();
        if (parsed === undefined) delete currentConfig.mcpServers;
        else currentConfig.mcpServers = parsed;
        renderMcpTab();
        setStatus('Applied MCP JSON', 'success');
      } catch (e) {
        setStatus(e.message, 'error');
      }
    });
  }
}

function bindEnvDeleteButtons(envContainer) {
  envContainer.querySelectorAll('[data-delete-env]').forEach(btn => {
    if (btn._bound) return;
    btn._bound = true;
    btn.addEventListener('click', () => {
      draftDirtyState.mcpForm = true;
      btn.closest('.env-entry-form')?.remove();
    });
  });
}

function collectMcpTab() {
  if (draftDirtyState.mcpJson && draftDirtyState.mcpForm) {
    throw new Error('MCP form and Raw JSON both have unapplied changes. Apply the JSON first, or discard one side before saving.');
  }
  if (draftDirtyState.mcpJson) {
    const parsed = readMcpConfigFromEditor();
    if (parsed === undefined) delete currentConfig.mcpServers;
    else currentConfig.mcpServers = parsed;
    draftDirtyState.mcpJson = false;
    return;
  }
  collectMcpFromForms();
}

function collectMcpFromForms() {
  ensureMcpJsonAppliedForFormAction();
  const existingServers = currentConfig.mcpServers || {};
  const servers = {};
  document.querySelectorAll('#mcp-server-list .provider-card').forEach(card => {
    const name = card.dataset.mcpName;
    if (!name) return;
    const s = cloneJsonValue(existingServers[name]) || {};
    const cmdEl = card.querySelector('[data-mcp-field="command"]');
    s.command = cmdEl ? cmdEl.value.trim() : '';
    const argsEl = card.querySelector('[data-mcp-field="args"]');
    s.args = argsEl ? argsEl.value.split('\n').map(a => a.trim()).filter(Boolean) : [];
    const cwdEl = card.querySelector('[data-mcp-field="cwd"]');
    if (cwdEl && cwdEl.value.trim()) {
      validateMcpCwdValue(cwdEl.value, 'MCP CWD');
      s.cwd = cwdEl.value.trim();
    }
    else delete s.cwd;
    const toEl = card.querySelector('[data-mcp-field="timeoutSecs"]');
    const parsedTimeout = toEl
      ? parsePositiveIntegerField(toEl.value, 'Timeout (s)')
      : undefined;
    if (parsedTimeout === undefined) delete s.timeoutSecs;
    else s.timeoutSecs = parsedTimeout;
    const enEl = card.querySelector('[data-mcp-field="enabled"]');
    s.enabled = enEl ? enEl.checked : true;

    // Collect env
    const env = {};
    card.querySelectorAll('.env-entry-form').forEach(ef => {
      const keyEl = ef.querySelector('[data-env-key]');
      const valEl = ef.querySelector('[data-env-val]');
      const k = keyEl ? keyEl.value.trim() : '';
      const v = valEl ? valEl.value : '';
      if (k) env[k] = v;
    });
    if (Object.keys(env).length > 0) s.env = env;
    else delete s.env;

    servers[name] = s;
  });
  currentConfig.mcpServers = Object.keys(servers).length > 0 ? servers : undefined;
  syncMcpJsonEditorFromCurrentConfig();
  draftDirtyState.mcpForm = false;
}

function renderS3Tab() {
  const container = document.getElementById('tab-s3');
  if (!container) return;
  const s3 = currentConfig.s3 || {};
  container.innerHTML = `
    <div class="settings-group">
      <div class="settings-group-title">S3-Compatible File Storage</div>
      ${row('Endpoint', inputText('cfg-s3-endpoint', s3.endpoint, 'https://s3.us-east-1.amazonaws.com'))}
      ${row('Region', inputText('cfg-s3-region', s3.region, 'us-east-1'))}
      ${row('Bucket', inputText('cfg-s3-bucket', s3.bucket))}
      ${row('Access Key', inputText('cfg-s3-access-key', s3.accessKey))}
      ${row('Secret Key', inputPassword('cfg-s3-secret-key', s3.secretKey))}
      ${row('Prefix', inputText('cfg-s3-prefix', s3.prefix, 'lingclaw/images/'))}
      ${row('URL Expiry (s)', inputNum('cfg-s3-expiry', s3.urlExpirySecs, 604800))}
      ${row('Lifecycle (days)', inputNum('cfg-s3-lifecycle', s3.lifecycleDays, 14))}
    </div>`;
}

function collectS3Tab() {
  const endpoint = strVal('cfg-s3-endpoint');
  const bucket = strVal('cfg-s3-bucket');
  if (!bucket && !endpoint) { currentConfig.s3 = undefined; return; }
  currentConfig.s3 = {
    endpoint: endpoint || undefined,
    region: strVal('cfg-s3-region') || undefined,
    bucket: bucket || undefined,
    accessKey: strVal('cfg-s3-access-key') || undefined,
    secretKey: strVal('cfg-s3-secret-key') || undefined,
    prefix: strVal('cfg-s3-prefix') || undefined,
    urlExpirySecs: numVal('cfg-s3-expiry'),
    lifecycleDays: numVal('cfg-s3-lifecycle'),
  };
}

// ── Test actions ──

async function testProvider(btn, providerName) {
  // Collect current form state
  try {
    collectModelsFromForms();
  } catch (e) {
    setStatus(e.message, 'error');
    return;
  }
  const p = currentConfig.models?.providers?.[providerName];
  if (!p) {
    setStatus(`Provider "${providerName}" not found.`, 'error');
    return;
  }

  const baseUrl = p.baseUrl || '';
  const apiKey = p.apiKey || '';
  const modelId = resolveProviderTestModelId(btn, providerName, p.models || []);
  if (!modelId) {
    btn.textContent = 'No models';
    btn.className = 'btn-test test-fail';
    return;
  }

  btn.textContent = 'Testing...';
  btn.className = 'btn-test testing';
  try {
    const resp = await fetch('/api/config/test-model', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        baseUrl, apiKey, api: p.api || 'openai-completions', modelId,
      }),
    });
    const data = await resp.json();
    if (data.ok) {
      btn.textContent = '✓ Connected';
      btn.className = 'btn-test test-ok';
    } else {
      btn.textContent = '✗ Failed';
      btn.title = data.error || 'Connection failed';
      btn.className = 'btn-test test-fail';
    }
  } catch (e) {
    btn.textContent = '✗ Error';
    btn.title = e.message;
    btn.className = 'btn-test test-fail';
  }
  setTimeout(() => { btn.textContent = 'Test'; btn.className = 'btn-test'; btn.title = ''; }, 4000);
}

async function testMcpFromForm(btn, serverName) {
  try {
    collectMcpFromForms();
  } catch (e) {
    setStatus(e.message, 'error');
    return;
  }
  const s = currentConfig.mcpServers?.[serverName];
  if (!s) {
    setStatus(`MCP server "${serverName}" not found.`, 'error');
    return;
  }

  btn.textContent = 'Testing...';
  btn.className = 'btn-test testing';
  try {
    const resp = await fetch('/api/config/test-mcp', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        command: s.command, args: s.args, env: s.env, cwd: s.cwd, timeoutSecs: s.timeoutSecs,
      }),
    });
    const data = await resp.json();
    if (data.ok) {
      btn.textContent = `✓ ${data.tools} tools`;
      btn.className = 'btn-test test-ok';
    } else {
      btn.textContent = '✗ Failed';
      btn.title = data.error || 'Connection failed';
      btn.className = 'btn-test test-fail';
    }
  } catch (e) {
    btn.textContent = '✗ Error';
    btn.title = e.message;
    btn.className = 'btn-test test-fail';
  }
  setTimeout(() => { btn.textContent = 'Test'; btn.className = 'btn-test'; btn.title = ''; }, 4000);
}

async function testMcp(btn, serverName) {
  let s;
  try {
    const servers = readMcpConfigFromEditor() || {};
    s = servers[serverName];
  } catch (e) {
    setStatus(e.message, 'error');
    return;
  }
  if (!s) {
    setStatus(`MCP server "${serverName}" is missing from the MCP JSON.`, 'error');
    return;
  }

  btn.textContent = 'Testing...';
  btn.className = 'btn-test testing';
  try {
    const resp = await fetch('/api/config/test-mcp', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        command: s.command, args: s.args, env: s.env, cwd: s.cwd, timeoutSecs: s.timeoutSecs,
      }),
    });
    const data = await resp.json();
    if (data.ok) {
      btn.textContent = `✓ ${data.tools} tools`;
      btn.className = 'btn-test test-ok';
    } else {
      btn.textContent = '✗ Failed';
      btn.title = data.error || 'Connection failed';
      btn.className = 'btn-test test-fail';
    }
  } catch (e) {
    btn.textContent = '✗ Error';
    btn.title = e.message;
    btn.className = 'btn-test test-fail';
  }
  setTimeout(() => { btn.textContent = 'Test'; btn.className = 'btn-test'; btn.title = ''; }, 4000);
}

// ── Helpers ──

function row(label, input) {
  return `<div class="settings-row"><label>${label}</label>${input}</div>`;
}

function inputNum(id, value, placeholder) {
  const v = value != null ? escHtml(String(value)) : '';
  return `<input type="number" id="${id}" value="${v}" placeholder="${placeholder || ''}">`;
}

function inputText(id, value, placeholder) {
  return `<input type="text" id="${id}" value="${escHtml(value || '')}" placeholder="${escHtml(placeholder || '')}">`;
}

function inputPassword(id, value) {
  return `<input type="password" id="${id}" value="${escHtml(value || '')}">`;
}

function modelSelect(id, value, options) {
  let html = `<select id="${id}"><option value="">-- none --</option>`;
  for (const opt of options) {
    const selected = opt === value ? ' selected' : '';
    html += `<option value="${escHtml(opt)}"${selected}>${escHtml(opt)}</option>`;
  }
  // If value exists but not in options, add it
  if (value && !options.includes(value)) {
    html += `<option value="${escHtml(value)}" selected>${escHtml(value)} (custom)</option>`;
  }
  html += `</select>`;
  return html;
}

function buildModelOptions(providers) {
  const options = [];
  for (const [name, p] of Object.entries(providers)) {
    for (const m of (p.models || [])) {
      if (!m || typeof m.id !== 'string' || m.id.trim() === '') continue;
      options.push(`${name}/${m.id}`);
    }
  }
  return options.sort();
}

function validateProviderName(name, existingProviders = null) {
  const trimmed = name.trim();
  if (!trimmed) {
    throw new Error('Provider name cannot be empty.');
  }
  if (trimmed.includes('/')) {
    throw new Error("Provider name cannot contain '/'.");
  }
  if (/\s/.test(trimmed)) {
    throw new Error('Provider name cannot contain whitespace.');
  }
  if (!/^[A-Za-z0-9._-]+$/.test(trimmed)) {
    throw new Error("Provider name may only contain letters, numbers, '.', '-' or '_'.");
  }
  if (existingProviders && Object.prototype.hasOwnProperty.call(existingProviders, trimmed)) {
    throw new Error(`Provider name '${trimmed}' already exists.`);
  }
  return trimmed;
}

function bindModelsTabInteractions(container) {
  container.querySelectorAll('[data-prov-field], [data-model-field]').forEach(el => {
    el.addEventListener('input', () => {
      draftDirtyState.modelsForm = true;
    });
    el.addEventListener('change', () => {
      draftDirtyState.modelsForm = true;
    });
  });
  const editor = container.querySelector('#models-json-editor');
  if (editor) {
    editor.addEventListener('input', () => {
      draftDirtyState.modelsJson = true;
    });
  }
  // Test buttons
  bindProviderTestButtons(container);
  // Delete provider
  container.querySelectorAll('[data-delete-provider]').forEach(btn => {
    btn.addEventListener('click', () => {
      try {
        collectModelsFromForms();
      } catch (e) {
        setStatus(e.message, 'error');
        return;
      }
      const name = btn.dataset.deleteProvider;
      if (currentConfig.models?.providers) delete currentConfig.models.providers[name];
      renderModelsTab();
      syncAgentModelDraftFromInputs();
      renderAgentsTab();
    });
  });
  // Add provider
  const addBtn = container.querySelector('#add-provider-btn');
  if (addBtn) {
    addBtn.addEventListener('click', () => {
      try {
        collectModelsFromForms();
      } catch (e) {
        setStatus(e.message, 'error');
        return;
      }
      const name = prompt('Enter provider name (e.g. openai, anthropic):');
      if (!name) return;
      if (!currentConfig.models) currentConfig.models = {};
      if (!currentConfig.models.providers) currentConfig.models.providers = {};

      let trimmed;
      try {
        trimmed = validateProviderName(name, currentConfig.models.providers);
      } catch (e) {
        setStatus(e.message, 'error');
        return;
      }
      currentConfig.models.providers[trimmed] = {
        api: 'openai-completions',
        baseUrl: '',
        apiKey: '',
        models: [],
      };
      renderModelsTab();
    });
  }
  // Add model to provider
  container.querySelectorAll('[data-add-model]').forEach(btn => {
    btn.addEventListener('click', () => {
      const provName = btn.dataset.addModel;
      try {
        collectModelsFromForms();
      } catch (e) {
        setStatus(e.message, 'error');
        return;
      }
      if (!currentConfig.models?.providers?.[provName]) return;
      const models = [...(currentConfig.models.providers[provName].models || [])];
      models.push({ id: '', reasoning: false });
      currentConfig.models.providers[provName].models = models;
      renderModelsTab();
    });
  });
  // Delete model
  container.querySelectorAll('[data-delete-model]').forEach(btn => {
    btn.addEventListener('click', () => {
      const provName = btn.dataset.deleteModel;
      const idx = parseInt(btn.dataset.modelIdx, 10);
      try {
        collectModelsFromForms();
      } catch (e) {
        setStatus(e.message, 'error');
        return;
      }
      const models = currentConfig.models?.providers?.[provName]?.models;
      if (models && idx >= 0 && idx < models.length) {
        models.splice(idx, 1);
        renderModelsTab();
        syncAgentModelDraftFromInputs();
        renderAgentsTab();
      }
    });
  });
  // Apply JSON button
  const applyBtn = container.querySelector('#models-json-apply');
  if (applyBtn) {
    applyBtn.addEventListener('click', () => {
      if (draftDirtyState.modelsForm) {
        setStatus('Models form has unapplied changes. Save or discard them before applying Raw JSON.', 'error');
        return;
      }
      try {
        const parsed = readModelsConfigFromEditor();
        if (parsed === undefined) delete currentConfig.models;
        else currentConfig.models = parsed;
        renderModelsTab();
        syncAgentModelDraftFromInputs();
        renderAgentsTab();
        setStatus('Applied Models JSON', 'success');
      } catch (e) {
        setStatus(e.message, 'error');
      }
    });
  }
}

function bindProviderTestButtons(container) {
  container.querySelectorAll('[data-test-provider]').forEach(btn => {
    btn.addEventListener('click', () => testProvider(btn, btn.dataset.testProvider));
  });
}

function getModelsProvidersForUi() {
  if (draftDirtyState.modelsJson) {
    return currentConfig.models?.providers || {};
  }
  // Collect from forms if they exist
  if (document.querySelector('#models-provider-list .provider-card')) {
    try {
      collectModelsFromForms();
    } catch (e) {
      setStatus(e.message, 'error');
      return currentConfig.models?.providers || {};
    }
  }
  return currentConfig.models?.providers || {};
}

function syncAgentModelDraftFromInputs() {
  if (!currentConfig) return;
  const hasRenderedInputs = [
    'cfg-agent-primary',
    'cfg-agent-fast',
    'cfg-agent-sub-agent',
    'cfg-agent-memory',
    'cfg-agent-reflection',
    'cfg-agent-context',
  ].some(id => document.getElementById(id));
  if (!hasRenderedInputs) return;

  if (!currentConfig.agents) currentConfig.agents = {};
  if (!currentConfig.agents.defaults) currentConfig.agents.defaults = {};
  if (!currentConfig.agents.defaults.model) currentConfig.agents.defaults.model = {};

  const model = currentConfig.agents.defaults.model;
  model.primary = strVal('cfg-agent-primary') || undefined;
  model.fast = strVal('cfg-agent-fast') || undefined;
  model['sub-agent'] = strVal('cfg-agent-sub-agent') || undefined;
  model.memory = strVal('cfg-agent-memory') || undefined;
  model.reflection = strVal('cfg-agent-reflection') || undefined;
  model.context = strVal('cfg-agent-context') || undefined;
}

function currentAgentModelRefs() {
  const draftRefs = [
    strVal('cfg-agent-primary'),
    strVal('cfg-agent-fast'),
    strVal('cfg-agent-sub-agent'),
    strVal('cfg-agent-memory'),
    strVal('cfg-agent-reflection'),
    strVal('cfg-agent-context'),
  ].filter(Boolean);
  if (draftRefs.length > 0) {
    return draftRefs;
  }

  const model = currentConfig?.agents?.defaults?.model || {};
  return [
    model.primary,
    model.fast,
    model['sub-agent'],
    model.memory,
    model.reflection,
    model.context,
  ].filter(Boolean);
}

function preferredTestModelId(providerName, models) {
  for (const modelRef of currentAgentModelRefs()) {
    const [refProvider, refModelId] = modelRef.split('/');
    if (refProvider === providerName && models.some(model => model.id === refModelId)) {
      return refModelId;
    }
  }
  return models[0]?.id || '';
}

function resolveProviderTestModelId(btn, providerName, models) {
  const selectedModelId = btn.closest('.provider-card')
    ?.querySelector('[data-provider-model-select]')
    ?.value
    ?.trim();
  if (selectedModelId && models.some(model => model.id === selectedModelId)) {
    return selectedModelId;
  }
  return preferredTestModelId(providerName, models);
}

function numVal(id) {
  const el = document.getElementById(id);
  if (!el || el.value === '') return undefined;
  const n = parseInt(el.value, 10);
  return isNaN(n) ? undefined : n;
}

function strVal(id) {
  const el = document.getElementById(id);
  return el ? el.value.trim() : '';
}

function triState(id, value) {
  const val = value === true ? 'true' : value === false ? 'false' : '';
  return `<select id="${id}">
    <option value=""${val === '' ? ' selected' : ''}>Default</option>
    <option value="true"${val === 'true' ? ' selected' : ''}>Enabled</option>
    <option value="false"${val === 'false' ? ' selected' : ''}>Disabled</option>
  </select>`;
}

function triVal(id) {
  const el = document.getElementById(id);
  if (!el || el.value === '') return undefined;
  return el.value === 'true';
}

function readModelsConfigFromEditor() {
  const editor = document.getElementById('models-json-editor');
  if (!editor) return currentConfig.models;

  const text = editor.value.trim();
  if (!text) {
    editor.classList.remove('has-error');
    const errEl = document.getElementById('models-json-error');
    if (errEl) errEl.textContent = '';
    return undefined;
  }

  try {
    const parsed = JSON.parse(text);
    validateModelsConfigDraftShape(parsed);
    editor.classList.remove('has-error');
    const errEl = document.getElementById('models-json-error');
    if (errEl) errEl.textContent = '';
    return parsed;
  } catch (e) {
    editor.classList.add('has-error');
    const errEl = document.getElementById('models-json-error');
    if (errEl) errEl.textContent = e.message;
    throw new Error('Models JSON is invalid: ' + e.message);
  }
}

function readMcpConfigFromEditor() {
  const editor = document.getElementById('mcp-json-editor');
  if (!editor) return currentConfig.mcpServers;

  const text = editor.value.trim();
  if (!text || text === '{}') {
    editor.classList.remove('has-error');
    const errEl = document.getElementById('mcp-json-error');
    if (errEl) errEl.textContent = '';
    return undefined;
  }

  try {
    const parsed = JSON.parse(text);
    validateMcpConfigDraftShape(parsed);
    editor.classList.remove('has-error');
    const errEl = document.getElementById('mcp-json-error');
    if (errEl) errEl.textContent = '';
    return parsed;
  } catch (e) {
    editor.classList.add('has-error');
    const errEl = document.getElementById('mcp-json-error');
    if (errEl) errEl.textContent = e.message;
    throw new Error('MCP JSON is invalid: ' + e.message);
  }
}

async function saveRawConfig(editor) {
  const text = editor.value.trim();
  if (!text) { setStatus('Config is empty', 'error'); return; }
  let parsed;
  try { parsed = JSON.parse(text); } catch (e) {
    editor.classList.add('has-error');
    const errEl = editor.parentElement?.querySelector('.json-editor-error');
    if (errEl) errEl.textContent = e.message;
    setStatus('Fix JSON syntax errors first', 'error');
    return;
  }
  editor.classList.remove('has-error');
  setStatus('Saving...');
  try {
    const resp = await fetch('/api/config', {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ config: parsed }),
    });
    const data = await resp.json();
    if (!resp.ok || data.error) {
      setStatus(data.error || 'Save failed', 'error');
      return;
    }
    setStatus('Saved! Reloading...', 'success');
    setTimeout(loadConfig, 600);
  } catch (e) {
    setStatus(`Save failed: ${e.message}`, 'error');
  }
}
