import { escHtml, formatTokenCount } from './utils.js';

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
  // Reserved for future interactive controls.
}

async function loadUsage() {
  try {
    const resp = await fetch('/api/usage');
    if (!resp.ok) {
      console.warn(`Usage fetch failed: HTTP ${resp.status}`);
      return;
    }
    const data = await resp.json();
    renderSummary(data);
    renderDailyChart(data);
    renderProviderChart(data);
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
      <div class="usage-stat-value">${formatTokenCount(data.daily_input + data.daily_output)}</div>
      <div class="usage-stat-label">Today Total</div>
    </div>
    <div class="usage-stat-card">
      <div class="usage-stat-value">${formatTokenCount(data.daily_input)}</div>
      <div class="usage-stat-label">Today Input</div>
    </div>
    <div class="usage-stat-card">
      <div class="usage-stat-value">${formatTokenCount(data.daily_output)}</div>
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

function renderDailyChart(data) {
  const canvas = document.getElementById('daily-chart');
  if (!canvas) return;
  const parent = canvas.parentElement;
  if (!parent) return;
  const ctx = canvas.getContext('2d');
  if (!ctx) return;
  const rect = parent.getBoundingClientRect();
  if (rect.width <= 32 || rect.height <= 32) return;
  canvas.width = rect.width - 32;
  canvas.height = rect.height - 32;

  drawBarChart(ctx, canvas.width, canvas.height, ['Input', 'Output'], [
    { value: data.daily_input || 0, color: '#2d8bcf' },
    { value: data.daily_output || 0, color: '#c06b9e' },
  ]);
}

function renderProviderChart(data) {
  const canvas = document.getElementById('provider-chart');
  if (!canvas) return;
  const parent = canvas.parentElement;
  if (!parent) return;
  const ctx = canvas.getContext('2d');
  if (!ctx) return;
  const rect = parent.getBoundingClientRect();
  if (rect.width <= 32 || rect.height <= 32) return;
  canvas.width = rect.width - 32;
  canvas.height = rect.height - 32;

  // All-time token breakdown (aggregate, not per-provider)
  const total = (data.total_input || 0) + (data.total_output || 0);
  drawBarChart(ctx, canvas.width, canvas.height, ['Input', 'Output', 'Total'], [
    { value: data.total_input || 0, color: '#2d8bcf' },
    { value: data.total_output || 0, color: '#c06b9e' },
    { value: total, color: '#6c63ff' },
  ]);
}

// ── Lightweight canvas chart ──

function drawBarChart(ctx, w, h, labels, bars) {
  ctx.clearRect(0, 0, w, h);
  const padding = { top: 20, right: 20, bottom: 40, left: 60 };
  const chartW = w - padding.left - padding.right;
  const chartH = h - padding.top - padding.bottom;

  let maxVal = 0;
  for (const b of bars) {
    if (b.value > maxVal) maxVal = b.value;
  }
  if (maxVal === 0) maxVal = 100;

  const isDark = window.matchMedia('(prefers-color-scheme: dark)').matches;
  const textColor = isDark ? '#b0b5c0' : '#7e8699';
  const gridColor = isDark ? 'rgba(255,255,255,.06)' : 'rgba(0,0,0,.06)';

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

  // Bars
  const barCount = bars.length;
  const gap = 16;
  const barWidth = Math.min(60, (chartW - gap * (barCount + 1)) / barCount);
  const totalWidth = barWidth * barCount + gap * (barCount - 1);
  const startX = padding.left + (chartW - totalWidth) / 2;

  bars.forEach((bar, i) => {
    const x = startX + i * (barWidth + gap);
    const barH = (bar.value / maxVal) * chartH;
    const y = padding.top + chartH - barH;

    // Bar with rounded top
    ctx.fillStyle = bar.color;
    ctx.beginPath();
    const radius = Math.min(4, barWidth / 2);
    ctx.moveTo(x, y + radius);
    ctx.quadraticCurveTo(x, y, x + radius, y);
    ctx.lineTo(x + barWidth - radius, y);
    ctx.quadraticCurveTo(x + barWidth, y, x + barWidth, y + radius);
    ctx.lineTo(x + barWidth, padding.top + chartH);
    ctx.lineTo(x, padding.top + chartH);
    ctx.closePath();
    ctx.fill();

    // Value on top
    ctx.fillStyle = textColor;
    ctx.font = '11px system-ui';
    ctx.textAlign = 'center';
    ctx.fillText(formatTokenCount(bar.value), x + barWidth / 2, y - 6);

    // Label
    ctx.fillText(labels[i], x + barWidth / 2, h - padding.bottom + 18);
  });
}
