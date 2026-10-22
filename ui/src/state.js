// App state and the actions that change it. Components read with useStore()
// and call these functions; all server traffic goes through `api`.
import { createApi } from './api.js';
import { createStore } from './store.js';
import { t } from './i18n.js';

export const api = createApi();
const params = new URLSearchParams(location.search);

function readLocal(key, fallback) {
  try { return localStorage.getItem(key) ?? fallback; } catch { return fallback; }
}
function writeLocal(key, value) {
  try { localStorage.setItem(key, value); } catch { /* storage blocked */ }
}
function readJson(key, fallback) {
  try { return JSON.parse(readLocal(key, '')) ?? fallback; } catch { return fallback; }
}

/** Last project picked on the start page; '' means a chat without a project. */
const PROJECT_KEY = 'dshx-project';

export const store = createStore({
  boot: 'loading',
  bootError: null,
  info: null,
  workspaces: [],
  sessions: [],
  cwd: '',
  view: 'chat',
  // Pane of Settings and tab of Plugins from `#/settings/<section>` and `#/plugins/<tab>` (null: the default).
  settingsSection: null,
  pluginsTab: null,
  activeId: null,
  header: null,
  events: [],
  status: 'idle',
  statusError: null,
  deltas: null,
  approval: null,
  pending: null,
  composer: {},
  conn: 'ok',
  toast: null,
  theme: params.get('theme') || readLocal('dshx-theme', 'system'),
  sidebarOpen: false,
  sidebarCollapsed: readLocal('dshx-sidebar', '') === 'collapsed',
  // 'projects' groups chats by folder; 'activity' (the bell) lists all by recency.
  sidebarMode: params.get('sidebar') || readLocal('dshx-sidebar-mode', 'projects'),
  activityFilter: 'all',
  folded: readJson('dshx-folded', []),
  palette: false,
  renaming: null,
  desktop: readDesktop(),
  // Right side panel: 'changes', 'files', 'live' or 'file:<path relative to the chat folder>'.
  panel: { open: readLocal('dshx-panel', '') === 'open', tab: 'changes', files: [] },
  panelWidth: Number(readLocal('dshx-panel-width', '0')) || 0,
  // Message sent while a turn runs: 'steer' joins it, 'queue' waits for it.
  followup: readLocal('dshx-followup', 'steer'),
  // Automations: every scheduled task (sidebar clock marks, Automations page).
  schedules: [],
  // Text the composer takes over once (e.g. "New automation").
  draft: null,
  notify: readLocal('dshx-notify', '') === 'on',
});

/** The Mac app's bridge (instances, notifications); null in a plain browser. */
function readDesktop() {
  const d = window.dshxDesktop;
  if (!d) return null;
  return { instances: [...(d.instances ?? [])], current: d.current ?? null, version: d.version ?? '' };
}
window.addEventListener('dshx-desktop-change', () => store.set({ desktop: readDesktop() }));
// Menu commands and notification clicks of the Mac app.
window.addEventListener('dshx-desktop-command', (e) => {
  const command = e.detail?.command;
  if (command === 'new-chat') { e.preventDefault(); newChat(); }
  else if (command === 'settings') { e.preventDefault(); openSettings(); }
});
window.addEventListener('dshx-desktop-notification', (e) => {
  if (e.detail?.tag?.startsWith('session-')) openSession(e.detail.tag);
});

// ---- theme ---------------------------------------------------------------

const dark = window.matchMedia?.('(prefers-color-scheme: dark)');
export function applyTheme() {
  const pref = store.get().theme;
  const resolved = pref === 'system' ? (dark?.matches ? 'dark' : 'light') : pref;
  document.documentElement.dataset.theme = resolved;
}
dark?.addEventListener?.('change', applyTheme);

export function setTheme(theme) {
  writeLocal('dshx-theme', theme);
  store.set({ theme });
  applyTheme();
}

// ---- boot and routing ----------------------------------------------------

export async function boot() {
  applyTheme();
  try {
    const [info, workspaces, sessions] = await Promise.all([api.info(), api.workspaces(), api.sessions()]);
    const recent = [...(workspaces ?? [])].sort((a, b) => (b.lastUsed ?? 0) - (a.lastUsed ?? 0));
    const picked = readLocal(PROJECT_KEY, null);
    const cwd = picked === '' || recent.some((w) => w.path === picked) ? picked : recent[0]?.path ?? '';
    store.set({ boot: 'ready', info, workspaces: workspaces ?? [], sessions: sessions ?? [], cwd });
    loadSchedules();
  } catch (e) {
    store.set({ boot: 'error', bootError: e });
    return;
  }
  window.addEventListener('hashchange', route);
  window.addEventListener('keydown', onShortcut);
  document.addEventListener('visibilitychange', () => { if (visible()) markSeen(store.get().activeId); });
  watchAll();
  if (!location.hash && api.fixture) {
    const fx = await api.ready.then(() => api.fx);
    if (fx.session) return openSession(fx.session.header.id, { replace: true });
    if (fx.cwd) store.set({ cwd: fx.cwd });
  }
  route();
}

function route() {
  const h = location.hash;
  const page = /^#\/(settings|plugins)(?:\/([\w-]+))?$/.exec(h);
  if (h.startsWith('#/s/')) openSession(decodeURIComponent(h.slice(4)));
  else if (page?.[1] === 'settings') store.set({ view: 'settings', settingsSection: page[2] ?? null, sidebarOpen: false });
  else if (page?.[1] === 'plugins') store.set({ view: 'plugins', pluginsTab: page[2] ?? null, sidebarOpen: false });
  else if (h === '#/automations') store.set({ view: 'automations', sidebarOpen: false });
  else showNewChat();
}

function setHash(hash, replace) {
  if (location.hash === hash || (!hash && !location.hash)) return;
  const url = location.pathname + location.search + hash;
  if (replace) history.replaceState(null, '', url);
  else history.pushState(null, '', url);
}

// ---- sessions ------------------------------------------------------------

let closeStream = null;
let openToken = 0;

const EMPTY_SESSION = {
  header: null, events: [], status: 'idle', statusError: null, deltas: null, approval: null, live: null, liveHidden: null,
  goal: null, team: null, goalForm: false,
};

/** Another chat: file tabs belong to the old one. */
const freshPanel = (p) => ({ ...p, files: [], tab: p.tab.startsWith('file:') ? 'changes' : p.tab });

function showNewChat() {
  closeStream?.();
  closeStream = null;
  openToken++;
  store.set((s) => ({ ...EMPTY_SESSION, activeId: null, view: 'chat', pending: null, sidebarOpen: false, composer: {}, panel: freshPanel(s.panel) }));
}

/** Start page; with a `cwd` ('' = no project) the new chat goes there. */
export function newChat(cwd) {
  if (typeof cwd === 'string') pickWorkspace(cwd);
  setHash('');
  showNewChat();
}

/**
 * Settings, at `section` (`models`, `usage`, …) or its first pane. Click
 * handlers may pass an event. `replace` switches panes without a history entry.
 */
export function openSettings(section, { replace = false } = {}) {
  const pane = typeof section === 'string' ? section : null;
  setHash(pane ? `#/settings/${pane}` : '#/settings', replace);
  store.set({ view: 'settings', settingsSection: pane, sidebarOpen: false });
}

/** Plugins, on `tab` (`skills`, `mcp`, …) or the tab used last. */
export function openPlugins(tab, { replace = false } = {}) {
  const pick = typeof tab === 'string' ? tab : null;
  setHash(pick ? `#/plugins/${pick}` : '#/plugins', replace);
  store.set({ view: 'plugins', pluginsTab: pick, sidebarOpen: false });
}

export function openAutomations() {
  setHash('#/automations');
  store.set({ view: 'automations', sidebarOpen: false });
  loadSchedules();
}

/** Back from Settings, Plugins or Automations to the chat that was open. */
export function closeSettings() {
  const id = store.get().activeId;
  setHash(id ? `#/s/${encodeURIComponent(id)}` : '');
  store.set({ view: 'chat' });
}

/** Loads a session and follows its live stream. `fresh` keeps the optimistic prompt. */
export async function openSession(id, { fresh = false, replace = false } = {}) {
  const s = store.get();
  setHash(`#/s/${encodeURIComponent(id)}`, replace);
  if (s.activeId === id) {
    store.set({ view: 'chat', sidebarOpen: false });
    return;
  }
  closeStream?.();
  closeStream = null;
  const my = ++openToken;
  store.set({
    ...EMPTY_SESSION, activeId: id, view: 'chat', sidebarOpen: false,
    pending: fresh ? s.pending : null, composer: fresh ? s.composer : {}, panel: freshPanel(s.panel),
  });
  try {
    const data = await api.session(id);
    if (my !== openToken) return;
    store.set({ header: data.header, events: data.events ?? [], status: data.status ?? 'idle' });
    markSeen(id);
    loadGoal(id);
    loadTeam(id);
    const events = data.events ?? [];
    const after = events.length ? events[events.length - 1].seq : -1;
    closeStream = api.stream(id, after, {
      message: (m) => onMessage(id, m),
      state: (st) => store.set({ conn: st === 'open' ? 'ok' : 'retry' }),
    });
  } catch (e) {
    if (my !== openToken) return;
    fail(e);
    if (e?.status === 404) newChat();
  }
}

// Deltas arrive many times per frame; fold them once per animation frame.
let deltaQueue = [];
let deltaFrame = 0;

function onMessage(id, m) {
  if (m.sessionId && m.sessionId !== id) return;
  if (store.get().activeId !== id) return;
  switch (m.kind) {
    case 'event': flushDeltas(); appendEvent(m.event); break;
    case 'delta':
      deltaQueue.push(m);
      deltaFrame ||= requestAnimationFrame(flushDeltas);
      break;
    case 'status': {
      flushDeltas();
      const patch = { status: m.status, statusError: m.status === 'error' ? m.error || null : null };
      if (m.status !== 'waiting-approval') patch.approval = null;
      if (m.status !== 'running') patch.deltas = null;
      store.set(patch);
      store.set((st) => ({ sessions: st.sessions.map((x) => (x.id === id ? { ...x, status: m.status } : x)) }));
      if (m.status === 'idle' || m.status === 'error') {
        if (visible()) markSeen(id);
        refreshSessions();
      }
      break;
    }
    case 'approval': store.set({ approval: m }); break;
    case 'live': onLive(m); break;
    case 'goal': store.set({ goal: m.goal ?? null }); break;
    default: break;
  }
}

function flushDeltas() {
  cancelAnimationFrame(deltaFrame);
  deltaFrame = 0;
  if (!deltaQueue.length) return;
  const queue = deltaQueue;
  deltaQueue = [];
  store.set((s) => {
    let cur = s.deltas;
    for (const m of queue) {
      const key = `${m.turn}:${m.step}`;
      if (!cur || cur.key !== key) cur = { key, turn: m.turn, step: m.step, blocks: {} };
      const prev = cur.blocks[m.index];
      cur = {
        ...cur,
        blocks: { ...cur.blocks, [m.index]: { type: m.blockType, text: (prev?.text ?? '') + m.text, start: prev?.start ?? Date.now() } },
      };
    }
    return { deltas: cur };
  });
}

function appendEvent(ev) {
  if (!ev || typeof ev.seq !== 'number') return;
  store.set((s) => {
    const last = s.events.length ? s.events[s.events.length - 1].seq : -1;
    if (ev.seq <= last) return null;
    const patch = { events: [...s.events, ev] };
    const d = ev.data ?? {};
    if ((ev.type === 'assistant/message' || ev.type === 'assistant/attempt') && s.deltas?.key === `${d.turn}:${d.step}`) patch.deltas = null;
    if (ev.type === 'turn/end') patch.deltas = null;
    if (ev.type === 'user/message' && d.source?.kind === 'user') patch.pending = null;
    if (ev.type === 'approval/decided' && s.approval) patch.approval = null;
    if (ev.type.startsWith('team/')) loadTeamSoon(s.activeId);
    if (ev.type === 'session/title' && d.title) {
      patch.sessions = s.sessions.map((x) => (x.id === s.activeId ? { ...x, title: d.title } : x));
    }
    return patch;
  });
  if (visible()) markSeen(store.get().activeId);
}

/** Browser/computer work: a new call brings the live view forward. */
function onLive(m) {
  const prev = store.get().live;
  const fresh = m.active && (!prev?.active || JSON.stringify(prev.args) !== JSON.stringify(m.args));
  store.set({ live: m });
  const p = store.get().panel;
  if (fresh && p.open && (p.tab === 'changes' || p.tab === 'files')) setPanelTab('live');
}

/** The browser's zone, which dsh's time context passes on to the model. */
function timeZone() {
  try {
    const zone = Intl.DateTimeFormat().resolvedOptions().timeZone;
    return zone ? { timeZone: zone } : {};
  } catch { return {}; }
}

// ---- automations ---------------------------------------------------------

const automation = () => store.get().info?.automation ?? {};

export async function loadSchedules() {
  if (!automation().schedules) return;
  try { store.set({ schedules: (await api.schedules()) ?? [] }); } catch { /* keep the old list */ }
}

async function loadGoal(id) {
  if (!automation().goals) return;
  try {
    const { goal } = await api.goal(id);
    if (store.get().activeId === id) store.set({ goal: goal ?? null });
  } catch { /* no goal support */ }
}

/** `{action: create|edit|pause|resume|clear, objective?, maxGoalRounds?}` */
export async function goalCommand(body) {
  const id = store.get().activeId;
  if (!id) return false;
  try {
    const { goal } = await api.goalCommand(id, body);
    if (store.get().activeId === id) store.set({ goal: goal ?? null });
    return true;
  } catch (e) {
    fail(e);
    return false;
  }
}

async function loadTeam(id) {
  if (!automation().team) return;
  try {
    const team = await api.team(id);
    // The Lead is always listed; a Team exists once it has a teammate.
    if (store.get().activeId === id) store.set({ team: team?.members?.length > 1 ? team : null });
  } catch { if (store.get().activeId === id) store.set({ team: null }); }
}

let teamTimer = 0;
function loadTeamSoon(id) {
  clearTimeout(teamTimer);
  teamTimer = setTimeout(() => loadTeam(id), 300);
}

export const setDraft = (draft) => store.set({ draft });

export const hideLive = () => store.set((s) => ({ liveHidden: s.live?.time ?? Date.now() }));

/** Status of every chat (running dots, unread after a background run finishes). */
function watchAll() {
  api.stream(null, -1, {
    message: (m) => {
      if (m.kind === 'schedule-changed') { loadSchedules(); return; }
      if (m.kind !== 'status' || !m.sessionId) return;
      const before = store.get().sessions.find((x) => x.id === m.sessionId);
      const known = !!before;
      const wasBusy = before?.status === 'running' || before?.status === 'waiting-approval';
      if ((wasBusy && (m.status === 'idle' || m.status === 'error')) || (m.status === 'waiting-approval' && before?.status !== m.status)) {
        notifyStatus(m.sessionId, m.status);
      }
      store.set((st) => ({ sessions: st.sessions.map((x) => (x.id === m.sessionId ? { ...x, status: m.status } : x)) }));
      if (!known || m.status === 'idle' || m.status === 'error') refreshSessions();
    },
  });
}

const visible = () => document.visibilityState === 'visible';

// "Seen" is debounced: a watched run sends many events.
const seenTimers = new Map();
export function markSeen(id) {
  if (!id) return;
  const row = store.get().sessions.find((x) => x.id === id);
  if (row?.unread) store.set((s) => ({ sessions: s.sessions.map((x) => (x.id === id ? { ...x, unread: false } : x)) }));
  clearTimeout(seenTimers.get(id));
  seenTimers.set(id, setTimeout(() => {
    seenTimers.delete(id);
    api.updateSession(id, { seen: true }).catch(() => {});
  }, 700));
}

let refreshTimer = 0;
export function refreshSessions() {
  clearTimeout(refreshTimer);
  refreshTimer = setTimeout(async () => {
    try {
      const rows = await api.sessions();
      // The chat on screen is read, even if its "seen" mark is still on the way.
      const { activeId, view } = store.get();
      const watching = view === 'chat' && visible() ? activeId : null;
      store.set({ sessions: rows.map((r) => (r.id === watching && r.unread ? { ...r, unread: false } : r)) });
    } catch { /* keep the old list */ }
  }, 250);
}

// ---- actions -------------------------------------------------------------

export async function send(text, images, opts) {
  const s = store.get();
  const body = { text, ...(images.length ? { images } : {}), ...opts, ...timeZone() };
  // Only a prompt that starts a turn now shows as pending; one sent while a
  // turn runs is steered into it or queued (the queue list shows it).
  const busy = !!s.activeId && (s.status === 'running' || s.status === 'waiting-approval');
  store.set({ ...(busy ? {} : { pending: { text, images, time: Date.now() } }), statusError: null });
  try {
    let id = s.activeId;
    if (!id) {
      ({ id } = await api.createSession(s.cwd, s.composer.preset));
      await openSession(id, { fresh: true });
      refreshSessions();
    }
    await api.prompt(id, body);
    if (store.get().activeId === id && store.get().status === 'idle') store.set({ status: 'running' });
    return true;
  } catch (e) {
    if (!busy) store.set({ pending: null });
    fail(e);
    return false;
  }
}

export async function cancel() {
  const id = store.get().activeId;
  if (!id) return;
  try { await api.cancel(id); } catch (e) { fail(e); }
}

export async function decide(callId, decision) {
  store.set({ approval: null });
  try { await api.decide(callId, decision); } catch (e) { fail(e); }
}

/** Pin, archive, rename or mark a chat; the sidebar updates before the server answers. */
export async function patchSession(id, patch) {
  const local = { ...patch };
  delete local.seen;
  if (patch.seen) local.unread = false;
  store.set((s) => ({ sessions: s.sessions.map((x) => (x.id === id ? { ...x, ...local } : x)) }));
  try {
    const row = await api.updateSession(id, patch);
    if (row?.id) store.set((s) => ({ sessions: s.sessions.map((x) => (x.id === id ? { ...x, ...row, status: x.status } : x)) }));
  } catch (e) {
    fail(e);
    refreshSessions();
  }
}

export const renameSession = (id, title) => patchSession(id, { title: title.trim() });
export const setPinned = (id, pinned) => patchSession(id, { pinned });
export const setArchived = (id, archived) => patchSession(id, { archived });
export const setUnread = (id, unread) => patchSession(id, unread ? { unread: true } : { seen: true });

export function markAllRead() {
  for (const x of store.get().sessions) if (x.unread) patchSession(x.id, { seen: true });
}

// ---- focus -----------------------------------------------------------------

/**
 * After a menu, bar or dialog closed: focus goes back to `el` when it is
 * still on the page, else to the composer, unless something else took it.
 */
export function restoreFocus(el) {
  setTimeout(() => {
    const now = document.activeElement;
    if (now && now !== document.body) return;
    const target = el?.isConnected ? el : document.querySelector('textarea.composer-input');
    target?.focus?.({ preventScroll: true });
  }, 0);
}

// ---- questions -------------------------------------------------------------

let answerAsked = null;

/**
 * An in-app "Are you sure?" (instead of window.confirm, which looks foreign
 * in a browser): `{title, text?, action, danger?}`; resolves true on the
 * action, false on Cancel or Esc. A new question cancels an open one.
 */
export function ask(question) {
  answerAsked?.(false);
  return new Promise((resolve) => {
    answerAsked = resolve;
    store.set({ ask: { danger: true, ...question } });
  });
}

export function answerAsk(ok) {
  const resolve = answerAsked;
  answerAsked = null;
  store.set({ ask: null });
  resolve?.(ok);
}

export async function deleteSession(id) {
  if (!(await ask({ title: t.confirmDelete, action: t.deleteChat }))) return;
  try {
    await api.deleteSession(id);
    store.set((s) => ({ sessions: s.sessions.filter((x) => x.id !== id) }));
    if (store.get().activeId === id) newChat();
  } catch (e) { fail(e); }
}

export async function updateSettings(patch) {
  try {
    const info = await api.updateSettings(patch);
    if (info) store.set({ info });
    return true;
  } catch (e) {
    fail(e);
    return false;
  }
}

export async function addWorkspace(path) {
  try {
    const ws = await api.addWorkspace(path);
    store.set((s) => ({ workspaces: [ws, ...s.workspaces.filter((w) => w.path !== ws.path)], cwd: ws.path }));
    writeLocal(PROJECT_KEY, ws.path);
    return true;
  } catch (e) {
    fail(e);
    return false;
  }
}

export function pickWorkspace(path) {
  writeLocal(PROJECT_KEY, path);
  store.set({ cwd: path });
}

export function setComposer(patch) {
  store.set((s) => ({ composer: { ...s.composer, ...patch } }));
}

export function toggleSidebar() {
  const narrow = window.matchMedia('(max-width: 860px)').matches;
  if (narrow) {
    store.set((s) => ({ sidebarOpen: !s.sidebarOpen }));
  } else {
    const collapsed = !store.get().sidebarCollapsed;
    writeLocal('dshx-sidebar', collapsed ? 'collapsed' : '');
    store.set({ sidebarCollapsed: collapsed });
  }
}

export function setFollowup(followup) {
  writeLocal('dshx-followup', followup);
  store.set({ followup });
}

/** Browser notifications need a permission; the Mac app shows its own. */
export async function setNotify(on) {
  if (on && !store.get().desktop && 'Notification' in window && Notification.permission !== 'granted') {
    on = (await Notification.requestPermission()) === 'granted';
  }
  writeLocal('dshx-notify', on ? 'on' : '');
  store.set({ notify: on });
}

/** A chat that is not on screen finished, failed or waits for approval. */
function notifyStatus(id, status) {
  const s = store.get();
  if (id === s.activeId && s.view === 'chat' && visible() && document.hasFocus()) return;
  const body = status === 'waiting-approval' ? t.notifyWaiting : status === 'error' ? t.notifyFailed : t.notifyDone;
  const title = s.sessions.find((x) => x.id === id)?.title || t.brand;
  if (s.desktop) { window.dshxDesktop?.notify?.(title, body, { tag: id }); return; }
  if (!s.notify || !('Notification' in window) || Notification.permission !== 'granted') return;
  try {
    const n = new Notification(title, { body, tag: id });
    n.onclick = () => { window.focus(); openSession(id); n.close(); };
  } catch { /* some browsers only notify from a service worker */ }
}

// The Mac app's Dock badge counts unread chats.
let lastBadge = -1;
store.subscribe(() => {
  const s = store.get();
  if (!s.desktop) return;
  const n = s.sessions.filter((x) => x.unread && !x.archived).length;
  if (n !== lastBadge) { lastBadge = n; window.dshxDesktop?.setBadge?.(n); }
});

export function setSidebarMode(mode) {
  writeLocal('dshx-sidebar-mode', mode);
  store.set({ sidebarMode: mode });
}

export function setActivityFilter(activityFilter) {
  store.set({ activityFilter });
}

/** Folds or unfolds one project in the sidebar. */
export function toggleFolded(path) {
  const folded = store.get().folded.includes(path) ? store.get().folded.filter((p) => p !== path) : [...store.get().folded, path];
  writeLocal('dshx-folded', JSON.stringify(folded));
  store.set({ folded });
}

// ---- side panel ----------------------------------------------------------

function setPanel(patch) {
  const panel = { ...store.get().panel, ...patch };
  writeLocal('dshx-panel', panel.open ? 'open' : '');
  store.set({ panel });
}

export const openPanel = (tab) => setPanel({ open: true, ...(tab ? { tab } : {}) });
export const closePanel = () => setPanel({ open: false });
export const togglePanel = () => setPanel({ open: !store.get().panel.open });
export const setPanelTab = (tab) => setPanel({ tab });

export function setPanelWidth(px) {
  writeLocal('dshx-panel-width', String(Math.round(px)));
  store.set({ panelWidth: px });
}

/** Path relative to the chat's folder, or null when it lies outside. */
export function relTo(cwd, path) {
  if (!path) return null;
  const p = path.replace(/:\d+(-\d+)?$/, '');
  if (!p.startsWith('/')) return p.replace(/^\.\//, '');
  if (cwd && p.startsWith(`${cwd}/`)) return p.slice(cwd.length + 1);
  return null;
}

/** Opens a file of the active chat as a panel tab (at most eight tabs). */
export function openFile(path) {
  const s = store.get();
  const cwd = s.header?.cwd ?? s.sessions.find((x) => x.id === s.activeId)?.cwd ?? '';
  const rel = relTo(cwd, path);
  if (rel == null || !s.activeId) { toast(t.outsideFolder); return; }
  const files = s.panel.files.includes(rel) ? s.panel.files : [...s.panel.files, rel].slice(-8);
  setPanel({ open: true, files, tab: `file:${rel}` });
}

export function closeFile(rel) {
  const p = store.get().panel;
  const files = p.files.filter((f) => f !== rel);
  const tab = p.tab === `file:${rel}` ? (files.length ? `file:${files[files.length - 1]}` : 'changes') : p.tab;
  setPanel({ files, tab });
}

export const openPalette = () => store.set({ palette: true });
export const closePalette = () => store.set({ palette: false });
export const startRename = (id) => store.set({ renaming: id });
export const stopRename = () => store.set({ renaming: null });

/** ⌘K search, ⌘B sidebar, ⌘⇧O new chat (⌘N belongs to the browser). */
function onShortcut(e) {
  const mod = e.metaKey || e.ctrlKey;
  if (!mod || e.altKey) return;
  const k = e.key.toLowerCase();
  if (k === 'k' && !e.shiftKey) { e.preventDefault(); store.set((s) => ({ palette: !s.palette })); }
  else if (k === 'b' && !e.shiftKey) { e.preventDefault(); toggleSidebar(); }
  else if (k === 'o' && e.shiftKey) { e.preventDefault(); newChat(); }
}

/** Asks the Mac app to switch instance (the page reloads into the other one). */
export function switchInstance(id) {
  window.dshxDesktop?.switchTo?.(id);
}

export function fail(e) {
  if (e?.status === 401 || e?.status === 403) store.set({ conn: 'unauthorized' });
  else if (e?.status === 0) store.set({ conn: 'offline' });
  else toast(e?.message || String(e));
}

let toastTimer = 0;
let snackTimer = 0;
/** A short note at the bottom with one action ("Undo"); the next one replaces it. */
export function snack(text, action) {
  clearTimeout(snackTimer);
  store.set((s) => ({ snack: { text, action, id: (s.snack?.id ?? 0) + 1 } }));
  snackTimer = setTimeout(() => store.set({ snack: null }), 6000);
}

/** Archives (or restores) a chat and offers to undo it. */
export function archiveWithUndo(row, archived = !row.archived) {
  setArchived(row.id, archived);
  snack(archived ? t.archivedChat(row.title || t.newChat) : t.restoredChat(row.title || t.newChat),
    [t.undo, () => { store.set({ snack: null }); setArchived(row.id, !archived); }]);
}

export function toast(text) {
  clearTimeout(toastTimer);
  store.set({ toast: text });
  toastTimer = setTimeout(() => store.set({ toast: null }), 5000);
}
