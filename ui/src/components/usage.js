// Context, cache and cost under the composer (dsh ContextMeter, StatsPills
// and the stats dialog), the usage of one turn, and Settings → Usage. Every
// number comes from the server (GET /api/sessions/:id/stats, GET /api/usage),
// which prices requests in USD; the UI only formats.
import { useEffect, useLayoutEffect, useRef, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t, lang } from '../i18n.js';
import { createStore, useStore } from '../store.js';
import { store, api, openSettings, openSession } from '../state.js';
import { Segmented } from './settings.js';
import { workspaceName } from './composer.js';
import { IconChevronRight } from '../icons.js';

// ---- formatting --------------------------------------------------------------

const nf = (opts) => new Intl.NumberFormat(lang, opts);
const PERCENT = nf({ style: 'percent', maximumFractionDigits: 0 });
const PERCENT1 = nf({ style: 'percent', maximumFractionDigits: 1 });
const INT = nf({ maximumFractionDigits: 0 });
const COMPACT = nf({ notation: 'compact', maximumFractionDigits: 1 });
const DECIMAL1 = nf({ maximumFractionDigits: 1 });

/** USD with as many decimals as a small amount needs: $0.00059, $0.0031, $0.042, $1.23. */
export function usd(value, approximate = false) {
  const v = Math.max(0, value ?? 0);
  const digits = v >= 1 || v === 0 ? 2 : Math.min(6, Math.max(2, 1 - Math.floor(Math.log10(v))));
  const s = nf({ style: 'currency', currency: 'USD', minimumFractionDigits: digits, maximumFractionDigits: digits }).format(v);
  return approximate ? `≈ ${s}` : s;
}

export const percent = (ratio, precise = false) => (precise ? PERCENT1 : PERCENT).format(Math.max(0, ratio || 0));
export const tokens = (n) => INT.format(n ?? 0);
/** Exact below 100k (grouped), then "1.2M" / "1,2 Mio.". */
export const shortTokens = (n) => ((n ?? 0) < 100_000 ? INT.format(n ?? 0) : COMPACT.format(n));

/** "0.4 s", "12 s", "3 min 20 s" from milliseconds. */
export function seconds(ms) {
  if (ms == null) return '–';
  if (ms < 10_000) return `${DECIMAL1.format(ms / 1000)} s`;
  return turnDuration(ms);
}

/** dsh `formatRunDuration`: "1m 20s" (German "1 min 20 s"). */
export function turnDuration(ms) {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor(total / 60) % 60;
  const s = total % 60;
  const g = t.unitGap;
  const parts = [];
  if (h) parts.push(`${h}${g}${t.hoursUnit}`);
  if (total >= 60) parts.push(`${m}${g}${t.minutesUnit}`);
  parts.push(`${s}${g}${t.secondsUnit}`);
  return parts.join(' ');
}

const inputOf = (u) => (u?.inputTokens ?? 0) + (u?.cacheReadTokens ?? 0) + (u?.cacheWriteTokens ?? 0);
const hitRate = (u) => (inputOf(u) ? (u.cacheReadTokens ?? 0) / inputOf(u) : null);
const allTokens = (u) => inputOf(u) + (u?.outputTokens ?? 0);

// ---- the open chat's stats ------------------------------------------------------

/** Stats of the active chat, refreshed after each request that reports usage. */
export const statsFeed = createStore({ id: null, stats: null });

const RELEVANT = new Set(['assistant/message', 'turn/start', 'turn/end', 'compaction/summary', 'compaction/end']);
let tracked = { id: null, seq: -1, loaded: false };
let timer = 0;
let pendingFrom = null;

async function fetchStats(id, fromTurn) {
  try {
    const next = await api.stats(id, fromTurn);
    if (!next || statsFeed.get().id !== id) return;
    const prev = statsFeed.get().stats;
    // Partial answers carry the turns from `fromTurn` on; the rest is kept.
    const turns = fromTurn != null && prev ? [...prev.turns.filter((x) => x.turn < fromTurn), ...next.turns] : next.turns;
    statsFeed.set({ stats: { ...next, turns } });
  } catch { /* keep the last numbers */ }
}

function schedule(id, fromTurn, delay) {
  pendingFrom = pendingFrom === null || fromTurn === null ? fromTurn : Math.min(pendingFrom, fromTurn);
  clearTimeout(timer);
  timer = setTimeout(() => {
    const from = pendingFrom;
    pendingFrom = null;
    fetchStats(id, from);
  }, delay);
}

store.subscribe(() => {
  const s = store.get();
  const id = s.activeId;
  if (id !== tracked.id) {
    tracked = { id, seq: -1, loaded: false };
    clearTimeout(timer);
    pendingFrom = null;
    statsFeed.set({ id, stats: null });
  }
  if (!id || s.header?.id !== id) return;
  const events = s.events;
  const last = events.length ? events[events.length - 1].seq : -1;
  if (!tracked.loaded) {
    tracked.loaded = true;
    tracked.seq = last;
    schedule(id, null, 0);
    return;
  }
  if (last <= tracked.seq) return;
  let relevant = false;
  let ended = false;
  let from = Infinity;
  for (let i = events.length - 1; i >= 0 && events[i].seq > tracked.seq; i--) {
    const e = events[i];
    if (!RELEVANT.has(e.type)) continue;
    relevant = true;
    ended ||= e.type === 'turn/end';
    if (typeof e.data?.turn === 'number') from = Math.min(from, e.data.turn);
  }
  tracked.seq = last;
  const known = statsFeed.get().stats?.turns;
  // A finished turn shows its cost at once; steps in a running turn are batched.
  if (relevant) schedule(id, Number.isFinite(from) ? from : known?.length ? known[known.length - 1].turn : null, ended ? 60 : 350);
});

export const useStats = () => useStore(statsFeed, (f) => f.stats);

// ---- popover -------------------------------------------------------------------

/**
 * A chip that opens a small dialog above (or below) it, placed in the viewport
 * so scrolling lists cannot clip it. `hover` also opens it on pointer hover.
 */
export function Pop({ label, trigger, class: klass, children, hover = false, title }) {
  const [open, setOpen] = useState(null); // null | 'click' | 'hover'
  const anchor = useRef(null);
  const panel = useRef(null);
  const hoverTimer = useRef(0);

  const place = () => {
    const el = panel.current;
    const a = anchor.current?.getBoundingClientRect();
    if (!el || !a) return;
    const box = el.getBoundingClientRect();
    let top = a.top - box.height - 8;
    if (top < 8) top = Math.min(innerHeight - box.height - 8, a.bottom + 8);
    const left = Math.max(8, Math.min(a.right - box.width, innerWidth - box.width - 8));
    el.style.top = `${Math.max(8, top)}px`;
    el.style.left = `${left}px`;
  };
  useLayoutEffect(() => { if (open) place(); });
  useEffect(() => {
    if (!open) return undefined;
    const down = (e) => {
      if (panel.current?.contains(e.target) || anchor.current?.contains(e.target)) return;
      setOpen(null);
    };
    const key = (e) => {
      if (e.key !== 'Escape') return;
      setOpen(null);
      anchor.current?.focus();
    };
    document.addEventListener('pointerdown', down, true);
    document.addEventListener('keydown', key);
    window.addEventListener('resize', place);
    window.addEventListener('scroll', place, true);
    return () => {
      document.removeEventListener('pointerdown', down, true);
      document.removeEventListener('keydown', key);
      window.removeEventListener('resize', place);
      window.removeEventListener('scroll', place, true);
    };
  }, [open]);

  const enter = () => {
    if (!hover || open === 'click') return;
    clearTimeout(hoverTimer.current);
    hoverTimer.current = setTimeout(() => setOpen((v) => v ?? 'hover'), 350);
  };
  const leave = () => {
    if (!hover) return;
    clearTimeout(hoverTimer.current);
    hoverTimer.current = setTimeout(() => setOpen((v) => (v === 'hover' ? null : v)), 160);
  };
  return html`<span class="ub-wrap" onMouseEnter=${enter} onMouseLeave=${leave}>
    <button type="button" ref=${anchor} class=${cx('ub-chip', klass)} aria-haspopup="dialog" aria-expanded=${!!open}
      aria-label=${label} title=${open ? undefined : title}
      onClick=${(e) => { e.stopPropagation(); clearTimeout(hoverTimer.current); setOpen((v) => (v === 'click' ? null : 'click')); }}>${trigger}</button>
    ${open ? html`<div ref=${panel} class="ub-pop" role="dialog" aria-label=${label} onMouseEnter=${enter} onMouseLeave=${leave}>${children}</div>` : null}
  </span>`;
}

function PopHead({ icon, title, value }) {
  return html`<div class="ub-head">
    <span class="ub-head-title">${icon}${title}</span>${value != null ? html`<span class="ub-head-value">${value}</span>` : null}
  </div><div class="ub-rule" aria-hidden="true" />`;
}

/** Label/value rows (dsh stat dialog grid); `null` rows are skipped. */
function Rows({ rows }) {
  return html`<dl class="ub-rows">${rows.filter(Boolean).map(([k, v, note]) => html`<dt>${k}</dt><dd>${v}${note ? html`<span class="ub-note">${note}</span>` : null}</dd>`)}</dl>`;
}

// ---- the bar under the composer ------------------------------------------------------

/** 16 px occupancy ring: ink, amber from 70 %, red from 90 %. */
function Ring({ ratio }) {
  const r = 6.25;
  const c = 2 * Math.PI * r;
  const level = ratio >= 0.9 ? 'is-full' : ratio >= 0.7 ? 'is-high' : '';
  return html`<svg class=${cx('ub-ring', level)} width="16" height="16" viewBox="0 0 16 16" aria-hidden="true">
    <circle class="ub-ring-track" cx="8" cy="8" r=${r} />
    <circle class="ub-ring-fill" cx="8" cy="8" r=${r} stroke-dasharray=${`${c * Math.min(1, Math.max(0, ratio))} ${c}`} transform="rotate(-90 8 8)" />
  </svg>`;
}

const CacheIcon = () => html`<svg class="icon" width="14" height="14" viewBox="0 0 16 16" aria-hidden="true"><ellipse cx="8" cy="4" rx="5.25" ry="2.25" fill="none" stroke="currentColor" stroke-width="1.3"/><path d="M2.75 4v8c0 1.25 2.35 2.25 5.25 2.25s5.25-1 5.25-2.25V4M2.75 8c0 1.25 2.35 2.25 5.25 2.25S13.25 9.25 13.25 8" fill="none" stroke="currentColor" stroke-width="1.3"/></svg>`;
const CoinIcon = () => html`<svg class="icon" width="14" height="14" viewBox="0 0 16 16" aria-hidden="true"><circle cx="8" cy="8" r="6.25" fill="none" stroke="currentColor" stroke-width="1.3"/><path d="M9.9 5.9c-.3-.7-1-1.1-1.9-1.1-1.1 0-1.9.6-1.9 1.5 0 2 3.9 1.1 3.9 3.3 0 .9-.9 1.6-2 1.6-.9 0-1.7-.5-2-1.2M8 3.6v1.2m0 6.4v1.2" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/></svg>`;

function ContextChip({ stats }) {
  const c = stats.context;
  if (!c?.contextWindow) return null;
  const ratio = c.usedTokens / c.contextWindow;
  const p = percent(ratio);
  const b = c.breakdown ?? {};
  const estimate = (b.systemTokens ?? 0) + (b.toolsTokens ?? 0) + (b.messageTokens ?? 0);
  // dsh scales the heuristic composition to the measured occupancy.
  const parts = [['system', t.ctxSystem, b.systemTokens], ['tools', t.ctxTools, b.toolsTokens], ['messages', t.ctxMessages, b.messageTokens]]
    .map(([key, name, n]) => ({ key, name, tokens: estimate ? Math.round((c.usedTokens * (n ?? 0)) / estimate) : 0 }));
  const auto = c.compaction;
  return html`<${Pop} label=${t.ctxAria(p)} title=${t.ctxAria(p)} trigger=${html`<${Ring} ratio=${ratio} /><span>${p}</span>`}>
    <div class="ub-head"><span class="ub-head-title"><${Ring} ratio=${ratio} />${t.ctxUsed(p, shortTokens(c.contextWindow))}</span>
      <span class="ub-head-value">${shortTokens(c.usedTokens)}</span></div>
    <div class="ub-meter" aria-hidden="true">${parts.map((x) => (x.tokens > 0
    ? html`<span class=${`ub-seg ub-seg-${x.key}`} style=${`width:${Math.max(0.6, (x.tokens / c.contextWindow) * 100)}%`} />` : null))}</div>
    <dl class="ub-rows ub-legend">${parts.map((x) => html`<dt><span class=${`ub-swatch ub-seg-${x.key}`} />${x.name}</dt><dd>${shortTokens(x.tokens)}</dd>`)}</dl>
    <p class="ub-foot">${c.estimated ? t.ctxAfterCompaction : t.ctxEstimate}
      ${auto ? ` ${auto.auto ? t.ctxAuto(percent(auto.thresholdPercent / 100)) : t.ctxAutoOff}` : ''}</p>
  </${Pop}>`;
}

function CacheChip({ stats }) {
  const total = stats.totals;
  const rate = hitRate(total);
  if (rate === null) return null;
  const last = hitRate(stats.lastRequest);
  return html`<${Pop} label=${t.cacheAria(percent(rate))} title=${t.cacheAria(percent(rate))} trigger=${html`<${CacheIcon} /><span>${percent(rate)}</span>`}>
    <${PopHead} icon=${html`<${CacheIcon} />`} title=${t.cache} value=${percent(rate, true)} />
    <${Rows} rows=${[
    [t.cacheHitChat, percent(rate, true)],
    last !== null && [t.cacheHitLast, percent(last, true)],
    [t.cachedInput, t.tokensCount(tokens(total.cacheReadTokens))],
    [t.uncachedInput, t.tokensCount(tokens(total.inputTokens))],
    total.cacheWriteTokens > 0 && [t.cacheWrites, t.tokensCount(tokens(total.cacheWriteTokens))],
    total.savedUsd > 0 && [t.cacheSaved, usd(total.savedUsd, total.approximate)],
  ]} />
  </${Pop}>`;
}

/** Cost label: USD, or tokens when nothing has a price. */
function costLabel(u, extra = 0) {
  if (!u) return '–';
  if (!u.costUsd && !extra && u.unpricedRequests) return t.tokensCount(shortTokens(allTokens(u)));
  return usd((u.costUsd ?? 0) + extra, u.approximate);
}

function CostChip({ stats, detailed }) {
  const total = stats.totals;
  const sub = stats.subagents;
  const lastTurn = stats.turns?.[stats.turns.length - 1];
  const tm = stats.timings ?? {};
  const label = costLabel(total, sub?.costUsd ?? 0);
  return html`<${Pop} label=${`${t.cost}: ${label}`} title=${t.cost} trigger=${html`<${CoinIcon} /><span>${label}</span>`}>
    <${PopHead} icon=${html`<${CoinIcon} />`} title=${t.cost} value=${label} />
    <${Rows} rows=${[
    [t.costChat, costLabel(total)],
    sub?.sessions > 0 && [t.costSubagents(sub.sessions), costLabel(sub)],
    lastTurn && [t.costLastTurn, costLabel(lastTurn)],
    [t.costCached, usd(total.cost?.cacheRead)],
    [t.costUncached, usd(total.cost?.input)],
    total.cacheWriteTokens > 0 && [t.costCacheWrite, usd(total.cost?.cacheWrite)],
    [t.costOutput, usd(total.cost?.output), total.reasoningTokens ? t.costReasoning(tokens(total.reasoningTokens)) : null],
    total.requests > 0 && [t.offPeak, t.offPeakValue(INT.format(total.offPeakRequests ?? 0), INT.format(total.requests))],
  ]} />
    ${detailed ? html`<div class="ub-section">${t.perf}</div>
    <${Rows} rows=${[
    [t.ttft, seconds(tm.avgTtftMs)],
    [t.tps, tm.tokensPerSecond != null ? t.tokPerSec(DECIMAL1.format(tm.tokensPerSecond)) : '–'],
    [t.llmTime, seconds(tm.llmMs)],
    [t.toolTime, seconds(tm.toolMs)],
  ]} />` : null}
    ${total.approximate ? html`<p class="ub-foot">${t.approxNote}</p>` : null}
    ${total.unpricedRequests ? html`<p class="ub-foot">${t.unpricedNote(total.unpricedRequests)}</p>` : null}
    <button type="button" class="ub-link" onClick=${openUsage}>${t.allUsage}<${IconChevronRight} size=${12} /></button>
  </${Pop}>`;
}

/** Settings → Usage. */
export function openUsage() {
  openSettings();
  requestAnimationFrame(() => document.getElementById('usage')?.scrollIntoView({ block: 'start', behavior: 'smooth' }));
}

/** Context ring, cache and cost under the composer; compact mode keeps ring and cost. */
export function UsageBar() {
  const stats = useStats();
  const mode = useStore(store, (s) => s.info?.ui?.performanceUsage ?? 'detailed');
  if (!stats || (!stats.totals?.requests && !stats.context?.usedTokens)) return null;
  const detailed = mode !== 'compact';
  return html`<div class="usage-bar">
    <${ContextChip} stats=${stats} />
    ${detailed ? html`<${CacheChip} stats=${stats} />` : null}
    <${CostChip} stats=${stats} detailed=${detailed} />
  </div>`;
}

// ---- one turn (dsh message.turnUsage.*) -------------------------------------------------

/** The cost of a finished turn; opens its usage on hover or click. */
export function TurnCost({ turn }) {
  const stats = useStats();
  const u = stats?.turns?.find((x) => x.turn === turn);
  if (!u?.requests) return null;
  const label = costLabel(u);
  const tps = u.timings?.tokensPerSecond;
  return html`<${Pop} hover label=${t.turnUsage} class="ub-turn" trigger=${html`<span>${label}</span>`}>
    <${PopHead} icon=${html`<${CoinIcon} />`} title=${t.turnUsage} value=${t.tokensCount(tokens(allTokens(u)))} />
    <${Rows} rows=${[
    u.routes?.length && [t.providerModel, html`<span class="ub-route">${u.routes.join(', ')}</span>`],
    hitRate(u) !== null && [t.cacheHit, percent(hitRate(u), true)],
    [t.uncachedInput, t.tokensCount(tokens(u.inputTokens))],
    [t.cachedInput, t.tokensCount(tokens(u.cacheReadTokens))],
    u.cacheWriteTokens > 0 && [t.cacheWrites, t.tokensCount(tokens(u.cacheWriteTokens))],
    [t.costOutput, t.tokensCount(tokens(u.outputTokens)), u.reasoningTokens ? t.costReasoning(tokens(u.reasoningTokens)) : null],
    tps != null && [t.speed, t.tokPerSec(DECIMAL1.format(tps))],
    [t.cost, label],
  ]} />
  </${Pop}>`;
}

// ---- Settings → Usage ------------------------------------------------------------------

const SERIES = 3; // categorical slots; further models fold into "other"

function localMidnight(daysAgo) {
  const d = new Date();
  d.setHours(0, 0, 0, 0);
  d.setDate(d.getDate() - daysAgo);
  return d.getTime();
}

const dayName = (day) => new Date(`${day}T12:00:00`).toLocaleDateString(lang, { weekday: 'short', day: 'numeric', month: 'numeric' });

function Tile({ label, value }) {
  return html`<div class="uv-tile"><div class="uv-tile-label">${label}</div><div class="uv-tile-value">${value}</div></div>`;
}

/** Cost per day as stacked bars, one colour per model (fixed order of the period's totals). */
function DayBars({ report }) {
  const series = report.models.slice(0, SERIES).map((m) => `${m.provider}/${m.model}`);
  const other = report.models.length > SERIES;
  const byCost = report.totals.costUsd > 0;
  const value = (u) => (byCost ? u.costUsd : allTokens(u));
  const max = Math.max(...report.days.map((d) => value(d)), 0);
  const every = report.days.length > 10 ? 5 : 1;
  return html`<div class="uv-chart">
    <div class="uv-bars" role="list">
      ${report.days.map((d, i) => {
    const parts = series.map((key, k) => ({ k, u: d.models.find((m) => `${m.provider}/${m.model}` === key) }))
      .filter((p) => p.u && value(p.u) > 0);
    const rest = d.models.filter((m) => !series.includes(`${m.provider}/${m.model}`)).reduce((n, m) => n + value(m), 0);
    const tip = t.usageDayTip(dayName(d.day), costLabel(d), shortTokens(allTokens(d)), hitRate(d) === null ? null : percent(hitRate(d)));
    // Shares in per cent: flex-grow below 1 in total would leave the bar partly empty.
    const share = (v) => (value(d) ? (v / value(d)) * 100 : 0);
    return html`<div class="uv-col" role="listitem" key=${d.day} title=${tip} aria-label=${tip}>
          <div class="uv-stack" style=${`height:${max ? (value(d) / max) * 100 : 0}%`}>
            ${rest > 0 ? html`<span class="uv-seg uv-other" style=${`flex-grow:${share(rest)}`} />` : null}
            ${parts.reverse().map((p) => html`<span class=${`uv-seg uv-s${p.k + 1}`} style=${`flex-grow:${share(value(p.u))}`} />`)}
          </div>
          <span class="uv-day">${i % every === 0 || i === report.days.length - 1 ? new Date(`${d.day}T12:00:00`).getDate() : ''}</span>
        </div>`;
  })}
    </div>
    ${series.length > 1 || other ? html`<div class="uv-legend">${series.map((key, k) => html`<span><i class=${`uv-swatch uv-s${k + 1}`} />${key.split('/').pop()}</span>`)}
      ${other ? html`<span><i class="uv-swatch uv-other" />${t.usageOther}</span>` : null}</div>` : null}
  </div>`;
}

export function UsageCard() {
  const [days, setDays] = useState(7);
  const [state, setState] = useState({ loading: true, report: null, error: null });
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    let live = true;
    setState((s) => ({ ...s, loading: true, error: null }));
    api.usage({ since: localMidnight(days - 1), tz: -new Date().getTimezoneOffset(), top: 5 })
      .then((report) => { if (live) setState({ loading: false, report, error: null }); })
      .catch((error) => { if (live) setState({ loading: false, report: null, error }); });
    return () => { live = false; };
  }, [days, attempt]);
  const r = state.report;
  const empty = r && !r.totals.requests;
  return html`<section class="card uv" id="usage">
    <div class="uv-title-row">
      <h2 class="card-title">${t.usage}</h2>
      <${Segmented} label=${t.usageRange} value=${days} onChange=${setDays} options=${[[7, t.usageRanges[7]], [30, t.usageRanges[30]]]} />
    </div>
    <p class="uv-lead">${t.usageLead}</p>
    ${state.error ? html`<div class="uv-state">${t.usageFailed} <button type="button" class="link-btn" onClick=${() => setAttempt((n) => n + 1)}>${t.retry}</button></div>`
    : !r ? html`<div class="uv-state" aria-busy="true">${t.loading}</div>`
      : html`<div class=${cx('uv-body', state.loading && 'is-loading')}>
        <div class="uv-tiles">
          <${Tile} label=${t.cost} value=${costLabel(r.totals)} />
          <${Tile} label=${t.usageTokens} value=${shortTokens(allTokens(r.totals))} />
          <${Tile} label=${t.usageCacheRate} value=${hitRate(r.totals) === null ? '–' : percent(hitRate(r.totals))} />
          <${Tile} label=${t.usageRequests} value=${INT.format(r.totals.requests)} />
        </div>
        ${empty ? html`<div class="uv-state">${t.usageEmpty}</div>` : html`
          <h3 class="uv-sub">${t.usageByDay}</h3>
          <${DayBars} report=${r} />
          <h3 class="uv-sub">${t.usageByModel}</h3>
          <div class="uv-table" role="table">
            <div class="uv-row uv-headrow" role="row">
              <span role="columnheader">${t.model}</span><span class="uv-num" role="columnheader">${t.usageTokens}</span>
              <span class="uv-num" role="columnheader">${t.cache}</span><span class="uv-num" role="columnheader">${t.cost}</span>
            </div>
            ${r.models.map((m, k) => html`<div class="uv-row" role="row" key=${`${m.provider}/${m.model}`}>
              <span class="uv-name" role="cell"><i class=${`uv-swatch ${k < SERIES ? `uv-s${k + 1}` : 'uv-other'}`} />${m.model}<span class="uv-dim">${m.provider}</span></span>
              <span class="uv-num" role="cell">${t.tokensCount(shortTokens(allTokens(m)))}</span>
              <span class="uv-num" role="cell">${hitRate(m) === null ? '–' : percent(hitRate(m))}</span>
              <span class="uv-num uv-strong" role="cell">${m.priced ? costLabel(m) : t.noPrice}</span>
            </div>`)}
          </div>
          <h3 class="uv-sub">${t.usageTopChats}</h3>
          <div class="uv-table">
            ${r.topChats.map((c) => html`<button type="button" class="uv-row uv-chat" key=${c.id} onClick=${() => openSession(c.id)}>
              <span class="uv-name"><span class="uv-chat-title">${c.title || t.newChat}</span>
                <span class="uv-dim">${[c.scratch ? t.noProject : workspaceName(c.cwd), c.subagents ? t.subagentsN(c.subagents) : ''].filter(Boolean).join(' · ')}</span></span>
              <span class="uv-num">${t.tokensCount(shortTokens(allTokens(c)))}</span>
              <span class="uv-num uv-strong">${costLabel(c)}</span>
            </button>`)}
          </div>`}
        ${r.totals.approximate ? html`<p class="ub-foot">${t.approxNote}</p>` : null}
        ${r.totals.unpricedRequests ? html`<p class="ub-foot">${t.unpricedNote(r.totals.unpricedRequests)}</p>` : null}
      </div>`}
  </section>`;
}
