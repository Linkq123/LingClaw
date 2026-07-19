import React, { useCallback, useEffect, useId, useMemo, useRef, useState } from 'react';
import type { UsageData } from '../types/config.js';
import { isChinese, subscribeLanguageChange, tr } from '../i18n.js';
import '../css/usage-console.css';

let openBridge: (() => void) | null = null;
let closeBridge: (() => void) | null = null;
let pendingOpen = false;
let bridgeSessionId = '';

export function openUsagePage(sessionId?: string): void {
  if (sessionId !== undefined) bridgeSessionId = sessionId;
  if (openBridge) openBridge();
  else pendingOpen = true;
}

export function closeUsagePage(): void {
  pendingOpen = false;
  closeBridge?.();
}

export const USAGE_RANGE_OPTIONS = [7, 14, 30] as const;
export type UsageRange = (typeof USAGE_RANGE_OPTIONS)[number];
export type UsageBreakdownScope = 'range' | 'total';

export interface UsagePair {
  input: number;
  output: number;
  total: number;
}

export interface UsageTimelinePoint extends UsagePair {
  date: string;
  label: string;
}

export interface UsageRankingEntry extends UsagePair {
  name: string;
  unattributed?: boolean;
}

export interface UsageDashboardAggregate {
  timeline: UsageTimelinePoint[];
  today: UsagePair;
  allTime: UsagePair;
  range: UsagePair;
  average: UsagePair;
  activeDays: number;
  peak: UsageTimelinePoint | null;
  providers: {
    range: UsageRankingEntry[];
    total: UsageRankingEntry[];
  };
  roles: {
    range: UsageRankingEntry[];
    total: UsageRankingEntry[];
  };
  hasUsage: boolean;
}

export interface UsageViewState {
  loading: boolean;
  refreshing: boolean;
  error: string;
  hasData: boolean;
}

export interface UsageViewProps {
  sessionId?: string;
  active?: boolean;
  className?: string;
  onRequestClose?: () => void;
  onStateChange?: (state: UsageViewState) => void;
}

const UNATTRIBUTED = '__unattributed__';
const ROLE_ORDER = ['Primary', 'Fast', 'Sub-Agent', 'Memory', 'Reflection', 'Context'];

type UsageCopyKey =
  | 'session'
  | 'rangeAverage'
  | 'activeDays'
  | 'rangeTotal'
  | 'peakDay'
  | 'composition'
  | 'byRole'
  | 'selectedRange'
  | 'cumulative'
  | 'unattributed'
  | 'dataTable'
  | 'date'
  | 'total'
  | 'refreshing'
  | 'rangeSummary'
  | 'share'
  | 'noRoleRangeData'
  | 'noProviderRangeData';

const USAGE_COPY: Record<UsageCopyKey, { en: string; zh: string }> = {
  session: { en: 'Session', zh: '会话' },
  rangeAverage: { en: 'Daily average', zh: '范围日均' },
  activeDays: { en: 'Active days', zh: '活跃天数' },
  rangeTotal: { en: 'Range total', zh: '范围总量' },
  peakDay: { en: 'Peak day', zh: '峰值日' },
  composition: { en: 'Input / Output', zh: '输入 / 输出构成' },
  byRole: { en: 'By Agent role', zh: '按 Agent 角色' },
  selectedRange: { en: 'Selected range', zh: '所选范围' },
  cumulative: { en: 'Cumulative', zh: '累计' },
  unattributed: { en: 'Unattributed', zh: '未归因' },
  dataTable: { en: 'View data table', zh: '查看数据表' },
  date: { en: 'Date', zh: '日期' },
  total: { en: 'Total', zh: '总计' },
  refreshing: { en: 'Refreshing usage data…', zh: '正在刷新用量数据…' },
  rangeSummary: { en: 'Range summary', zh: '范围摘要' },
  share: { en: 'Share', zh: '占比' },
  noRoleRangeData: { en: 'No role data for this view', zh: '当前视图暂无角色数据' },
  noProviderRangeData: { en: 'No provider data for this view', zh: '当前视图暂无服务商数据' },
};

function usageCopy(key: UsageCopyKey): string {
  const translationKey = `usage.${key}`;
  const translated = tr(translationKey);
  if (translated !== translationKey) return translated;
  const copy = USAGE_COPY[key];
  return isChinese() ? copy.zh : copy.en;
}

function useLanguageVersion(): number {
  const [version, setVersion] = useState(0);
  useEffect(() => subscribeLanguageChange(() => setVersion((current) => current + 1)), []);
  return version;
}

function safeNumber(value: unknown): number {
  return typeof value === 'number' && Number.isFinite(value) ? Math.max(0, value) : 0;
}

function usagePair(input: unknown, output: unknown, explicitTotal?: unknown): UsagePair {
  const safeInput = safeNumber(input);
  const safeOutput = safeNumber(output);
  return {
    input: safeInput,
    output: safeOutput,
    total: Math.max(safeInput + safeOutput, safeNumber(explicitTotal)),
  };
}

function normalizePair(pair: [number, number] | undefined): UsagePair {
  return usagePair(pair?.[0], pair?.[1]);
}

function addPair(target: UsagePair, value: UsagePair): UsagePair {
  return usagePair(target.input + value.input, target.output + value.output);
}

function localDateString(date: Date): string {
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, '0');
  const day = String(date.getDate()).padStart(2, '0');
  return `${year}-${month}-${day}`;
}

function dateLabel(date: Date): string {
  return `${date.getMonth() + 1}/${date.getDate()}`;
}

function timelineDates(
  days: number,
  now: Date,
): Array<{ date: string; label: string; isToday: boolean }> {
  const normalizedDays = Math.max(1, Math.floor(days));
  const result: Array<{ date: string; label: string; isToday: boolean }> = [];
  for (let offset = normalizedDays - 1; offset >= 0; offset -= 1) {
    const date = new Date(now.getFullYear(), now.getMonth(), now.getDate());
    date.setDate(date.getDate() - offset);
    result.push({ date: localDateString(date), label: dateLabel(date), isToday: offset === 0 });
  }
  return result;
}

export function buildUsageTimeline(
  data: UsageData,
  days: number,
  now: Date = new Date(),
): UsageTimelinePoint[] {
  const history = new Map((data.usage_history || []).map((entry) => [entry.date, entry]));
  return timelineDates(days, now).map(({ date, label, isToday }) => {
    const entry = history.get(date);
    const pair = isToday
      ? usagePair(data.daily_input, data.daily_output)
      : usagePair(entry?.input, entry?.output);
    return { date, label, ...pair };
  });
}

function addDimension(
  target: Map<string, UsagePair>,
  values: Record<string, [number, number]> | undefined,
): void {
  for (const [name, rawPair] of Object.entries(values || {})) {
    if (!name) continue;
    const pair = normalizePair(rawPair);
    if (pair.total <= 0) continue;
    target.set(name, addPair(target.get(name) || usagePair(0, 0), pair));
  }
}

function rangeDimension(
  data: UsageData,
  days: number,
  now: Date,
  field: 'providers' | 'roles',
): Map<string, UsagePair> {
  const history = new Map((data.usage_history || []).map((entry) => [entry.date, entry]));
  const totals = new Map<string, UsagePair>();
  for (const date of timelineDates(days, now)) {
    if (date.isToday) {
      addDimension(totals, field === 'providers' ? data.daily_providers : data.daily_roles);
    } else {
      addDimension(totals, history.get(date.date)?.[field]);
    }
  }
  return totals;
}

function rankDimension(values: Map<string, UsagePair>, target: UsagePair): UsageRankingEntry[] {
  const entries: UsageRankingEntry[] = Array.from(values, ([name, pair]) => ({
    name,
    ...pair,
  })).sort((left, right) => right.total - left.total || left.name.localeCompare(right.name));
  if (entries.length === 0) return [];

  const attributed = entries.reduce((sum, entry) => addPair(sum, entry), usagePair(0, 0));
  const missingInput = Math.max(0, target.input - attributed.input);
  const missingOutput = Math.max(0, target.output - attributed.output);
  if (missingInput + missingOutput > 0) {
    entries.push({
      name: UNATTRIBUTED,
      ...usagePair(missingInput, missingOutput),
      unattributed: true,
    });
  }

  return entries;
}

function cumulativeDimension(
  values: Record<string, [number, number]> | undefined,
  target: UsagePair,
): UsageRankingEntry[] {
  const totals = new Map<string, UsagePair>();
  addDimension(totals, values);
  return rankDimension(totals, target);
}

function dimensionHasUsage(values: Record<string, [number, number]> | undefined): boolean {
  return Object.values(values || {}).some((value) => normalizePair(value).total > 0);
}

export function hasAnyUsageData(data: UsageData): boolean {
  if (
    usagePair(data.daily_input, data.daily_output).total > 0 ||
    usagePair(data.total_input, data.total_output, data.total).total > 0 ||
    dimensionHasUsage(data.daily_providers) ||
    dimensionHasUsage(data.total_providers) ||
    dimensionHasUsage(data.daily_roles) ||
    dimensionHasUsage(data.total_roles)
  ) {
    return true;
  }
  return (data.usage_history || []).some(
    (entry) =>
      usagePair(entry.input, entry.output).total > 0 ||
      dimensionHasUsage(entry.providers) ||
      dimensionHasUsage(entry.roles),
  );
}

export function buildUsageDashboard(
  data: UsageData,
  days: number,
  now: Date = new Date(),
): UsageDashboardAggregate {
  const timeline = buildUsageTimeline(data, days, now);
  const today = usagePair(data.daily_input, data.daily_output);
  const allTime = usagePair(data.total_input, data.total_output, data.total);
  const range = timeline.reduce((sum, point) => addPair(sum, point), usagePair(0, 0));
  const safeDays = Math.max(1, Math.floor(days));
  const average = usagePair(range.input / safeDays, range.output / safeDays);
  const activeDays = timeline.filter((point) => point.total > 0).length;
  const peak = timeline.reduce<UsageTimelinePoint | null>(
    (current, point) => (!current || point.total > current.total ? point : current),
    null,
  );
  const providerRange = rangeDimension(data, safeDays, now, 'providers');
  const roleRange = rangeDimension(data, safeDays, now, 'roles');

  const providers = {
    range: rankDimension(providerRange, range),
    total: cumulativeDimension(data.total_providers, allTime),
  };
  const roles = {
    range: rankDimension(roleRange, range),
    total: cumulativeDimension(data.total_roles, allTime),
  };
  const hasUsage = hasAnyUsageData(data);

  return {
    timeline,
    today,
    allTime,
    range,
    average,
    activeDays,
    peak: peak?.total ? peak : null,
    providers,
    roles,
    hasUsage,
  };
}

export function formatTokenCount(value: number | undefined): string {
  const number = safeNumber(value);
  if (number >= 1_000_000) return `${(number / 1_000_000).toFixed(1).replace(/\.0$/, '')}M`;
  if (number >= 1_000) return `${(number / 1_000).toFixed(1).replace(/\.0$/, '')}K`;
  return String(Math.round(number));
}

function formatPercent(value: number, total: number): string {
  if (total <= 0) return '0%';
  const percent = (value / total) * 100;
  return `${percent >= 10 ? percent.toFixed(0) : percent.toFixed(1)}%`;
}

function MetricCard({ label, value, detail }: { label: string; value: string; detail: string }) {
  return (
    <article className="usage-console-metric usage-stat-card">
      <span>{label}</span>
      <strong>{value}</strong>
      <small>{detail}</small>
    </article>
  );
}

function RangePicker({
  value,
  onChange,
}: {
  value: UsageRange;
  onChange: (range: UsageRange) => void;
}) {
  return (
    <div className="usage-console-range" aria-label={tr('usage.dailyRange')} role="group">
      {USAGE_RANGE_OPTIONS.map((range) => (
        <button
          key={range}
          type="button"
          className={value === range ? 'is-active' : ''}
          aria-pressed={value === range}
          onClick={() => onChange(range)}
        >
          {tr('usage.days', { count: range })}
        </button>
      ))}
    </div>
  );
}

function TrendChart({ aggregate }: { aggregate: UsageDashboardAggregate }) {
  const chartId = useId();
  const chartTitleId = `${chartId}-title`;
  const width = 720;
  const height = 250;
  const padding = { top: 18, right: 16, bottom: 40, left: 50 };
  const chartWidth = width - padding.left - padding.right;
  const chartHeight = height - padding.top - padding.bottom;
  const max = Math.max(1, ...aggregate.timeline.map((point) => point.total));
  const slotWidth = chartWidth / aggregate.timeline.length;
  const barWidth = Math.max(4, Math.min(22, slotWidth * 0.58));
  const labelEvery = aggregate.timeline.length <= 7 ? 1 : aggregate.timeline.length <= 14 ? 2 : 5;
  const pointDescriptions = aggregate.timeline.map((point, index) => ({
    id: `${chartId}-point-${index}`,
    text: `${point.date}: ${tr('usage.input')} ${formatTokenCount(point.input)}, ${tr(
      'usage.output',
    )} ${formatTokenCount(point.output)}, ${usageCopy('total')} ${formatTokenCount(point.total)}`,
  }));

  return (
    <>
      <svg
        className="usage-console-trend"
        viewBox={`0 0 ${width} ${height}`}
        role="group"
        aria-labelledby={chartTitleId}
      >
        <title id={chartTitleId}>{tr('usage.dailyUsage')}</title>
        {[0, 0.25, 0.5, 0.75, 1].map((ratio) => {
          const y = padding.top + chartHeight * ratio;
          return (
            <g key={ratio} aria-hidden="true">
              <line x1={padding.left} x2={width - padding.right} y1={y} y2={y} />
              <text x={padding.left - 8} y={y + 4} textAnchor="end">
                {formatTokenCount(max * (1 - ratio))}
              </text>
            </g>
          );
        })}
        {aggregate.timeline.map((point, index) => {
          const x = padding.left + slotWidth * index + (slotWidth - barWidth) / 2;
          const inputHeight = (point.input / max) * chartHeight;
          const outputHeight = (point.output / max) * chartHeight;
          const bottom = padding.top + chartHeight;
          const tooltipWidth = 138;
          const tooltipX = Math.max(
            padding.left,
            Math.min(width - padding.right - tooltipWidth, x + barWidth / 2 - tooltipWidth / 2),
          );
          const tooltipY = Math.max(padding.top, bottom - inputHeight - outputHeight - 34);
          const description = pointDescriptions[index];
          return (
            <g
              key={point.date}
              className="usage-console-trend-point"
              tabIndex={0}
              role="img"
              aria-label={point.date}
              aria-describedby={description.id}
            >
              <rect
                className="usage-console-bar-input"
                x={x}
                y={bottom - inputHeight}
                width={barWidth}
                height={inputHeight}
                rx="2"
              />
              <rect
                className="usage-console-bar-output"
                x={x}
                y={bottom - inputHeight - outputHeight}
                width={barWidth}
                height={outputHeight}
                rx="2"
              />
              {(index % labelEvery === 0 || index === aggregate.timeline.length - 1) && (
                <text aria-hidden="true" x={x + barWidth / 2} y={height - 15} textAnchor="middle">
                  {point.label}
                </text>
              )}
              <g className="usage-console-trend-tooltip" aria-hidden="true">
                <rect x={tooltipX} y={tooltipY} width={tooltipWidth} height="26" rx="6" />
                <text x={tooltipX + tooltipWidth / 2} y={tooltipY + 17} textAnchor="middle">
                  {point.label} · {formatTokenCount(point.total)}
                </text>
              </g>
            </g>
          );
        })}
      </svg>
      <div className="usage-console-chart-descriptions">
        {pointDescriptions.map((description) => (
          <span key={description.id} id={description.id}>
            {description.text}
          </span>
        ))}
      </div>
      <div className="usage-console-legend" aria-hidden="true">
        <span>
          <i className="is-input" />
          {tr('usage.input')}
        </span>
        <span>
          <i className="is-output" />
          {tr('usage.output')}
        </span>
      </div>
      <details className="usage-console-data-table">
        <summary>{usageCopy('dataTable')}</summary>
        <div>
          <table>
            <thead>
              <tr>
                <th scope="col">{usageCopy('date')}</th>
                <th scope="col">{tr('usage.input')}</th>
                <th scope="col">{tr('usage.output')}</th>
                <th scope="col">{usageCopy('total')}</th>
              </tr>
            </thead>
            <tbody>
              {aggregate.timeline.map((point) => (
                <tr key={point.date}>
                  <th scope="row">
                    <time dateTime={point.date}>{point.date}</time>
                  </th>
                  <td>{point.input}</td>
                  <td>{point.output}</td>
                  <td>{point.total}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </details>
    </>
  );
}

function CompositionChart({ pair }: { pair: UsagePair }) {
  const radius = 42;
  const circumference = 2 * Math.PI * radius;
  const inputRatio = pair.total > 0 ? pair.input / pair.total : 0;
  const inputDash = circumference * inputRatio;
  const outputDash = Math.max(0, circumference - inputDash);

  return (
    <div className="usage-console-composition">
      <svg viewBox="0 0 120 120" role="img" aria-label={usageCopy('composition')}>
        <circle className="usage-console-donut-track" cx="60" cy="60" r={radius} />
        <circle
          className="usage-console-donut-input"
          cx="60"
          cy="60"
          r={radius}
          strokeDasharray={`${inputDash} ${circumference - inputDash}`}
        />
        <circle
          className="usage-console-donut-output"
          cx="60"
          cy="60"
          r={radius}
          strokeDasharray={`${outputDash} ${circumference - outputDash}`}
          strokeDashoffset={-inputDash}
        />
        <text x="60" y="57" textAnchor="middle">
          {formatTokenCount(pair.total)}
        </text>
        <text className="usage-console-donut-caption" x="60" y="73" textAnchor="middle">
          {usageCopy('total')}
        </text>
      </svg>
      <dl>
        <div>
          <dt>
            <i className="is-input" />
            {tr('usage.input')}
          </dt>
          <dd>
            <strong>{formatTokenCount(pair.input)}</strong>
            <span>{formatPercent(pair.input, pair.total)}</span>
          </dd>
        </div>
        <div>
          <dt>
            <i className="is-output" />
            {tr('usage.output')}
          </dt>
          <dd>
            <strong>{formatTokenCount(pair.output)}</strong>
            <span>{formatPercent(pair.output, pair.total)}</span>
          </dd>
        </div>
      </dl>
    </div>
  );
}

function ScopePicker({
  value,
  onChange,
  label,
}: {
  value: UsageBreakdownScope;
  onChange: (scope: UsageBreakdownScope) => void;
  label: string;
}) {
  return (
    <div className="usage-console-scope" role="group" aria-label={label}>
      {(['range', 'total'] as const).map((scope) => (
        <button
          key={scope}
          type="button"
          aria-pressed={value === scope}
          className={value === scope ? 'is-active' : ''}
          onClick={() => onChange(scope)}
        >
          {scope === 'range' ? usageCopy('selectedRange') : usageCopy('cumulative')}
        </button>
      ))}
    </div>
  );
}

function Ranking({ entries, emptyText }: { entries: UsageRankingEntry[]; emptyText: string }) {
  if (entries.length === 0) return <div className="usage-console-inline-empty">{emptyText}</div>;
  const max = Math.max(1, ...entries.map((entry) => entry.total));
  const total = entries.reduce((sum, entry) => sum + entry.total, 0);
  return (
    <ol className="usage-console-ranking">
      {entries.map((entry) => (
        <li key={entry.name} className={entry.unattributed ? 'is-unattributed' : ''}>
          <div className="usage-console-rank-label">
            <span title={entry.unattributed ? usageCopy('unattributed') : entry.name}>
              {entry.unattributed ? usageCopy('unattributed') : entry.name}
            </span>
            <strong>{formatTokenCount(entry.total)}</strong>
          </div>
          <div className="usage-console-rank-track" aria-hidden="true">
            <i style={{ width: `${(entry.total / max) * 100}%` }} />
          </div>
          <small>
            {tr('usage.inOut', {
              input: formatTokenCount(entry.input),
              output: formatTokenCount(entry.output),
            })}
            {' · '}
            {formatPercent(entry.total, total)} {usageCopy('share')}
          </small>
        </li>
      ))}
    </ol>
  );
}

function LoadingSkeleton() {
  return (
    <div className="usage-console-skeleton" role="status" aria-label={tr('usage.loading')}>
      <div className="usage-console-skeleton-metrics">
        {Array.from({ length: 4 }, (_, index) => (
          <i key={index} />
        ))}
      </div>
      <i className="usage-console-skeleton-chart" />
    </div>
  );
}

export function UsageView({
  sessionId = '',
  active = true,
  className = '',
  onRequestClose,
  onStateChange,
}: UsageViewProps) {
  useLanguageVersion();
  const [data, setData] = useState<UsageData | null>(null);
  const [dataSessionId, setDataSessionId] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState('');
  const [range, setRange] = useState<UsageRange>(7);
  const [providerScope, setProviderScope] = useState<UsageBreakdownScope>('range');
  const [roleScope, setRoleScope] = useState<UsageBreakdownScope>('range');
  const requestGenerationRef = useRef(0);
  const activeRequestRef = useRef<AbortController | null>(null);
  const dataRef = useRef<UsageData | null>(null);
  const dataSessionIdRef = useRef<string | null>(null);
  const requestedSession = sessionId || 'main';

  const loadUsage = useCallback(
    async (preserveData: boolean) => {
      activeRequestRef.current?.abort();
      const controller = new AbortController();
      activeRequestRef.current = controller;
      const generation = ++requestGenerationRef.current;
      if (preserveData && dataSessionIdRef.current === requestedSession && dataRef.current) {
        setLoading(false);
        setRefreshing(true);
      } else {
        setRefreshing(false);
        setLoading(true);
        dataRef.current = null;
        dataSessionIdRef.current = null;
        setData(null);
        setDataSessionId(null);
      }
      setError('');

      try {
        const url = sessionId
          ? `/api/usage?session=${encodeURIComponent(sessionId)}`
          : '/api/usage';
        const response = await fetch(url, { signal: controller.signal });
        if (!response.ok) throw new Error(`HTTP ${response.status}`);
        const payload = (await response.json()) as UsageData;
        if (controller.signal.aborted || generation !== requestGenerationRef.current) return;
        dataRef.current = payload;
        dataSessionIdRef.current = requestedSession;
        setData(payload);
        setDataSessionId(requestedSession);
      } catch (caught: unknown) {
        const exception = caught as Error;
        if (
          exception.name === 'AbortError' ||
          controller.signal.aborted ||
          generation !== requestGenerationRef.current
        ) {
          return;
        }
        setError(exception.message);
      } finally {
        if (!controller.signal.aborted && generation === requestGenerationRef.current) {
          setLoading(false);
          setRefreshing(false);
          activeRequestRef.current = null;
        }
      }
    },
    [requestedSession, sessionId],
  );

  useEffect(() => {
    if (!active) {
      activeRequestRef.current?.abort();
      activeRequestRef.current = null;
      requestGenerationRef.current += 1;
      setLoading(false);
      setRefreshing(false);
      return;
    }
    if (dataSessionIdRef.current === requestedSession && dataRef.current) return;
    void loadUsage(false);
    return () => {
      activeRequestRef.current?.abort();
      activeRequestRef.current = null;
      requestGenerationRef.current += 1;
    };
  }, [active, loadUsage, requestedSession]);

  useEffect(
    () => () => {
      activeRequestRef.current?.abort();
      requestGenerationRef.current += 1;
    },
    [],
  );

  const currentData = dataSessionId === requestedSession ? data : null;
  const visibleLoading = loading;
  const aggregate = useMemo(
    () => (currentData ? buildUsageDashboard(currentData, range) : null),
    [currentData, range],
  );
  const viewState = useMemo<UsageViewState>(
    () => ({ loading: visibleLoading, refreshing, error, hasData: Boolean(currentData) }),
    [currentData, error, refreshing, visibleLoading],
  );
  useEffect(() => onStateChange?.(viewState), [onStateChange, viewState]);

  const sourceNote = currentData
    ? currentData.source_scope === 'latest_update'
      ? tr('usage.latestSource', {
          input: currentData.input_source || 'estimated',
          output: currentData.output_source || 'estimated',
        })
      : tr('usage.source', {
          input: currentData.input_source || 'estimated',
          output: currentData.output_source || 'estimated',
        })
    : '';

  return (
    <section
      className={`usage-console-view ${className}`.trim()}
      aria-labelledby="usage-view-title"
      aria-busy={visibleLoading || refreshing}
    >
      <header className="usage-console-toolbar">
        <div>
          <h2 id="usage-view-title" tabIndex={-1}>
            {tr('usage.title')}
          </h2>
          <p>
            <span>{usageCopy('session')}</span>
            <code>{requestedSession}</code>
          </p>
        </div>
        <div className="usage-console-toolbar-actions">
          <RangePicker value={range} onChange={setRange} />
          <button
            type="button"
            className="usage-console-refresh"
            onClick={() => void loadUsage(true)}
            disabled={visibleLoading || refreshing}
          >
            <svg
              className={`icon${visibleLoading || refreshing ? ' is-spinning' : ''}`}
              aria-hidden="true"
            >
              <use href="#icon-refresh" />
            </svg>
            <span>{refreshing ? usageCopy('refreshing') : tr('usage.refresh')}</span>
          </button>
          {onRequestClose && (
            <button
              type="button"
              className="usage-console-close"
              aria-label={tr('common.close')}
              title={tr('common.close')}
              onClick={onRequestClose}
            >
              <svg className="icon" aria-hidden="true">
                <use href="#icon-close" />
              </svg>
            </button>
          )}
        </div>
      </header>

      <div className="usage-console-live" role="status" aria-live="polite">
        {refreshing ? usageCopy('refreshing') : ''}
      </div>

      {error && currentData && (
        <div className="usage-console-alert" role="alert">
          <span>{tr('usage.loadFailed', { error })}</span>
          <button type="button" onClick={() => void loadUsage(true)} disabled={refreshing}>
            {tr('usage.retry')}
          </button>
        </div>
      )}

      {!currentData && visibleLoading && <LoadingSkeleton />}

      {!currentData && !visibleLoading && error && (
        <div className="usage-console-state" role="alert">
          <svg className="icon" aria-hidden="true">
            <use href="#icon-activity" />
          </svg>
          <strong>{tr('usage.loadErrorTitle')}</strong>
          <p>{tr('usage.loadFailed', { error })}</p>
          <button type="button" onClick={() => void loadUsage(false)} disabled={visibleLoading}>
            {tr('usage.retry')}
          </button>
        </div>
      )}

      {currentData && aggregate && !aggregate.hasUsage && (
        <div className="usage-console-state" role="status">
          <svg className="icon" aria-hidden="true">
            <use href="#icon-chart" />
          </svg>
          <strong>{tr('usage.emptyTitle')}</strong>
          <p>{tr('usage.emptyBody')}</p>
        </div>
      )}

      {currentData && aggregate?.hasUsage && (
        <div className="usage-console-content">
          <div className="usage-console-metrics">
            <MetricCard
              label={tr('usage.todayTotal')}
              value={formatTokenCount(aggregate.today.total)}
              detail={tr('usage.inOut', {
                input: formatTokenCount(aggregate.today.input),
                output: formatTokenCount(aggregate.today.output),
              })}
            />
            <MetricCard
              label={tr('usage.allTimeTotal')}
              value={formatTokenCount(aggregate.allTime.total)}
              detail={tr('usage.inOut', {
                input: formatTokenCount(aggregate.allTime.input),
                output: formatTokenCount(aggregate.allTime.output),
              })}
            />
            <MetricCard
              label={usageCopy('rangeAverage')}
              value={formatTokenCount(aggregate.average.total)}
              detail={tr('usage.days', { count: range })}
            />
            <MetricCard
              label={usageCopy('activeDays')}
              value={String(aggregate.activeDays)}
              detail={`${aggregate.activeDays} / ${range}`}
            />
          </div>

          <section className="usage-console-panel usage-console-trend-panel">
            <header>
              <div>
                <span>{usageCopy('rangeSummary')}</span>
                <h3>{tr('usage.dailyUsage')}</h3>
              </div>
              <dl>
                <div>
                  <dt>{usageCopy('rangeTotal')}</dt>
                  <dd>{formatTokenCount(aggregate.range.total)}</dd>
                </div>
                <div>
                  <dt>{usageCopy('peakDay')}</dt>
                  <dd>
                    {aggregate.peak
                      ? `${aggregate.peak.label} · ${formatTokenCount(aggregate.peak.total)}`
                      : '—'}
                  </dd>
                </div>
              </dl>
            </header>
            {aggregate.range.total > 0 ? (
              <TrendChart aggregate={aggregate} />
            ) : (
              <div className="usage-console-inline-empty">{tr('usage.noDailyData')}</div>
            )}
          </section>

          <div className="usage-console-grid">
            <section className="usage-console-panel">
              <header>
                <h3>{usageCopy('composition')}</h3>
              </header>
              <CompositionChart pair={aggregate.range} />
            </section>

            <section className="usage-console-panel">
              <header>
                <h3>{tr('usage.byProvider')}</h3>
                <ScopePicker
                  value={providerScope}
                  onChange={setProviderScope}
                  label={tr('usage.providerRange')}
                />
              </header>
              <Ranking
                entries={aggregate.providers[providerScope]}
                emptyText={usageCopy('noProviderRangeData')}
              />
            </section>

            <section className="usage-console-panel">
              <header>
                <h3>{usageCopy('byRole')}</h3>
                <ScopePicker
                  value={roleScope}
                  onChange={setRoleScope}
                  label={tr('usage.roleBreakdown')}
                />
              </header>
              <Ranking
                entries={sortRoles(aggregate.roles[roleScope])}
                emptyText={usageCopy('noRoleRangeData')}
              />
            </section>
          </div>

          <p className="usage-console-source-note">{sourceNote}</p>
        </div>
      )}
    </section>
  );
}

export function sortRoles(entries: UsageRankingEntry[]): UsageRankingEntry[] {
  const order = new Map(ROLE_ORDER.map((name, index) => [name.toLowerCase(), index]));
  return [...entries].sort((left, right) => {
    const totalDifference = right.total - left.total;
    if (totalDifference !== 0) return totalDifference;
    const leftOrder = order.get(left.name.toLowerCase()) ?? ROLE_ORDER.length;
    const rightOrder = order.get(right.name.toLowerCase()) ?? ROLE_ORDER.length;
    if (leftOrder !== rightOrder) return leftOrder - rightOrder;
    if (left.unattributed !== right.unattributed) return left.unattributed ? 1 : -1;
    return left.name.localeCompare(right.name);
  });
}

/** Compatibility island used until every caller mounts UsageView through Console. */
export function UsagePage() {
  const [visible, setVisible] = useState(false);
  const [sessionId, setSessionId] = useState('');

  useEffect(() => {
    openBridge = () => {
      setSessionId(bridgeSessionId);
      setVisible(true);
    };
    closeBridge = () => setVisible(false);
    if (pendingOpen) {
      pendingOpen = false;
      setSessionId(bridgeSessionId);
      setVisible(true);
    }
    return () => {
      openBridge = null;
      closeBridge = null;
    };
  }, []);

  useEffect(() => {
    const root = document.getElementById('usage-page');
    if (root) root.hidden = !visible;
  }, [visible]);

  if (!visible) return null;
  return (
    <UsageView
      sessionId={sessionId}
      active={visible}
      className="usage-console-compat"
      onRequestClose={() => setVisible(false)}
    />
  );
}
