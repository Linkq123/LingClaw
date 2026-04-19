import { escHtml } from './utils.js';

let currentConfig = null;
let currentConfigPath = '';

const SETTINGS_TAB_CONTENTS = `
  <div class="tab-content active" id="tab-general"></div>
  <div class="tab-content" id="tab-models"></div>
  <div class="tab-content" id="tab-agents"></div>
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
  const providers = getModelsProvidersForUi();

  let html = `<div id="models-provider-list">${renderModelsProviderCards(providers)}</div>`;
  html += `<p style="font-size:11px;color:var(--dim);margin-top:8px">Edit provider settings in the raw JSON below. Provider tests use the current JSON editor contents.</p>
    <div class="json-editor-wrap">
      <textarea class="json-editor" id="models-json-editor" spellcheck="false">${escHtml(JSON.stringify(currentConfig.models || { providers: {} }, null, 2))}</textarea>
      <div class="json-editor-error" id="models-json-error"></div>
    </div>`;
  container.innerHTML = html;

  bindModelsTabInteractions(container);
}

function collectModelsTab() {
  currentConfig.models = readModelsConfigFromEditor();
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
  for (const [key, val] of Object.entries(m)) {
    if (!val) continue;
    if (val.includes('/')) {
      const [provName] = val.split('/');
      if (!providers[provName]) {
        throw new Error(`Agent model "${key}" references unknown provider "${provName}". Add it in Models tab first.`);
      }
    }
  }
}

function renderMcpTab() {
  const container = document.getElementById('tab-mcp');
  if (!container) return;
  const servers = currentConfig.mcpServers || {};
  const names = Object.keys(servers).sort();

  let html = '';
  if (names.length > 0) {
    for (const name of names) {
      const s = servers[name];
      html += `
        <div class="provider-card">
          <div class="provider-card-header">
            <span class="provider-card-name">${escHtml(name)}</span>
            <div style="display:flex;gap:6px;align-items:center">
              <span style="font-size:11px;color:${s.enabled !== false ? 'var(--accent-success)' : 'var(--dim)'}">${s.enabled !== false ? 'Enabled' : 'Disabled'}</span>
              <button class="btn-test" data-test-mcp="${escHtml(name)}">Test</button>
            </div>
          </div>
          <div style="font-size:12px;color:var(--dim);line-height:1.6">
            Command: <code>${escHtml(s.command || '')} ${(s.args || []).map(a => escHtml(a)).join(' ')}</code>
          </div>
        </div>`;
    }
  }
  html += `<p style="font-size:12px;color:var(--dim);margin-top:8px">Edit MCP servers in the JSON below.</p>
    <div class="json-editor-wrap">
      <textarea class="json-editor" id="mcp-json-editor" spellcheck="false">${escHtml(JSON.stringify(currentConfig.mcpServers || {}, null, 2))}</textarea>
      <div class="json-editor-error" id="mcp-json-error"></div>
    </div>`;
  container.innerHTML = html;

  container.querySelectorAll('[data-test-mcp]').forEach(btn => {
    btn.addEventListener('click', () => testMcp(btn, btn.dataset.testMcp));
  });
}

function collectMcpTab() {
  currentConfig.mcpServers = readMcpConfigFromEditor();
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
  let p;
  try {
    const providers = readModelsConfigFromEditor()?.providers || {};
    p = providers[providerName];
  } catch (e) {
    setStatus(e.message, 'error');
    return;
  }
  if (!p) {
    setStatus(`Provider "${providerName}" is missing from the Models JSON.`, 'error');
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
  const v = value != null ? value : '';
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
      options.push(`${name}/${m.id}`);
    }
  }
  return options.sort();
}

function bindModelsTabInteractions(container) {
  const editor = container.querySelector('#models-json-editor');
  if (editor) {
    editor.addEventListener('input', handleModelsEditorInput);
  }
  bindProviderTestButtons(container);
}

function bindProviderTestButtons(container) {
  container.querySelectorAll('[data-test-provider]').forEach(btn => {
    btn.addEventListener('click', () => testProvider(btn, btn.dataset.testProvider));
  });
}

function handleModelsEditorInput() {
  const draftConfig = getModelsConfigDraft();
  if (draftConfig === null) return;

  updateModelsProviderList(draftConfig?.providers || {});
  syncAgentModelDraftFromInputs();
  renderAgentsTab();
}

function updateModelsProviderList(providers) {
  const list = document.getElementById('models-provider-list');
  if (!list) return;
  list.innerHTML = renderModelsProviderCards(providers);
  bindProviderTestButtons(list);
}

function renderModelsProviderCards(providers) {
  const names = Object.keys(providers).sort();
  if (names.length === 0) {
    return `<p style="color:var(--dim)">No providers configured. Add providers in the JSON config.</p>`;
  }

  let html = '';
  for (const name of names) {
    html += renderModelsProviderCard(name, providers[name]);
  }
  return html;
}

function renderModelsProviderCard(name, provider) {
  const models = provider.models || [];
  const modelList = models.map(model => model.id || model.name).join(', ');
  const selectedModelId = preferredTestModelId(name, models);
  const modelSelect = models.length > 0
    ? `<select data-provider-model-select="${escHtml(name)}" style="max-width:190px;padding:5px 8px">
        ${models.map(model => {
          const modelId = model.id || '';
          const selected = modelId === selectedModelId ? ' selected' : '';
          return `<option value="${escHtml(modelId)}"${selected}>${escHtml(modelId)}</option>`;
        }).join('')}
      </select>`
    : `<span style="font-size:11px;color:var(--dim)">No model</span>`;

  return `
    <div class="provider-card">
      <div class="provider-card-header">
        <span class="provider-card-name">${escHtml(name)}</span>
        <div style="display:flex;gap:6px;align-items:center;flex-wrap:wrap;justify-content:flex-end">
          <span style="font-size:11px;color:var(--dim)">${escHtml(provider.api || 'openai-completions')}</span>
          ${modelSelect}
          <button class="btn-test" data-test-provider="${escHtml(name)}">Test</button>
        </div>
      </div>
      <div style="font-size:12px;color:var(--dim);line-height:1.6">Base URL: ${escHtml(provider.baseUrl || 'not set')}</div>
      <div style="font-size:12px;color:var(--dim);line-height:1.6">API Key: ${provider.apiKey ? 'configured' : 'missing'}</div>
      <div class="provider-models-list">Models: ${escHtml(modelList) || '<em>none</em>'}</div>
    </div>`;
}

function getModelsConfigDraft() {
  const editor = document.getElementById('models-json-editor');
  if (!editor) return currentConfig.models;

  const text = editor.value.trim();
  if (!text) return undefined;

  try {
    return JSON.parse(text);
  } catch {
    return null;
  }
}

function getModelsProvidersForUi() {
  const draftConfig = getModelsConfigDraft();
  if (draftConfig === null) {
    return currentConfig.models?.providers || {};
  }
  return draftConfig?.providers || {};
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
