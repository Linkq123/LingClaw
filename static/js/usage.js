import { escHtml, formatTokenCount } from './utils.js';

let cachedUsageData = null;
let dailyRange = 7;
let providerRange = 7;
const ROLE_ORDER = ['Primary', 'Fast', 'Sub-Agent', 'Memory', 'Reflection', 'Context'];

export function openUsagePage() {
  const page = document.getElementById('usage-page');
  if (page) page.hidden = false;
  loadUsage();
}

export function closeUsagePage() {
  const page = document.getElementById('usage-page');
  if (page) page.hidden = true;
}

export function initUsageListeners() {
  bindRangePicker('daily-range-picker', range => {
    dailyRange = range;
    if (cachedUsageData) renderDailyChart(cachedUsageData, dailyRange);
  });
  bindRangePicker('provider-range-picker', range => {
    providerRange = range;
    if (cachedUsageData) renderProviderChart(cachedUsageData, providerRange);
  });
}

function bindRangePicker(containerId, onChange) {
  const container = document.getElementById(containerId);
  if (!container) return;
  container.addEventListener('click', e => {
    const btn = e.target.closest('.range-btn');
    if (!btn) return;
    container.querySelectorAll('.range-btn').forEach(b => b.classList.remove('active'));
    btn.classList.add('active');
    onChange(parseInt(btn.dataset.range, 10));
  });
}

async function loadUsage() {
  try {
    const resp = await fetch('/api/usage');
    if (!resp.ok) {
      console.warn(`Usage fetch failed: HTTP ${resp.status}`);
      return;
    }
    cachedUsageData = await resp.json();
    renderSummary(cachedUsageData);
    renderRoleBreakdown(cachedUsageData);
    renderDailyChart(cachedUsageData, dailyRange);
    renderProviderChart(cachedUsageData, providerRange);
  } catch (e) {
    console.error('Failed to load usage data:', e);
  }
}

function renderSummary(data) {
  const container = document.getElementById('usage-summary');
  if (!container) return;
  const total = (data.total_input || 0) + (data.total_output || 0);
  const inputSource = escHtml(data.input_source || 'estimated');
  const outputSource = escHtml(data.output_source || 'estimated');
  const sourceScope = data.source_scope || 'latest_update';
  const sourceNote = sourceScope === 'latest_update'
    ? `Latest recorded token source: input ${inputSource}, output ${outputSource}. Cumulative totals may still include earlier estimates.`
    : `Token source: input ${inputSource}, output ${outputSource}.`;
  container.innerHTML = `
    <div class="usage-stat-card">
      <div class="usage-stat-value">${formatTokenCount((data.daily_input || 0) + (data.daily_output || 0))}</div>
      <div class="usage-stat-label">Today Total</div>
    </div>
    <div class="usage-stat-card">
      <div class="usage-stat-value">${formatTokenCount(data.daily_input || 0)}</div>
      <div class="usage-stat-label">Today Input</div>
    </div>
    <div class="usage-stat-card">
      <div class="usage-stat-value">${formatTokenCount(data.daily_output || 0)}</div>
      <div class="usage-stat-label">Today Output</div>
    </div>
    <div class="usage-stat-card">
      <div class="usage-stat-value">${formatTokenCount(total)}</div>
      <div class="usage-stat-label">All-Time Total</div>
    </div>
    <div class="usage-stat-card">
      <div class="usage-stat-value">${formatTokenCount(data.total_input)}</div>
      <div class="usage-stat-label">All-Time Input</div>
    </div>
    <div class="usage-stat-card">
      <div class="usage-stat-value">${formatTokenCount(data.total_output)}</div>
      <div class="usage-stat-label">All-Time Output</div>
    </div>
    <div class="usage-summary-note">${sourceNote}</div>`;
}

function normalizeUsagePair(pair) {
  return {
    input: Array.isArray(pair) ? (pair[0] || 0) : 0,
    output: Array.isArray(pair) ? (pair[1] || 0) : 0,
  };
}

function renderRoleBreakdown(data) {
  const container = document.getElementById('usage-role-breakdown');
  if (!container) return;

  const dailyRoles = data.daily_roles || {};
  const totalRoles = data.total_roles || {};
  const names = Array.from(new Set([
    ...ROLE_ORDER,
    ...Object.keys(dailyRoles),
    ...Object.keys(totalRoles),
  ])).filter(name => {
    if (!name) return false;
    const today = normalizeUsagePair(dailyRoles[name]);
    const total = normalizeUsagePair(totalRoles[name]);
    return today.input + today.output + total.input + total.output > 0;
  });

  if (names.length === 0) {
    container.innerHTML = '<p class="usage-empty-note">No role usage data yet.</p>';
    return;
  }

  container.innerHTML = names.map(name => {
    const today = normalizeUsagePair(dailyRoles[name]);
    const total = normalizeUsagePair(totalRoles[name]);
    return `
      <article class="usage-role-card">
        <div class="usage-role-header">
          <h3>${escHtml(name)}</h3>
          <span class="usage-role-chip">${formatTokenCount(total.input + total.output)}</span>
        </div>
        <div class="usage-role-metrics">
          <div>
            <span class="usage-role-kicker">Today</span>
            <strong>${formatTokenCount(today.input + today.output)}</strong>
            <span>${formatTokenCount(today.input)} in / ${formatTokenCount(today.output)} out</span>
          </div>
          <div>
            <span class="usage-role-kicker">All-Time</span>
            <strong>${formatTokenCount(total.input + total.output)}</strong>
            <span>${formatTokenCount(total.input)} in / ${formatTokenCount(total.output)} out</span>
          </div>
        </div>
      </article>`;
  }).join('');
}

// ── Daily line chart ──

function localDateStr(d) {
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, '0');
  const day = String(d.getDate()).padStart(2, '0');
  return `${y}-${m}-${day}`;
}

function buildDailyTimeline(data, days) {
  const history = data.usage_history || [];
  const today = new Date();
  const timeline = [];
  for (let i = days - 1; i >= 0; i--) {
    const d = new Date(today);
    d.setDate(d.getDate() - i);
    const dateStr = localDateStr(d);
    const snap = history.find(h => h.date === dateStr);
    if (i === 0) {
      // Today: use live daily counters
      timeline.push({
        date: dateStr,
        label: formatDateLabel(d),
        input: data.daily_input || 0,
        output: data.daily_output || 0,
      });
    } else if (snap) {
      timeline.push({
        date: dateStr,
        label: formatDateLabel(d),
        input: snap.input || 0,
        output: snap.output || 0,
      });
    } else {
      timeline.push({ date: dateStr, label: formatDateLabel(d), input: 0, output: 0 });
    }
  }
  return timeline;
}

function formatDateLabel(d) {
  return `${d.getMonth() + 1}/${d.getDate()}`;
}

function renderDailyChart(data, days) {
  const canvas = document.getElementById('daily-chart');
  if (!canvas) return;
  const parent = canvas.parentElement;
  if (!parent) return;
  const ctx = canvas.getContext('2d');
  if (!ctx) return;
  const rect = parent.getBoundingClientRect();
  if (rect.width <= 32 || rect.height <= 32) return;

  const dpr = window.devicePixelRatio || 1;
  canvas.width = (rect.width - 32) * dpr;
  canvas.height = (rect.height - 32) * dpr;
  canvas.style.width = (rect.width - 32) + 'px';
  canvas.style.height = (rect.height - 32) + 'px';
  ctx.scale(dpr, dpr);

  const w = rect.width - 32;
  const h = rect.height - 32;

  const timeline = buildDailyTimeline(data, days);
  drawLineChart(ctx, w, h, timeline);
}

function drawLineChart(ctx, w, h, timeline) {
  ctx.clearRect(0, 0, w, h);
  const padding = { top: 24, right: 20, bottom: 44, left: 60 };
  const chartW = w - padding.left - padding.right;
  const chartH = h - padding.top - padding.bottom;

  const isDark = window.matchMedia('(prefers-color-scheme: dark)').matches;
  const textColor = isDark ? '#b0b5c0' : '#7e8699';
  const gridColor = isDark ? 'rgba(255,255,255,.06)' : 'rgba(0,0,0,.06)';

  let maxVal = 0;
  for (const p of timeline) {
    const total = p.input + p.output;
    if (total > maxVal) maxVal = total;
  }
  if (maxVal === 0) maxVal = 100;

  // Grid lines
  const gridLines = 4;
  for (let i = 0; i <= gridLines; i++) {
    const y = padding.top + (chartH / gridLines) * i;
    ctx.strokeStyle = gridColor;
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(padding.left, y);
    ctx.lineTo(padding.left + chartW, y);
    ctx.stroke();

    const val = maxVal - (maxVal / gridLines) * i;
    ctx.fillStyle = textColor;
    ctx.font = '11px system-ui';
    ctx.textAlign = 'right';
    ctx.fillText(formatTokenCount(Math.round(val)), padding.left - 8, y + 4);
  }

  const n = timeline.length;
  if (n === 0) return;
  const stepX = n > 1 ? chartW / (n - 1) : 0;

  const getY = val => padding.top + chartH - (val / maxVal) * chartH;

  // Draw stacked area (output below, input on top)
  // Input line
  drawDataLine(ctx, timeline, stepX, padding, getY, d => d.input, '#2d8bcf', 'rgba(45,139,207,.12)');
  // Output line
  drawDataLine(ctx, timeline, stepX, padding, getY, d => d.output, '#c06b9e', 'rgba(192,107,158,.12)');

  // Data points and X labels
  for (let i = 0; i < n; i++) {
    const x = padding.left + i * stepX;

    // X label
    ctx.fillStyle = textColor;
    ctx.font = '10px system-ui';
    ctx.textAlign = 'center';
    // Show every label if <=7, or every other if more
    if (n <= 7 || i % 2 === 0 || i === n - 1) {
      ctx.fillText(timeline[i].label, x, h - padding.bottom + 16);
    }
  }

  // Legend
  ctx.font = '11px system-ui';
  const legendY = h - 6;
  ctx.fillStyle = '#2d8bcf';
  ctx.fillRect(padding.left, legendY - 6, 10, 3);
  ctx.fillStyle = textColor;
  ctx.textAlign = 'left';
  ctx.fillText('Input', padding.left + 14, legendY);
  ctx.fillStyle = '#c06b9e';
  ctx.fillRect(padding.left + 60, legendY - 6, 10, 3);
  ctx.fillStyle = textColor;
  ctx.fillText('Output', padding.left + 74, legendY);
}

function drawDataLine(ctx, timeline, stepX, padding, getY, accessor, color, fillColor) {
  const n = timeline.length;
  if (n === 0) return;

  // Area fill
  ctx.beginPath();
  for (let i = 0; i < n; i++) {
    const x = padding.left + i * stepX;
    const y = getY(accessor(timeline[i]));
    if (i === 0) ctx.moveTo(x, y);
    else ctx.lineTo(x, y);
  }
  ctx.lineTo(padding.left + (n - 1) * stepX, getY(0));
  ctx.lineTo(padding.left, getY(0));
  ctx.closePath();
  ctx.fillStyle = fillColor;
  ctx.fill();

  // Line
  ctx.beginPath();
  for (let i = 0; i < n; i++) {
    const x = padding.left + i * stepX;
    const y = getY(accessor(timeline[i]));
    if (i === 0) ctx.moveTo(x, y);
    else ctx.lineTo(x, y);
  }
  ctx.strokeStyle = color;
  ctx.lineWidth = 2;
  ctx.stroke();

  // Dots
  for (let i = 0; i < n; i++) {
    const val = accessor(timeline[i]);
    if (val === 0) continue;
    const x = padding.left + i * stepX;
    const y = getY(val);
    ctx.beginPath();
    ctx.arc(x, y, 3, 0, Math.PI * 2);
    ctx.fillStyle = color;
    ctx.fill();
  }
}

// ── Provider bar chart ──

function buildProviderTotals(data, days) {
  const history = data.usage_history || [];
  const today = new Date();
  const totals = {};

  for (let i = days - 1; i >= 0; i--) {
    const d = new Date(today);
    d.setDate(d.getDate() - i);
    const dateStr = localDateStr(d);

    if (i === 0) {
      // Today: use live daily_providers
      const dp = data.daily_providers || {};
      for (const [name, pair] of Object.entries(dp)) {
        if (!totals[name]) totals[name] = { input: 0, output: 0 };
        totals[name].input += pair[0] || 0;
        totals[name].output += pair[1] || 0;
      }
    } else {
      const snap = history.find(h => h.date === dateStr);
      if (snap && snap.providers) {
        for (const [name, pair] of Object.entries(snap.providers)) {
          if (!totals[name]) totals[name] = { input: 0, output: 0 };
          totals[name].input += pair[0] || 0;
          totals[name].output += pair[1] || 0;
        }
      }
    }
  }

  return totals;
}

function renderProviderChart(data, days) {
  const canvas = document.getElementById('provider-chart');
  if (!canvas) return;
  const parent = canvas.parentElement;
  if (!parent) return;
  const ctx = canvas.getContext('2d');
  if (!ctx) return;
  const rect = parent.getBoundingClientRect();
  if (rect.width <= 32 || rect.height <= 32) return;

  const dpr = window.devicePixelRatio || 1;
  canvas.width = (rect.width - 32) * dpr;
  canvas.height = (rect.height - 32) * dpr;
  canvas.style.width = (rect.width - 32) + 'px';
  canvas.style.height = (rect.height - 32) + 'px';
  ctx.scale(dpr, dpr);

  const w = rect.width - 32;
  const h = rect.height - 32;

  const providerTotals = buildProviderTotals(data, days);
  const providers = Object.keys(providerTotals).sort();

  if (providers.length === 0) {
    ctx.clearRect(0, 0, w, h);
    ctx.fillStyle = window.matchMedia('(prefers-color-scheme: dark)').matches ? '#b0b5c0' : '#7e8699';
    ctx.font = '13px system-ui';
    ctx.textAlign = 'center';
    ctx.fillText('No per-provider data available yet', w / 2, h / 2);
    return;
  }

  drawGroupedBarChart(ctx, w, h, providers, providerTotals);
}

const PROVIDER_COLORS = ['#2d8bcf', '#c06b9e', '#6c63ff', '#e8a838', '#44c4a1', '#d65c5c', '#8b7ec8', '#3ea8b5'];

function truncateProviderLabel(name, maxChars) {
  if (name.length <= maxChars) return name;
  if (maxChars <= 1) return name.slice(0, maxChars);
  return name.slice(0, maxChars - 1) + '…';
}

function drawGroupedBarChart(ctx, w, h, providers, totals) {
  ctx.clearRect(0, 0, w, h);
  const padding = { top: 24, right: 20, bottom: 48, left: 60 };
  const chartW = w - padding.left - padding.right;
  const chartH = h - padding.top - padding.bottom;

  const isDark = window.matchMedia('(prefers-color-scheme: dark)').matches;
  const textColor = isDark ? '#b0b5c0' : '#7e8699';
  const gridColor = isDark ? 'rgba(255,255,255,.06)' : 'rgba(0,0,0,.06)';

  // Each provider gets two bars (input + output)
  const groupCount = providers.length;
  const barsPerGroup = 2;
  const groupGap = 24;
  const barGap = 4;
  const availW = chartW - groupGap * (groupCount + 1);
  const barWidth = Math.min(32, Math.max(12, availW / (groupCount * barsPerGroup)));
  const groupWidth = barWidth * barsPerGroup + barGap;

  let maxVal = 0;
  for (const t of Object.values(totals)) {
    maxVal = Math.max(maxVal, t.input, t.output);
  }
  if (maxVal === 0) maxVal = 100;

  // Grid
  const gridLines = 4;
  for (let i = 0; i <= gridLines; i++) {
    const y = padding.top + (chartH / gridLines) * i;
    ctx.strokeStyle = gridColor;
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(padding.left, y);
    ctx.lineTo(padding.left + chartW, y);
    ctx.stroke();

    const val = maxVal - (maxVal / gridLines) * i;
    ctx.fillStyle = textColor;
    ctx.font = '11px system-ui';
    ctx.textAlign = 'right';
    ctx.fillText(formatTokenCount(Math.round(val)), padding.left - 8, y + 4);
  }

  const totalWidth = groupWidth * groupCount + groupGap * (groupCount - 1);
  const startX = padding.left + (chartW - totalWidth) / 2;

  providers.forEach((name, gi) => {
    const t = totals[name];
    const gx = startX + gi * (groupWidth + groupGap);
    const color = PROVIDER_COLORS[gi % PROVIDER_COLORS.length];
    const label = truncateProviderLabel(name, Math.max(4, Math.floor(groupWidth / 7)));

    // Input bar
    drawRoundedBar(ctx, gx, padding.top + chartH, barWidth, (t.input / maxVal) * chartH, color, 0.85);
    // Output bar
    drawRoundedBar(ctx, gx + barWidth + barGap, padding.top + chartH, barWidth, (t.output / maxVal) * chartH, color, 0.5);

    // Value labels
    ctx.fillStyle = textColor;
    ctx.font = '10px system-ui';
    ctx.textAlign = 'center';
    if (t.input > 0) {
      ctx.fillText(formatTokenCount(t.input), gx + barWidth / 2, padding.top + chartH - (t.input / maxVal) * chartH - 6);
    }
    if (t.output > 0) {
      ctx.fillText(formatTokenCount(t.output), gx + barWidth + barGap + barWidth / 2, padding.top + chartH - (t.output / maxVal) * chartH - 6);
    }

    // Provider label
    ctx.fillStyle = textColor;
    ctx.font = '11px system-ui';
    ctx.textAlign = 'center';
    ctx.fillText(label, gx + groupWidth / 2, h - padding.bottom + 16);
  });

  // Legend
  ctx.font = '11px system-ui';
  const legendY = h - 6;
  ctx.fillStyle = PROVIDER_COLORS[0];
  ctx.globalAlpha = 0.85;
  ctx.fillRect(padding.left, legendY - 6, 10, 8);
  ctx.globalAlpha = 1;
  ctx.fillStyle = textColor;
  ctx.textAlign = 'left';
  ctx.fillText('Input', padding.left + 14, legendY);
  ctx.fillStyle = PROVIDER_COLORS[0];
  ctx.globalAlpha = 0.5;
  ctx.fillRect(padding.left + 56, legendY - 6, 10, 8);
  ctx.globalAlpha = 1;
  ctx.fillStyle = textColor;
  ctx.fillText('Output', padding.left + 70, legendY);
}

function drawRoundedBar(ctx, x, bottom, width, height, color, alpha) {
  if (height < 1) return;
  const y = bottom - height;
  const radius = Math.min(3, width / 2, height / 2);
  ctx.globalAlpha = alpha;
  ctx.fillStyle = color;
  ctx.beginPath();
  ctx.moveTo(x, y + radius);
  ctx.quadraticCurveTo(x, y, x + radius, y);
  ctx.lineTo(x + width - radius, y);
  ctx.quadraticCurveTo(x + width, y, x + width, y + radius);
  ctx.lineTo(x + width, bottom);
  ctx.lineTo(x, bottom);
  ctx.closePath();
  ctx.fill();
  ctx.globalAlpha = 1;
}
