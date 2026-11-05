// Around the composer: the composer commands (/compact, /view, /status, …),
// a short result line, prompts queued behind the running turn (dsh
// `queue.*`, ⌘↩ steers them all), the MCP servers this chat offers
// ("Active: [Linear ✕]") and the composer's connector picker that chooses
// them per chat (on the start page: for the chat the first prompt creates).
import { useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t } from '../i18n.js';
import { createStore, useStore } from '../store.js';
import { store, api, fail, openPlugins, restoreFocus, setDraft } from '../state.js';
import { IconPencil, IconX, IconArrowUp, IconPlug } from '../icons.js';
import { openGoalForm } from './goal.js';
import { setUiPref } from './worklog.js';
import { VIEW_MODES } from '../worklog.js';
import { Menu } from './common.js';
import { StatusDot } from './plugin-kit.js';
import { useChatMode, useStagedMode } from './modes.js';

// ---- result line -------------------------------------------------------------

/** A quiet line above the composer (command results); `tone` = muted | error. */
export const notice = createStore({ text: null, tone: 'muted', id: 0 });
let noticeTimer = 0;
export function notify(text, tone = 'muted') {
  clearTimeout(noticeTimer);
  notice.set((n) => ({ text, tone, id: n.id + 1 }));
  noticeTimer = setTimeout(() => notice.set({ text: null }), 5000);
}

// ---- commands ----------------------------------------------------------------

/** The commands the composer runs itself; `view` takes a mode. */
export const COMMANDS = ['compact', 'goal', 'model', 'permission', 'status', 'view', 'export'];

/** Opens a menu of the composer seat by its chip (they own their state). */
function openChip(selector) {
  const chip = document.querySelector(selector);
  if (!chip) return false;
  if (chip.getAttribute('aria-expanded') !== 'true') chip.click();
  return true;
}

let compacting = false;
/** `/compact`: condenses older history now (also the context popover's button). */
export async function compactChat() {
  const id = store.get().activeId;
  if (!id || compacting) return;
  compacting = true;
  notify(t.ciCompacting);
  try {
    const r = await api.compact(id);
    notify(r?.compacted ? t.ciCompacted(r.items ?? 0, r.tokens ?? 0) : t.ciNothingToCompact);
  } catch (e) {
    // dsh /compact's texts: busy, or no useful summary (the attempt stays in the log).
    const why = e?.message ?? '';
    notify(/unavailable|not idle/i.test(why) ? t.ciCompactBusy : e?.status ? t.ciCompactFailed : why || String(e), 'error');
  } finally {
    compacting = false;
  }
}

/** `/export`: downloads the session log as JSONL. */
async function exportChat() {
  const id = store.get().activeId;
  if (!id) return;
  try {
    const blob = await api.exportLog(id);
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `dsh-codex-${id}.jsonl`;
    document.body.append(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  } catch (e) { fail(e); }
}

/** Runs a composer command; `arg` is `/view`'s mode. */
export function runCommand(name, arg = '') {
  switch (name) {
    case 'compact': compactChat(); break;
    case 'goal': openGoalForm(); break;
    case 'model': if (!openChip('.seat .chip-model')) notify(t.ciUnavailable, 'error'); break;
    case 'permission': if (!openChip('.seat .chip-access')) notify(t.ciUnavailable, 'error'); break;
    case 'status': if (!openChip('.usage-bar .ub-chip')) notify(t.ciNoStats); break;
    case 'view':
      if (VIEW_MODES.includes(arg)) { setUiPref({ workDetails: arg }); notify(t.ciViewSet(t.workModes[arg] ?? arg)); }
      else notify(t.ciViewUsage(VIEW_MODES.join(' | ')), 'error');
      break;
    case 'export': exportChat(); break;
    default: break;
  }
}

// ---- queued prompts ----------------------------------------------------------

/** Accepted prompts remain visible until the log removes them from their inbox. */
export function pendingPrompts(events) {
  const lists = { 'next-turn': [], 'next-step': [] };
  for (const e of events) {
    if (e.type !== 'agent/inbox/spliced') continue;
    const d = e.data ?? {};
    const list = lists[d.target];
    if (list) list.splice(d.start ?? 0, d.removedCount ?? 0, ...(d.inserted ?? []));
  }
  return ['next-step', 'next-turn'].flatMap((target) => lists[target]
    .filter((m) => m.source?.kind === 'user' || m.source === 'user')
    .map((m) => ({ ...m, target })));
}

/** Only next-turn messages support queue editing and "steer all". */
export const queuedPrompts = (events) => pendingPrompts(events).filter((m) => m.target === 'next-turn');

const textOf = (m) => (m.content ?? []).filter((b) => b.type === 'text').map((b) => b.text).join('\n');

export function useQueue() {
  const events = useStore(store, (s) => s.events);
  const status = useStore(store, (s) => s.status);
  const running = status === 'running' || status === 'waiting-approval';
  const queued = useMemo(() => queuedPrompts(events), [events]);
  return running ? queued : [];
}

async function queueAction(item, body, failure, id = store.get().activeId) {
  if (!id) return false;
  try {
    await api.updateQueue(id, item, body);
    return true;
  } catch {
    notify(failure, 'error');
    return false;
  }
}

/** ⌘↩ on an empty composer: every queued prompt joins the running turn now. */
export async function steerQueued() {
  const s = store.get();
  const running = s.status === 'running' || s.status === 'waiting-approval';
  const queued = running ? queuedPrompts(s.events) : [];
  if (!queued.length) return false;
  for (const m of queued) {
    if (!(await queueAction(m.id, { action: 'steer' }, t.ciQueueSteerFailed, s.activeId))) break;
  }
  return true;
}

function QueueItem({ item, running }) {
  const [edit, setEdit] = useState(null);
  // The prompt may start while it is being edited: the change goes into
  // the composer with a word, not nowhere.
  const editing = useRef(null);
  editing.current = edit;
  useEffect(() => {
    const chat = store.get().activeId;
    return () => {
      const text = editing.current;
      if (text == null || store.get().activeId !== chat) return;
      if (!text.trim() || text.trim() === textOf(item).trim()) return;
      setDraft(text);
      notify(t.ciQueueStartedWhileEditing);
    };
  }, []);
  const text = textOf(item);
  const queued = item.target === 'next-turn';
  const textOnly = (item.content ?? []).every((b) => b.type === 'text');
  const save = async () => {
    const next = edit.trim();
    if (!next) return;
    if (await queueAction(item.id, { action: 'edit', text: next }, t.ciQueueEditFailed)) close();
  };
  // Back to the composer when the edit ends.
  const close = () => { setEdit(null); restoreFocus(null); };
  if (edit !== null) {
    return html`<li class="qp-item is-editing">
      <textarea class="qp-edit" rows="2" value=${edit} aria-label=${t.ciQueueEdit}
        ref=${(el) => { if (el && !el.dataset.focused) { el.dataset.focused = '1'; el.focus(); } }}
        onInput=${(e) => setEdit(e.currentTarget.value)}
        onKeyDown=${(e) => {
    if (e.key === 'Escape') { e.preventDefault(); close(); }
    else if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) { e.preventDefault(); save(); }
  }} />
      <div class="qp-actions">
        <button type="button" class="btn btn-small" onClick=${close}>${t.ciQueueCancel}</button>
        <button type="button" class="btn btn-small btn-primary" disabled=${!edit.trim()} onClick=${save}>${t.ciQueueSave}</button>
      </div>
    </li>`;
  }
  return html`<li class="qp-item">
    <div class="qp-message">
      <span class="qp-text" title=${text}>${text || t.ciQueueAttachment}</span>
      <span class="qp-state">${queued ? t.ciQueueNextTurn : t.ciQueueNextStep}</span>
    </div>
    ${queued ? html`<span class="qp-controls">
    ${running ? html`<button type="button" class="icon-btn qp-btn" aria-label=${t.ciQueueSteer} title=${t.ciQueueSteer}
      onClick=${() => queueAction(item.id, { action: 'steer' }, t.ciQueueSteerFailed)}><${IconArrowUp} size=${13} /></button>` : null}
    <button type="button" class="icon-btn qp-btn" aria-label=${t.ciQueueEdit} title=${textOnly ? t.ciQueueEdit : t.ciQueueEditUnsupported}
      disabled=${!textOnly} onClick=${() => setEdit(text)}><${IconPencil} size=${13} /></button>
    <button type="button" class="icon-btn qp-btn" aria-label=${t.ciQueueRemove} title=${t.ciQueueRemove}
      onClick=${() => queueAction(item.id, { action: 'remove' }, t.ciQueueRemoveFailed)}><${IconX} size=${13} /></button>
    </span>` : null}
  </li>`;
}

function QueuedPrompts() {
  const events = useStore(store, (s) => s.events);
  const status = useStore(store, (s) => s.status);
  const running = status === 'running' || status === 'waiting-approval';
  const pending = useMemo(() => pendingPrompts(events), [events]);
  if (!pending.length) return null;
  const canSteer = running && pending.some((m) => m.target === 'next-turn');
  return html`<section class="qp" aria-label=${t.ciPendingCount(pending.length)}>
    <div class="qp-head"><span>${t.ciPendingCount(pending.length)}</span>${canSteer ? html`<span class="qp-hint">${t.ciQueueHint}</span>` : null}</div>
    <ul class="qp-list">${pending.map((m) => html`<${QueueItem} key=${`${m.target}:${m.id}`} item=${m} running=${running} />`)}</ul>
  </section>`;
}

// ---- MCP servers of the chat ---------------------------------------------------

/**
 * The MCP servers the open chat offers (GET /api/sessions/:id/mcp): `active`
 * names, `servers` the picker rows `{name, selected, preselected, status,
 * retrying, error, tools}` (null until loaded), `minimal` = no MCP at all.
 */
export const chatMcp = createStore({ id: null, active: [], servers: null, minimal: false });

/** Picker rows from the server list (`GET /api/mcp/servers`), for the start page and older servers. */
function rowsOf(list, selected) {
  return (Array.isArray(list) ? list : []).filter((s) => s.enabled !== false).map((s) => {
    const off = new Set(s.disabledTools ?? []);
    return {
      name: s.name, preselected: s.always === true, selected: selected(s), status: s.status ?? 'stopped',
      retrying: !!s.retrying, error: s.error ?? null, tools: (s.tools ?? []).filter((x) => !off.has(x.rawName)).length,
    };
  });
}

async function loadMcp(id) {
  try {
    const r = await api.sessionMcp(id);
    const active = r?.active ?? [];
    let servers = Array.isArray(r?.servers) ? r.servers : null;
    if (!servers && !r?.minimal) servers = rowsOf(await api.mcpServers().catch(() => []), (s) => active.includes(s.name));
    if (store.get().activeId === id) chatMcp.set({ id, active, servers: servers ?? [], minimal: !!r?.minimal });
  } catch { if (store.get().activeId === id) chatMcp.set({ id, active: [], servers: [] }); }
}

/**
 * The start page's choice for the chat its first prompt creates: the server
 * rows (null until loaded) and `picks`, the switches that differ from the
 * preselection (`always`). Applied right after the chat is created.
 */
export const stagedMcp = createStore({ servers: null, picks: {} });

/** Picks that still change something: servers `rows` lists, in another state than `current` (default: the preselection). */
const livePicks = (picks, rows, current = (s) => s.preselected) => Object.fromEntries(Object.entries(picks)
  .filter(([name, on]) => rows.some((s) => s.name === name && current(s) !== on)));

let staging = null;
/**
 * (Re)loads the start page's rows (each visit of the start page, each time
 * the picker opens there); picks for servers that left or that now match
 * the preselection go.
 */
function loadStaged() {
  staging ??= (async () => {
    try {
      const rows = rowsOf(await api.mcpServers(), (s) => s.always === true);
      stagedMcp.set((m) => ({ servers: rows, picks: livePicks(m.picks, rows) }));
    } catch { stagedMcp.set((m) => ({ servers: m.servers ?? [] })); } finally { staging = null; }
  })();
  return staging;
}

const stagedRows = (m) => (m.servers ?? []).map((s) => (s.name in m.picks ? { ...s, selected: m.picks[s.name] } : s));

/**
 * Applies the start page's switches to the chat its first prompt just
 * created: none for Minimal (it has no MCP servers), and only for servers the
 * new chat still lists with another state than the one picked.
 */
export async function applyStagedMcp(id, { preset } = {}) {
  const { picks, servers } = stagedMcp.get();
  stagedMcp.set({ picks: {} });
  if (!Object.keys(picks).length || preset === 'minimal') return;
  // What the new chat offers now, else (older servers) the start page's rows.
  let rows = servers ?? [];
  let current = (s) => s.preselected;
  try {
    const chat = await api.sessionMcp(id);
    if (chat?.minimal) return;
    if (Array.isArray(chat?.servers)) { rows = chat.servers; current = (s) => s.selected; }
  } catch { /* keep the start page's rows */ }
  for (const [name, on] of Object.entries(livePicks(picks, rows, current))) {
    try { await api.setSessionMcp(id, name, on); } catch { notify(t.cnApplyFailed(name), 'error'); }
  }
}

let tracked = { id: null, seq: -1 };
store.subscribe(() => {
  const s = store.get();
  if (s.activeId !== tracked.id) {
    tracked = { id: s.activeId, seq: -1 };
    chatMcp.set({ id: s.activeId, active: [], servers: null, minimal: false });
    if (s.activeId) loadMcp(s.activeId);
    return;
  }
  if (!s.activeId) return;
  const events = s.events;
  const last = events.length ? events[events.length - 1].seq : -1;
  if (last <= tracked.seq) return;
  let changed = false;
  for (let i = events.length - 1; i >= 0 && events[i].seq > tracked.seq; i--) {
    const e = events[i];
    const kind = e.data?.source?.kind;
    if (e.type === 'turn/end' || kind === 'mcp-activation' || kind === 'mcp-deactivation') changed = true;
  }
  tracked.seq = last;
  if (changed) loadMcp(s.activeId);
});

/** Switches `name` on or off for the open chat, else for the chat the start page creates. */
async function setServer(name, on) {
  const id = store.get().activeId;
  if (!id) {
    stagedMcp.set((m) => {
      const picks = { ...m.picks };
      if ((m.servers ?? []).find((s) => s.name === name)?.preselected === on) delete picks[name];
      else picks[name] = on;
      return { picks };
    });
    return;
  }
  chatMcp.set((m) => ({
    active: on ? [...new Set([...m.active, name])].sort() : m.active.filter((x) => x !== name),
    servers: m.servers?.map((s) => (s.name === name ? { ...s, selected: on } : s)) ?? null,
  }));
  try {
    const r = await api.setSessionMcp(id, name, on);
    if (store.get().activeId !== id) return;
    chatMcp.set({ active: r?.active ?? [], ...(Array.isArray(r?.servers) ? { servers: r.servers } : {}) });
  } catch (e) {
    fail(e);
    loadMcp(id);
  }
}

/** Opens a skill's, server's or plugin's details on its tab of the Plugins page. */
export function openDetails(kind, name) {
  store.set({ pluginFocus: { kind, name } });
  openPlugins({ skill: 'skills', server: 'mcp' }[kind] ?? 'plugins');
}

/** The connector rows and selected names of the open chat or of the start page. */
function useConnectors() {
  const id = useStore(store, (s) => s.activeId);
  const chat = useStore(chatMcp, (m) => m);
  const staged = useStore(stagedMcp, (m) => m);
  const chatMode = useChatMode();
  const stagedMode = useStagedMode();
  // Every visit of the start page: servers or their preselection may have changed.
  useEffect(() => { if (!id) loadStaged(); }, [id]);
  return useMemo(() => {
    if (id) {
      const minimal = chat.minimal || chatMode === 'minimal';
      return { id, minimal, rows: chat.servers, active: minimal ? [] : chat.active };
    }
    const minimal = stagedMode === 'minimal';
    const rows = staged.servers == null ? null : stagedRows(staged);
    return { id, minimal, rows, active: minimal || !rows ? [] : rows.filter((s) => s.selected).map((s) => s.name) };
  }, [id, chat, staged, chatMode, stagedMode]);
}

/** The selected servers as chips above the composer, each with a ✕ that switches it off. */
function ActiveServers() {
  const { active } = useConnectors();
  if (!active.length) return null;
  return html`<div class="as-row" role="group" aria-label=${t.ciActive}>
    <span class="as-label">${t.ciActive}</span>
    ${active.map((name) => html`<span class="as-chip" key=${name}>
      <button type="button" class="as-name" title=${t.ciServerDetails(name)} onClick=${() => openDetails('server', name)}>
        <span class="as-dot" aria-hidden="true" />${name}
      </button>
      <button type="button" class="as-x" aria-label=${t.ciDeactivate(name)} title=${t.ciDeactivate(name)} onClick=${() => setServer(name, false)}>
        <${IconX} size=${11} />
      </button>
    </span>`)}
  </div>`;
}

/** Arrow keys, Home and End move between the picker's rows. */
function rowKeys(e) {
  const keys = { ArrowDown: 1, ArrowUp: -1, Home: -Infinity, End: Infinity };
  if (!(e.key in keys)) return;
  const rows = [...e.currentTarget.querySelectorAll('.cn-row')];
  if (!rows.length) return;
  e.preventDefault();
  const at = rows.indexOf(document.activeElement);
  const step = keys[e.key];
  const next = Number.isFinite(step) ? (at + step + rows.length) % rows.length : step < 0 ? 0 : rows.length - 1;
  rows[next]?.focus();
}

function ConnectorRow({ row }) {
  const connected = row.status === 'connected';
  return html`<button type="button" role="menuitemcheckbox" aria-checked=${!!row.selected} class="cn-row"
    title=${row.error && !connected ? row.error : ''} onClick=${() => setServer(row.name, !row.selected)}>
    <span class="cn-text">
      <span class="cn-name"><span class="cn-label">${row.name}</span>
        ${row.preselected ? html`<span class="cn-tag" title=${t.cnPreselectedHint}>${t.cnPreselected}</span>` : null}</span>
      <span class="cn-meta"><${StatusDot} status=${row.status} retrying=${row.retrying} />
        ${connected ? html`<span class="cn-sep" aria-hidden="true">·</span><span>${t.mcpToolsN(row.tools ?? 0)}</span>` : null}</span>
    </span>
    <span class=${cx('toggle', 'cn-switch', row.selected && 'is-on')} aria-hidden="true"><span class="toggle-knob" /></span>
  </button>`;
}

/** "Connectors" in the composer's toolbar: which MCP servers this chat (or the next new one) uses. */
export function ConnectorsButton() {
  const { id, minimal, rows, active } = useConnectors();
  const [open, setOpen] = useState(false);
  const anchor = useRef(null);
  const list = useRef(null);
  const toggle = () => {
    if (minimal) return;
    if (!open) { if (id) loadMcp(id); else loadStaged(); }
    setOpen((v) => !v);
  };
  // Once the rows are there: keep the popover above its button (it grew) and
  // focus the first row, so the keyboard can start right away.
  useEffect(() => {
    const menu = list.current?.closest('.menu');
    const a = anchor.current?.getBoundingClientRect();
    if (!open || !menu || !a) return;
    const box = menu.getBoundingClientRect();
    let top = a.top - box.height - 6;
    if (top < 8) top = Math.max(8, Math.min(innerHeight - box.height - 8, a.bottom + 6));
    menu.style.top = `${top}px`;
    const el = list.current.querySelector('.cn-row');
    if (el && !list.current.contains(document.activeElement)) el.focus();
  }, [open, rows]);
  useEffect(() => { if (minimal) setOpen(false); }, [minimal]);
  const count = active.length;
  const label = minimal ? t.cnMinimal : count ? t.cnButtonCount(count) : t.cnButton;
  return html`<span class="chip-wrap cn-wrap">
    <button type="button" ref=${anchor} class=${cx('chip', 'cn-btn', count && 'has-count', minimal && 'is-disabled')} aria-haspopup="menu"
      aria-expanded=${open} aria-label=${label} title=${label} aria-disabled=${minimal ? 'true' : null} onClick=${toggle}>
      <${IconPlug} size=${15} /><span class="chip-label cn-btn-label">${t.cnButton}</span>
      ${count && !minimal ? html`<span class="cn-count" aria-hidden="true">${count}</span>` : null}
    </button>
    <${Menu} open=${open} onClose=${() => setOpen(false)} anchor=${anchor} side="top" align="start" fixed class="cn-menu">
      <div class="menu-head cn-head">${id ? t.cnTitleChat : t.cnTitleNew}</div>
      <div class="cn-list" ref=${list} onKeyDown=${rowKeys}>
        ${rows == null ? html`<div class="cn-empty">${t.loading}</div>`
    : rows.length ? rows.map((row) => html`<${ConnectorRow} key=${row.name} row=${row} />`)
      : html`<div class="cn-empty">${t.cnEmpty}</div>`}
      </div>
      <div class="menu-sep" />
      <p class="cn-foot">${t.cnFoot}</p>
      <button type="button" class="menu-item menu-action cn-manage" onClick=${() => { setOpen(false); openPlugins('mcp'); }}>
        <span class="menu-icon"><${IconPlug} size=${14} /></span><span class="menu-label">${t.cnManage}</span>
      </button>
    </${Menu}>
  </span>`;
}

function Notice() {
  const n = useStore(notice, (x) => x);
  if (!n.text) return null;
  return html`<div class=${cx('ci-notice', n.tone === 'error' && 'is-error')} role="status" key=${n.id}>${n.text}</div>`;
}

/** Everything that sits on top of the composer card (not on the start page's tray). */
export function ComposerExtras() {
  useEffect(() => () => clearTimeout(noticeTimer), []);
  return html`<${Notice} /><${QueuedPrompts} /><${ActiveServers} />`;
}
