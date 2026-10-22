// The status line that stands for a run of work steps (dsh
// ProcessGroupHeader), one-line reasoning (dsh ReasoningRow), the line a
// finished turn folds into (dsh TurnProcessNodeView) and Settings → Appearance.
import { useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t, duration } from '../i18n.js';
import { useStore } from '../store.js';
import { store, updateSettings } from '../state.js';
import {
  VIEW_MODES, policyOf, activityOf, activityLabel, processTitle, liveToolDetail, reasoningDetail,
  stripBold, firstLine, latestCompletedParagraphFirstLine,
} from '../worklog.js';
import {
  IconFile, IconImage, IconSearch, IconPencil, IconTerminal, IconGlobe, IconList, IconWindow, IconPointer, IconPlug, IconTool,
  IconChevronRight, IconChevronDown,
} from '../icons.js';
import { Shimmer, MenuItem } from './common.js';
import { ToolItem } from './tools.js';
import { Markdown } from './transcript.js';
import { Field, Segmented } from './settings.js';
import { TurnCost, turnDuration } from './usage.js';

// ---- preferences ---------------------------------------------------------------

export const useWorkPolicy = () => policyOf(useStore(store, (s) => s.info?.ui?.workDetails));

/** Edits show their diff in the transcript only in the detailed views. */
export function diffsOpen() {
  const mode = store.get().info?.ui?.workDetails;
  return mode === 'detailed' || mode === 'verbose';
}

/** Saves `{workDetails?, performanceUsage?}` for every browser (ui-state.json); the view switches at once. */
export async function setUiPref(patch) {
  const before = store.get().info;
  if (before) store.set({ info: { ...before, ui: { ...(before.ui ?? {}), ...patch } } });
  if (!(await updateSettings({ ui: patch })) && before) store.set({ info: before });
}

// ---- icons -----------------------------------------------------------------------

const S = { fill: 'none', stroke: 'currentColor', 'stroke-width': 1.3, 'stroke-linecap': 'round', 'stroke-linejoin': 'round' };
const glyph = (body) => ({ size = 14 }) => html`<svg class="icon" width=${size} height=${size} viewBox="0 0 16 16" aria-hidden="true">${body}</svg>`;
export const IconThink = glyph(html`<path d="M5.6 12.25h4.8M6.4 14.25h3.2M8 1.75a4.25 4.25 0 0 0-2.55 7.65c.5.38.8.93.8 1.55v.3h3.5v-.3c0-.62.3-1.17.8-1.55A4.25 4.25 0 0 0 8 1.75Z" ...${S}/>`);
const IconPeople = glyph(html`<circle cx="5.75" cy="5.5" r="2.25" ...${S}/><path d="M1.75 13.25c.3-2.2 1.9-3.5 4-3.5s3.7 1.3 4 3.5M10.5 3.6a2.1 2.1 0 1 1 0 4M11.6 9.9c1.45.35 2.4 1.5 2.65 3.35" ...${S}/>`);
const IconQuestion = glyph(html`<circle cx="8" cy="8" r="6.25" ...${S}/><path d="M6.2 6.2a1.85 1.85 0 1 1 2.6 1.7c-.5.25-.8.6-.8 1.15v.35" ...${S}/><circle cx="8" cy="11.4" r=".75" fill="currentColor"/>`);
const IconBook = glyph(html`<path d="M2.75 3.25c1.9-.6 3.65-.4 5.25.75v9.25c-1.6-1.15-3.35-1.35-5.25-.75V3.25ZM13.25 3.25c-1.9-.6-3.65-.4-5.25.75v9.25c1.6-1.15 3.35-1.35 5.25-.75V3.25Z" ...${S}/>`);

const ICONS = {
  thinking: IconThink, read: IconFile, readImage: IconImage, search: IconSearch, edit: IconPencil, write: IconPencil,
  commands: IconTerminal, code: IconTerminal, webSearch: IconGlobe, webFetch: IconGlobe, subagents: IconPeople,
  plan: IconList, questions: IconQuestion, browser: IconWindow, mac: IconPointer, skill: IconBook, mcp: IconPlug, tools: IconTool,
};
const iconOf = (kind) => ICONS[kind.startsWith('mcp:') ? 'mcp' : kind] ?? IconTool;

// ---- reasoning -------------------------------------------------------------------

/**
 * Reasoning as one line: while it streams, the first line of the latest
 * finished paragraph (`**` dropped), afterwards its first line; a click shows
 * the whole text.
 */
export function ReasoningLine({ text, ms, live, preview = true }) {
  const [open, setOpen] = useState(false);
  const summary = useMemo(() => stripBold(live ? latestCompletedParagraphFirstLine(text) : firstLine(text)).trim(), [text, live]);
  const label = live ? t.thinking : ms != null && ms >= 500 ? t.thoughtFor(duration(ms)) : t.thought;
  const shown = !open && summary && (live || preview);
  return html`<div class=${cx('reasoning', 'wl-reasoning', open && 'is-open')}>
    <button type="button" class="row is-expandable" onClick=${() => setOpen((v) => !v)} aria-expanded=${open}>
      <span class="lead"><${IconThink} /></span>
      <${Shimmer} active=${live}><span class="row-title">${label}</span></${Shimmer}>
      ${shown ? html`<span class="sep" /><span class=${cx('row-summary', 'wl-preview', live && 'is-live')}>${summary}</span>` : null}
      <span class="chev"><${IconChevronRight} size=${12} /></span>
    </button>
    ${open ? html`<div class="reasoning-body"><${Markdown} text=${text} /></div>` : null}
  </div>`;
}

// ---- the status line ----------------------------------------------------------------

const MINIMUM_MS = 150; // dsh PROCESS_TITLE_MINIMUM_MS: a live title stays this long before the next

const same = (a, b) => a.activity === b.activity && a.detail === b.detail && a.preparing === b.preparing && a.waiting === b.waiting;

/** dsh `useStableLiveProcessTitle`: no flicker when steps change quickly. */
function useStableTitle(desired, active) {
  const [shown, setShown] = useState(desired);
  const shownRef = useRef(shown);
  const wanted = useRef(desired);
  const since = useRef(Date.now());
  useEffect(() => {
    wanted.current = desired;
    if (!active || same(shownRef.current, desired)) return undefined;
    const commit = () => {
      shownRef.current = wanted.current;
      since.current = Date.now();
      setShown(wanted.current);
    };
    const left = MINIMUM_MS - (Date.now() - since.current);
    if (left <= 0) { commit(); return undefined; }
    const id = setTimeout(commit, left);
    return () => clearTimeout(id);
  }, [active, desired.activity, desired.detail, desired.preparing, desired.waiting]);
  return active ? shown : desired;
}

/** What an open group works on right now (dsh `processActivity` for the live part). */
function liveActivity(group, cwd) {
  if (group.prep) return { activity: activityOf(group.prep.name), detail: '', preparing: true, waiting: false };
  let running = null;
  for (const x of group.items) {
    if (x.kind === 'tool' && !x.result && (!running || x.time >= running.time)) running = x;
  }
  if (running) {
    const waiting = !!running.approval && !running.approval.outcome;
    return { activity: waiting ? 'questions' : activityOf(running.name), detail: liveToolDetail(running, cwd), preparing: false, waiting };
  }
  const live = group.live?.length ? group.live[group.live.length - 1].text : '';
  return { activity: 'thinking', detail: live ? reasoningDetail(live) : '', preparing: false, waiting: false };
}

/** The header of a work group: icon, activity, " · " what it works on; a click shows the steps. */
function GroupHead({ group, closed, policy, cwd, open, onToggle }) {
  const desired = closed ? { activity: 'thinking', detail: '', preparing: false, waiting: false } : liveActivity(group, cwd);
  const live = useStableTitle(desired, !closed);
  let label;
  if (closed) label = processTitle(group.counts);
  else if (live.waiting) label = t.waitingYou;
  else if (live.preparing) label = activityLabel(t.workPrepare, live.activity === 'thinking' ? 'tools' : live.activity);
  else label = activityLabel(t.workLive, live.activity);
  const detail = !closed && policy.liveProcessDetail ? live.detail : '';
  const Icon = iconOf(closed ? group.counts[0]?.kind ?? 'thinking' : live.activity);
  const empty = !group.items.length && !group.live?.length;
  return html`<button type="button" class=${cx('row', 'wl-head', !empty && 'is-expandable')} onClick=${empty ? undefined : onToggle}
      aria-expanded=${empty ? undefined : open} tabindex=${empty ? -1 : 0}>
    <span class="lead"><${Icon} /></span>
    <${Shimmer} active=${!closed}><span class="row-title wl-title">${detail ? `${label} · ${detail}` : label}</span></${Shimmer}>
    ${empty ? null : html`<span class="chev"><${IconChevronRight} size=${12} /></span>`}
  </button>`;
}

/** One item inside a group or in a flat view. */
export function WorkItem({ item, cwd, preview }) {
  if (item.kind === 'tool') return html`<${ToolItem} item=${item} cwd=${cwd} />`;
  return html`<${ReasoningLine} text=${item.text} ms=${item.ms} live=${!!item.live} preview=${preview} />`;
}

/**
 * A run of reasoning and tool steps behind one status line. Pending
 * approvals keep it open (dsh `alwaysOpen`).
 */
export function WorkGroup({ group, closed, policy, cwd }) {
  const [open, setOpen] = useState(false);
  const waiting = group.items.some((x) => x.kind === 'tool' && x.approval && !x.approval.outcome);
  const expanded = open || waiting;
  return html`<div class=${cx('wl-group', expanded && 'is-open', !closed && 'is-live')}>
    <${GroupHead} group=${group} closed=${closed} policy=${policy} cwd=${cwd} open=${expanded} onToggle=${() => setOpen((v) => !v)} />
    ${expanded ? html`<div class="wl-body">
      ${group.items.map((x) => html`<${WorkItem} key=${x.key} item=${x} cwd=${cwd} preview=${policy.settledReasoningPreview} />`)}
      ${(group.live ?? []).map((b) => html`<${ReasoningLine} key=${b.key} text=${b.text} ms=${b.ms} live=${b.live} />`)}
    </div>` : null}
  </div>`;
}

/** "Analyzing the request" before anything streams (flat views keep "Thinking"). */
export function ThinkingRow({ policy }) {
  if (policy.stepGrouping === 'none') {
    return html`<div class="row thinking"><${Shimmer} active><span class="row-title">${t.thinking}</span></${Shimmer}></div>`;
  }
  return html`<div class="wl-group is-live"><div class="row wl-head">
    <span class="lead"><${IconThink} /></span>
    <${Shimmer} active><span class="row-title wl-title">${t.workLive.thinking}</span></${Shimmer}>
  </div></div>`;
}

// ---- the finished turn ----------------------------------------------------------------

/** "Completed in 1m 20s · 12 tool calls · 3 messages · $0.0031"; a click folds the turn. */
export function TurnLine({ turn, folded, foldable, onToggle }) {
  const work = turn.work;
  const status = turn.status;
  let label;
  if (status === 'aborted') label = t.stopped;
  else if (status === 'error') label = t.turnFailedShort;
  else if (turn.start != null && turn.end != null) label = t.turnTook(turnDuration(Math.max(1000, turn.end - turn.start)));
  else label = t.turnWorked;
  const parts = [label];
  if (work.toolCalls) parts.push(t.toolCalls(work.toolCalls));
  if (work.messages) parts.push(t.messagesN(work.messages));
  return html`<div class=${cx('wl-turn', folded && 'is-folded')}>
    <button type="button" class="wl-turn-btn" disabled=${!foldable} aria-expanded=${foldable ? !folded : undefined} onClick=${onToggle}>
      <span class="wl-turn-label">${parts.join(' · ')}</span>
      ${foldable ? html`<span class="wl-turn-chev"><${IconChevronDown} size=${14} /></span>` : null}
    </button>
    <${TurnCost} turn=${turn.turn} />
  </div>`;
}

// ---- menus and settings ----------------------------------------------------------------

/** Quick switch in the chat's "…" menu. */
export function ViewModeItems() {
  const mode = useStore(store, (s) => s.info?.ui?.workDetails ?? 'standard');
  return html`<div class="menu-sep" /><div class="menu-head">${t.workDetails}</div>
    ${VIEW_MODES.map((m) => html`<${MenuItem} key=${m} checked=${m === mode} onClick=${() => setUiPref({ workDetails: m })}>${t.workModes[m]}</${MenuItem}>`)}`;
}

/** Settings → Appearance: work details and the performance & usage bar. */
export function AppearanceCard() {
  const ui = useStore(store, (s) => s.info?.ui);
  const mode = ui?.workDetails ?? 'standard';
  const perf = ui?.performanceUsage ?? 'detailed';
  return html`<section class="card">
    <h2 class="card-title">${t.appearance}</h2>
    <${Field} label=${t.workDetails} hint=${t.workModeHints[mode] ?? t.workDetailsHint}>
      <${Segmented} label=${t.workDetails} value=${mode} onChange=${(v) => setUiPref({ workDetails: v })}
        options=${VIEW_MODES.map((m) => [m, t.workModes[m]])} />
    </${Field}>
    <${Field} label=${t.perfUsage} hint=${t.perfUsageHint}>
      <${Segmented} label=${t.perfUsage} value=${perf} onChange=${(v) => setUiPref({ performanceUsage: v })}
        options=${[['compact', t.perfModes.compact], ['detailed', t.perfModes.detailed]]} />
    </${Field}>
  </section>`;
}
