import React, { useState, useEffect, useRef, useCallback, useMemo } from 'react';
import type { UsageData } from '../types/config.js';

// ── Module-level bridge ───────────────────────────────────────────────────────

let _open: (() => void) | null = null;
let _close: (() => void) | null = null;
// See note on the same flag in SettingsPage.tsx.
let pendingOpen = false;

export function openUsagePage(): void {
  if (_open) _open();
  else pendingOpen = true;
}
export function closeUsagePage(): void {
  pendingOpen = false;
  _close?.();
}
// ── Format helpers ────────────────────────────────────────────────────────────

const ROLE_ORDER = ['Primary', 'Fast', 'Sub-Agent', 'Memory', 'Reflection', 'Context'];
const RANGE_OPTIONS: ReadonlyArray<{ value: number; label: string }> = [
  { value: 7, label: '7 days' },
  { value: 14, label: '14 days' },
  { value: 30, label: '30 days' },
];
const PROVIDER_COLORS = [
  '#2d8bcf',
  '#c06b9e',
  '#6c63ff',
  '#e8a838',
  '#44c4a1',
  '#d65c5c',
  '#8b7ec8',
  '#3ea8b5',
];

function formatTokenCount(n: number | undefined): string {
  if (n == null) return '0';
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(1).replace(/\.0$/, '') + 'M';
  if (n >= 1_000) return (n / 1_000).toFixed(1).replace(/\.0$/, '') + 'K';
  return String(n);
}

function normalizeUsagePair(pair: [number, number] | undefined): { input: number; output: number } {
  return {
    input: Array.isArray(pair) ? pair[0] || 0 : 0,
    output: Array.isArray(pair) ? pair[1] || 0 : 0,
  };
}

function localDateStr(d: Date): string {
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, '0');
  const day = String(d.getDate()).padStart(2, '0');
  return `${y}-${m}-${day}`;
}

function formatDateLabel(d: Date): string {
  return `${d.getMonth() + 1}/${d.getDate()}`;
}

// ── Summary section ───────────────────────────────────────────────────────────

function UsageSummary({ data }: { data: UsageData }) {
  const total = (data.total_input || 0) + (data.total_output || 0);
  const inputSource = data.input_source || 'estimated';
  const outputSource = data.output_source || 'estimated';
  const sourceScope = data.source_scope || 'latest_update';
  const sourceNote =
    sourceScope === 'latest_update'
      ? `Latest recorded token source: input ${inputSource}, output ${outputSource}. Cumulative totals may still include earlier estimates.`
      : `Token source: input ${inputSource}, output ${outputSource}.`;

  const stats = [
    { value: (data.daily_input || 0) + (data.daily_output || 0), label: 'Today Total' },
    { value: data.daily_input || 0, label: 'Today Input' },
    { value: data.daily_output || 0, label: 'Today Output' },
    { value: total, label: 'All-Time Total' },
    { value: data.total_input || 0, label: 'All-Time Input' },
    { value: data.total_output || 0, label: 'All-Time Output' },
  ];

  return (
    <div className="usage-summary">
      {stats.map((s) => (
        <div key={s.label} className="usage-stat-card">
          <div className="usage-stat-value">{formatTokenCount(s.value)}</div>
          <div className="usage-stat-label">{s.label}</div>
        </div>
      ))}
      <div className="usage-summary-note">{sourceNote}</div>
    </div>
  );
}

// ── Role breakdown ────────────────────────────────────────────────────────────

function RoleBreakdown({ data }: { data: UsageData }) {
  const dailyRoles = data.daily_roles || {};
  const totalRoles = data.total_roles || {};
  const names = Array.from(
    new Set([...ROLE_ORDER, ...Object.keys(dailyRoles), ...Object.keys(totalRoles)]),
  ).filter((name) => {
    if (!name) return false;
    const today = normalizeUsagePair(dailyRoles[name] as [number, number]);
    const tot = normalizeUsagePair(totalRoles[name] as [number, number]);
    return today.input + today.output + tot.input + tot.output > 0;
  });

  if (names.length === 0) {
    return (
      <div className="usage-role-breakdown">
        <p className="usage-empty-note">No role usage data yet.</p>
      </div>
    );
  }

  return (
    <div className="usage-role-breakdown">
      {names.map((name) => {
        const today = normalizeUsagePair(dailyRoles[name] as [number, number]);
        const tot = normalizeUsagePair(totalRoles[name] as [number, number]);
        return (
          <article key={name} className="usage-role-card">
            <div className="usage-role-header">
              <h3>{name}</h3>
              <span className="usage-role-chip">{formatTokenCount(tot.input + tot.output)}</span>
            </div>
            <div className="usage-role-metrics">
              <div className="usage-role-metric">
                <span className="usage-role-kicker">Today</span>
                <strong>{formatTokenCount(today.input + today.output)}</strong>
                <span>
                  {formatTokenCount(today.input)} in / {formatTokenCount(today.output)} out
                </span>
              </div>
              <div className="usage-role-metric">
                <span className="usage-role-kicker">All-Time</span>
                <strong>{formatTokenCount(tot.input + tot.output)}</strong>
                <span>
                  {formatTokenCount(tot.input)} in / {formatTokenCount(tot.output)} out
                </span>
              </div>
            </div>
          </article>
        );
      })}
    </div>
  );
}

// ── Daily line chart ──────────────────────────────────────────────────────────

interface DayPoint {
  date: string;
  label: string;
  input: number;
  output: number;
}

function buildDailyTimeline(data: UsageData, days: number): DayPoint[] {
  const history = data.usage_history || [];
  const today = new Date();
  const timeline: DayPoint[] = [];
  for (let i = days - 1; i >= 0; i--) {
    const d = new Date(today);
    d.setDate(d.getDate() - i);
    const dateStr = localDateStr(d);
    const label = formatDateLabel(d);
    if (i === 0) {
      timeline.push({
        date: dateStr,
        label,
        input: data.daily_input || 0,
        output: data.daily_output || 0,
      });
    } else {
      const snap = history.find((h) => h.date === dateStr);
      timeline.push({ date: dateStr, label, input: snap?.input || 0, output: snap?.output || 0 });
    }
  }
  return timeline;
}

function drawDataLine(
  ctx: CanvasRenderingContext2D,
  timeline: DayPoint[],
  stepX: number,
  padding: { top: number; right: number; bottom: number; left: number },
  getY: (v: number) => number,
  accessor: (d: DayPoint) => number,
  color: string,
  fillColor: string,
) {
  const n = timeline.length;
  if (n === 0) return;
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

function DailyChart({ data, days }: { data: UsageData; days: number }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  // Memoize the timeline so that unrelated parent re-renders (e.g. range
  // selector on the sibling chart, loading flag flips) don't force us to
  // recompute O(days) history lookups. `data` identity is stable between
  // fetches so depending on the whole object here is correct.
  const timeline = useMemo(() => buildDailyTimeline(data, days), [data, days]);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const parent = canvas.parentElement;
    if (!parent) return;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;

    const rect = parent.getBoundingClientRect();
    if (rect.width <= 32 || rect.height <= 32) return;

    const dpr = window.devicePixelRatio || 1;
    const w = rect.width - 32;
    const h = rect.height - 32;
    canvas.width = w * dpr;
    canvas.height = h * dpr;
    canvas.style.width = w + 'px';
    canvas.style.height = h + 'px';
    ctx.scale(dpr, dpr);

    const padding = { top: 24, right: 20, bottom: 44, left: 60 };
    const chartW = w - padding.left - padding.right;
    const chartH = h - padding.top - padding.bottom;

    const isDark = window.matchMedia('(prefers-color-scheme: dark)').matches;
    const textColor = isDark ? '#b0b5c0' : '#7e8699';
    const gridColor = isDark ? 'rgba(255,255,255,.06)' : 'rgba(0,0,0,.06)';

    let maxVal = 0;
    for (const p of timeline) if (p.input + p.output > maxVal) maxVal = p.input + p.output;
    if (maxVal === 0) maxVal = 100;

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
    const getY = (val: number) => padding.top + chartH - (val / maxVal) * chartH;

    drawDataLine(
      ctx,
      timeline,
      stepX,
      padding,
      getY,
      (d) => d.input,
      '#2d8bcf',
      'rgba(45,139,207,.12)',
    );
    drawDataLine(
      ctx,
      timeline,
      stepX,
      padding,
      getY,
      (d) => d.output,
      '#c06b9e',
      'rgba(192,107,158,.12)',
    );

    for (let i = 0; i < n; i++) {
      const x = padding.left + i * stepX;
      ctx.fillStyle = textColor;
      ctx.font = '10px system-ui';
      ctx.textAlign = 'center';
      if (n <= 7 || i % 2 === 0 || i === n - 1) {
        ctx.fillText(timeline[i].label, x, h - padding.bottom + 16);
      }
    }

    const legendY = h - 6;
    ctx.font = '11px system-ui';
    ctx.fillStyle = '#2d8bcf';
    ctx.fillRect(padding.left, legendY - 6, 10, 3);
    ctx.fillStyle = textColor;
    ctx.textAlign = 'left';
    ctx.fillText('Input', padding.left + 14, legendY);
    ctx.fillStyle = '#c06b9e';
    ctx.fillRect(padding.left + 60, legendY - 6, 10, 3);
    ctx.fillStyle = textColor;
    ctx.fillText('Output', padding.left + 74, legendY);
  }, [timeline]);

  return <canvas ref={canvasRef} />;
}

// ── Provider bar chart ────────────────────────────────────────────

interface ProviderTotals {
  input: number;
  output: number;
}

function buildProviderTotals(data: UsageData, days: number): Record<string, ProviderTotals> {
  const history = data.usage_history || [];
  const today = new Date();
  const totals: Record<string, ProviderTotals> = {};

  for (let i = days - 1; i >= 0; i--) {
    const d = new Date(today);
    d.setDate(d.getDate() - i);
    const dateStr = localDateStr(d);

    if (i === 0) {
      const dp = data.daily_providers || {};
      for (const [name, pair] of Object.entries(dp)) {
        if (!totals[name]) totals[name] = { input: 0, output: 0 };
        totals[name].input += (pair as [number, number])[0] || 0;
        totals[name].output += (pair as [number, number])[1] || 0;
      }
    } else {
      const snap = history.find((h) => h.date === dateStr);
      if (snap?.providers) {
        for (const [name, pair] of Object.entries(snap.providers)) {
          if (!totals[name]) totals[name] = { input: 0, output: 0 };
          totals[name].input += (pair as [number, number])[0] || 0;
          totals[name].output += (pair as [number, number])[1] || 0;
        }
      }
    }
  }
  return totals;
}

function truncateLabel(name: string, maxChars: number): string {
  if (name.length <= maxChars) return name;
  if (maxChars <= 1) return name.slice(0, maxChars);
  return name.slice(0, maxChars - 1) + '…';
}

function ProviderChart({ data, days }: { data: UsageData; days: number }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  // Memoize the aggregated provider totals so the effect only re-runs when
  // the underlying data actually changes. `data` identity is stable between
  // fetches so depending on the whole object here is correct.
  const providerTotals = useMemo(() => buildProviderTotals(data, days), [data, days]);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const parent = canvas.parentElement;
    if (!parent) return;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;

    const rect = parent.getBoundingClientRect();
    if (rect.width <= 32 || rect.height <= 32) return;

    const dpr = window.devicePixelRatio || 1;
    const w = rect.width - 32;
    const h = rect.height - 32;
    canvas.width = w * dpr;
    canvas.height = h * dpr;
    canvas.style.width = w + 'px';
    canvas.style.height = h + 'px';
    ctx.scale(dpr, dpr);

    const providers = Object.keys(providerTotals).sort();

    const isDark = window.matchMedia('(prefers-color-scheme: dark)').matches;
    const textColor = isDark ? '#b0b5c0' : '#7e8699';

    if (providers.length === 0) {
      ctx.clearRect(0, 0, w, h);
      ctx.fillStyle = textColor;
      ctx.font = '13px system-ui';
      ctx.textAlign = 'center';
      ctx.fillText('No per-provider data available yet', w / 2, h / 2);
      return;
    }

    const gridColor = isDark ? 'rgba(255,255,255,.06)' : 'rgba(0,0,0,.06)';
    const padding = { top: 24, right: 20, bottom: 48, left: 60 };
    const chartW = w - padding.left - padding.right;
    const chartH = h - padding.top - padding.bottom;

    const groupCount = providers.length;
    const groupGap = 24;
    const barGap = 4;
    const availW = chartW - groupGap * (groupCount + 1);
    const barWidth = Math.min(32, Math.max(12, availW / (groupCount * 2)));
    const groupWidth = barWidth * 2 + barGap;

    let maxVal = 0;
    for (const t of Object.values(providerTotals)) maxVal = Math.max(maxVal, t.input, t.output);
    if (maxVal === 0) maxVal = 100;

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
      const t = providerTotals[name];
      const gx = startX + gi * (groupWidth + groupGap);
      const color = PROVIDER_COLORS[gi % PROVIDER_COLORS.length];

      // Input bar
      const inputH = (t.input / maxVal) * chartH;
      ctx.fillStyle = color;
      ctx.globalAlpha = 0.9;
      ctx.fillRect(gx, padding.top + chartH - inputH, barWidth, inputH);
      ctx.globalAlpha = 1;

      // Output bar
      const outputH = (t.output / maxVal) * chartH;
      ctx.fillStyle = color;
      ctx.globalAlpha = 0.5;
      ctx.fillRect(gx + barWidth + barGap, padding.top + chartH - outputH, barWidth, outputH);
      ctx.globalAlpha = 1;

      // Label
      const labelX = gx + groupWidth / 2;
      ctx.fillStyle = textColor;
      ctx.font = '10px system-ui';
      ctx.textAlign = 'center';
      const maxLabelChars = Math.max(3, Math.floor(groupWidth / 6));
      ctx.fillText(truncateLabel(name, maxLabelChars), labelX, h - padding.bottom + 16);
    });

    // Legend
    const legendY = h - 4;
    ctx.font = '11px system-ui';
    ctx.fillStyle = '#2d8bcf';
    ctx.globalAlpha = 0.9;
    ctx.fillRect(padding.left, legendY - 7, 10, 6);
    ctx.globalAlpha = 1;
    ctx.fillStyle = textColor;
    ctx.textAlign = 'left';
    ctx.fillText('Input', padding.left + 14, legendY);
    ctx.fillStyle = '#2d8bcf';
    ctx.globalAlpha = 0.5;
    ctx.fillRect(padding.left + 60, legendY - 7, 10, 6);
    ctx.globalAlpha = 1;
    ctx.fillStyle = textColor;
    ctx.fillText('Output', padding.left + 74, legendY);
  }, [providerTotals]);

  return <canvas ref={canvasRef} />;
}

// ── Main UsagePage component ──────────────────────────────────────────────────

export function UsagePage() {
  const [visible, setVisible] = useState(false);
  const [usageData, setUsageData] = useState<UsageData | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState('');
  const [dailyRange, setDailyRange] = useState(7);
  const [providerRange, setProviderRange] = useState(7);

  useEffect(() => {
    _open = () => setVisible(true);
    _close = () => setVisible(false);
    if (pendingOpen) {
      pendingOpen = false;
      setVisible(true);
    }
    return () => {
      _open = null;
      _close = null;
    };
  }, []);

  // Toggle the container element's hidden attribute
  useEffect(() => {
    const el = document.getElementById('usage-page');
    if (el) el.hidden = !visible;
  }, [visible]);

  // Shared loader: used by both the visibility effect and the manual refresh
  // button. Accepts an optional AbortSignal so the visibility effect can cancel
  // in-flight requests when the overlay closes mid-load.
  const loadUsage = useCallback(async (signal?: AbortSignal) => {
    setLoading(true);
    setError('');
    try {
      const resp = await fetch('/api/usage', signal ? { signal } : undefined);
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      const data: UsageData = await resp.json();
      setUsageData(data);
    } catch (e: unknown) {
      if ((e as Error).name === 'AbortError') return;
      setError(`Failed to load usage data: ${(e as Error).message}`);
    } finally {
      if (!signal || !signal.aborted) setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (!visible) return;
    const controller = new AbortController();
    void loadUsage(controller.signal);
    return () => controller.abort();
  }, [visible, loadUsage]);

  const refreshUsage = useCallback(() => {
    void loadUsage();
  }, [loadUsage]);

  if (!visible) return null;

  // Note: RANGE_OPTIONS lives at module scope to keep a stable reference
  // across renders.

  // Render inside #usage-page overlay (panel content only)
  return (
    <div className="page-panel page-panel-wide usage-panel">
      <div className="page-header">
        <h2>Token Usage</h2>
        <div style={{ display: 'flex', alignItems: 'center', gap: 12 }}>
          <button className="btn-secondary" onClick={refreshUsage} disabled={loading}>
            {loading ? 'Loading...' : 'Refresh'}
          </button>
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

      <div className="page-body usage-page-body" id="usage-body">
        {error && <p style={{ color: 'var(--accent-error)' }}>{error}</p>}
        {!usageData && !error && loading && <p style={{ color: 'var(--dim)' }}>Loading...</p>}
        {usageData && (
          <>
            <UsageSummary data={usageData} />

            <div className="usage-role-section">
              <div className="usage-chart-header">
                <h3>Model Role Breakdown</h3>
              </div>
              <RoleBreakdown data={usageData} />
            </div>

            <div className="usage-charts-section">
              <div className="usage-chart-wrap">
                <div className="usage-chart-header">
                  <h3>Daily Usage</h3>
                  <select
                    value={dailyRange}
                    onChange={(e) => setDailyRange(Number(e.target.value))}
                  >
                    {RANGE_OPTIONS.map((o) => (
                      <option key={o.value} value={o.value}>
                        {o.label}
                      </option>
                    ))}
                  </select>
                </div>
                <div className="usage-chart-container" style={{ height: 220 }}>
                  <DailyChart data={usageData} days={dailyRange} />
                </div>
              </div>

              <div className="usage-chart-wrap">
                <div className="usage-chart-header">
                  <h3>By Provider</h3>
                  <select
                    value={providerRange}
                    onChange={(e) => setProviderRange(Number(e.target.value))}
                  >
                    {RANGE_OPTIONS.map((o) => (
                      <option key={o.value} value={o.value}>
                        {o.label}
                      </option>
                    ))}
                  </select>
                </div>
                <div className="usage-chart-container" style={{ height: 220 }}>
                  <ProviderChart data={usageData} days={providerRange} />
                </div>
              </div>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
