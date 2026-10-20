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

export const store = createStore({
  boot: 'loading',
  bootError: null,
  info: null,
  workspaces: [],
  sessions: [],
  cwd: '',
  view: 'chat',
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
    store.set({ boot: 'ready', info, workspaces: workspaces ?? [], sessions: sessions ?? [], cwd: recent[0]?.path ?? sessions?.[0]?.cwd ?? '' });
  } catch (e) {
    store.set({ boot: 'error', bootError: e });
    return;
  }
  window.addEventListener('hashchange', route);
  if (!location.hash && api.fixture) {
    const fx = await api.ready.then(() => api.fx);
    if (fx.session) return openSession(fx.session.header.id, { replace: true });
    if (fx.cwd) store.set({ cwd: fx.cwd });
  }
  route();
}

function route() {
  const h = location.hash;
  if (h.startsWith('#/s/')) openSession(decodeURIComponent(h.slice(4)));
  else if (h === '#/settings') store.set({ view: 'settings', sidebarOpen: false });
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

const EMPTY_SESSION = { header: null, events: [], status: 'idle', statusError: null, deltas: null, approval: null };

function showNewChat() {
  closeStream?.();
  closeStream = null;
  openToken++;
  store.set({ ...EMPTY_SESSION, activeId: null, view: 'chat', pending: null, sidebarOpen: false, composer: {} });
}

export function newChat() {
  setHash('');
  showNewChat();
}

export function openSettings() {
  setHash('#/settings');
  store.set({ view: 'settings', sidebarOpen: false });
}

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
    pending: fresh ? s.pending : null, composer: fresh ? s.composer : {},
  });
  try {
    const data = await api.session(id);
    if (my !== openToken) return;
    store.set({ header: data.header, events: data.events ?? [], status: data.status ?? 'idle', cwd: data.header?.cwd ?? store.get().cwd });
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
      if (m.status === 'idle' || m.status === 'error') refreshSessions();
      break;
    }
    case 'approval': store.set({ approval: m }); break;
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
    if (ev.type === 'session/title' && d.title) {
      patch.sessions = s.sessions.map((x) => (x.id === s.activeId ? { ...x, title: d.title } : x));
    }
    return patch;
  });
}

let refreshTimer = 0;
export function refreshSessions() {
  clearTimeout(refreshTimer);
  refreshTimer = setTimeout(async () => {
    try { store.set({ sessions: await api.sessions() }); } catch { /* keep the old list */ }
  }, 250);
}

// ---- actions -------------------------------------------------------------

export async function send(text, images, opts) {
  const s = store.get();
  const body = { text, ...(images.length ? { images } : {}), ...opts };
  store.set({ pending: { text, images, time: Date.now() }, statusError: null });
  try {
    let id = s.activeId;
    if (!id) {
      ({ id } = await api.createSession(s.cwd));
      await openSession(id, { fresh: true });
      refreshSessions();
    }
    await api.prompt(id, body);
    if (store.get().activeId === id && store.get().status === 'idle') store.set({ status: 'running' });
    return true;
  } catch (e) {
    store.set({ pending: null });
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

export async function deleteSession(id) {
  if (!confirm(t.confirmDelete)) return;
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
    return true;
  } catch (e) {
    fail(e);
    return false;
  }
}

export function pickWorkspace(path) {
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

export function fail(e) {
  if (e?.status === 401 || e?.status === 403) store.set({ conn: 'unauthorized' });
  else if (e?.status === 0) store.set({ conn: 'offline' });
  else toast(e?.message || String(e));
}

let toastTimer = 0;
export function toast(text) {
  clearTimeout(toastTimer);
  store.set({ toast: text });
  toastTimer = setTimeout(() => store.set({ toast: null }), 5000);
}
