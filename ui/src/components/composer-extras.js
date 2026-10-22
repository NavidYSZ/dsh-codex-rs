// Around the composer: the composer commands (/compact, /view, /status, …),
// a short result line, prompts queued behind the running turn (dsh
// `queue.*`, ⌘↩ steers them all) and the MCP servers this chat activated
// ("Active: [Linear ✕]").
import { useEffect, useMemo, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t } from '../i18n.js';
import { createStore, useStore } from '../store.js';
import { store, api, fail, openPlugins, restoreFocus } from '../state.js';
import { IconPencil, IconX, IconArrowUp } from '../icons.js';
import { openGoalForm } from './goal.js';
import { setUiPref } from './worklog.js';
import { VIEW_MODES } from '../worklog.js';

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

/** Prompts waiting for the next turn, folded from the inbox splices of the log. */
export function queuedPrompts(events) {
  const lists = { 'next-turn': [], 'next-step': [] };
  for (const e of events) {
    if (e.type !== 'agent/inbox/spliced') continue;
    const d = e.data ?? {};
    const list = lists[d.target];
    if (list) list.splice(d.start ?? 0, d.removedCount ?? 0, ...(d.inserted ?? []));
  }
  return lists['next-turn'].filter((m) => m.source?.kind === 'user' || m.source === 'user');
}

const textOf = (m) => (m.content ?? []).filter((b) => b.type === 'text').map((b) => b.text).join('\n');

export function useQueue() {
  const events = useStore(store, (s) => s.events);
  const status = useStore(store, (s) => s.status);
  const running = status === 'running' || status === 'waiting-approval';
  const queued = useMemo(() => queuedPrompts(events), [events]);
  return running ? queued : [];
}

async function queueAction(item, body, failure) {
  const id = store.get().activeId;
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
    if (!(await queueAction(m.id, { action: 'steer' }, t.ciQueueSteerFailed))) break;
  }
  return true;
}

function QueueItem({ item }) {
  const [edit, setEdit] = useState(null);
  const text = textOf(item);
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
    <span class="qp-text" title=${text}>${text || t.ciQueueAttachment}</span>
    <button type="button" class="icon-btn qp-btn" aria-label=${t.ciQueueSteer} title=${t.ciQueueSteer}
      onClick=${() => queueAction(item.id, { action: 'steer' }, t.ciQueueSteerFailed)}><${IconArrowUp} size=${13} /></button>
    <button type="button" class="icon-btn qp-btn" aria-label=${t.ciQueueEdit} title=${textOnly ? t.ciQueueEdit : t.ciQueueEditUnsupported}
      disabled=${!textOnly} onClick=${() => setEdit(text)}><${IconPencil} size=${13} /></button>
    <button type="button" class="icon-btn qp-btn" aria-label=${t.ciQueueRemove} title=${t.ciQueueRemove}
      onClick=${() => queueAction(item.id, { action: 'remove' }, t.ciQueueRemoveFailed)}><${IconX} size=${13} /></button>
  </li>`;
}

function QueuedPrompts() {
  const queued = useQueue();
  if (!queued.length) return null;
  return html`<section class="qp" aria-label=${t.ciQueueCount(queued.length)}>
    <div class="qp-head"><span>${t.ciQueueCount(queued.length)}</span><span class="qp-hint">${t.ciQueueHint}</span></div>
    <ul class="qp-list">${queued.map((m) => html`<${QueueItem} key=${m.id} item=${m} />`)}</ul>
  </section>`;
}

// ---- active MCP servers --------------------------------------------------------

/** The MCP servers the open chat offers (GET /api/sessions/:id/mcp). */
export const chatMcp = createStore({ id: null, active: [] });

async function loadMcp(id) {
  try {
    const r = await api.sessionMcp(id);
    if (store.get().activeId === id) chatMcp.set({ id, active: r?.active ?? [] });
  } catch { if (store.get().activeId === id) chatMcp.set({ id, active: [] }); }
}

let tracked = { id: null, seq: -1 };
store.subscribe(() => {
  const s = store.get();
  if (s.activeId !== tracked.id) {
    tracked = { id: s.activeId, seq: -1 };
    chatMcp.set({ id: s.activeId, active: [] });
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

async function deactivate(server) {
  const id = store.get().activeId;
  if (!id) return;
  chatMcp.set((m) => ({ active: m.active.filter((x) => x !== server) }));
  try {
    const r = await api.setSessionMcp(id, server, false);
    chatMcp.set({ active: r?.active ?? [] });
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

function ActiveServers() {
  const active = useStore(chatMcp, (m) => m.active);
  const id = useStore(store, (s) => s.activeId);
  if (!id || !active.length) return null;
  return html`<div class="as-row" role="group" aria-label=${t.ciActive}>
    <span class="as-label">${t.ciActive}</span>
    ${active.map((name) => html`<span class="as-chip" key=${name}>
      <button type="button" class="as-name" title=${t.ciServerDetails(name)} onClick=${() => openDetails('server', name)}>
        <span class="as-dot" aria-hidden="true" />${name}
      </button>
      <button type="button" class="as-x" aria-label=${t.ciDeactivate(name)} title=${t.ciDeactivate(name)} onClick=${() => deactivate(name)}>
        <${IconX} size=${11} />
      </button>
    </span>`)}
  </div>`;
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
