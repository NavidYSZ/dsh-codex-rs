// App shell: flat sidebar, top bar, start page or conversation, composer seat.
import { useMemo, useRef, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t, age } from '../i18n.js';
import { useStore } from '../store.js';
import { store, newChat, openSettings, deleteSession, toggleSidebar } from '../state.js';
import { buildTranscript } from '../reduce.js';
import { IconCompose, IconGear, IconSidebar, IconTrash, IconFolder, Mark } from '../icons.js';
import { Transcript } from './transcript.js';
import { ComposerSeat, WorkspaceMenu, workspaceName } from './composer.js';
import { Settings } from './settings.js';

function groupSessions(sessions, workspaces) {
  const groups = new Map();
  for (const s of sessions) {
    if (!groups.has(s.cwd)) groups.set(s.cwd, { cwd: s.cwd, sessions: [], latest: 0 });
    const g = groups.get(s.cwd);
    g.sessions.push(s);
    g.latest = Math.max(g.latest, s.updatedAt ?? s.createdAt ?? 0);
  }
  for (const g of groups.values()) g.sessions.sort((a, b) => (b.updatedAt ?? 0) - (a.updatedAt ?? 0));
  const named = new Map(workspaces.map((w) => [w.path, w.name]));
  return [...groups.values()]
    .sort((a, b) => b.latest - a.latest)
    .map((g) => ({ ...g, name: named.get(g.cwd) || workspaceName(g.cwd) }));
}

function Sidebar() {
  const sessions = useStore(store, (s) => s.sessions);
  const workspaces = useStore(store, (s) => s.workspaces);
  const activeId = useStore(store, (s) => s.activeId);
  const view = useStore(store, (s) => s.view);
  const groups = useMemo(() => groupSessions(sessions, workspaces), [sessions, workspaces]);
  const now = Date.now();
  return html`<aside class="sidebar" aria-label="Sidebar">
    <div class="sb-head">
      <span class="brand">${t.brand}</span>
      <button type="button" class="icon-btn sb-toggle" aria-label=${t.toggleSidebar} title=${t.toggleSidebar} onClick=${toggleSidebar}>
        <${IconSidebar} size=${16} />
      </button>
    </div>
    <nav class="sb-nav">
      <button type="button" class="nav-item" onClick=${newChat}>
        <${IconCompose} size=${16} /><span>${t.newChat}</span>
      </button>
    </nav>
    <div class="sb-list">
      ${groups.length ? groups.map((g) => html`<div class="sb-group" key=${g.cwd}>
        <div class="sb-group-head" title=${g.cwd}><${IconFolder} size=${14} /><span>${g.name}</span></div>
        ${g.sessions.map((s) => html`<a key=${s.id} href=${`#/s/${encodeURIComponent(s.id)}`}
            class=${cx('sb-session', s.id === activeId && view === 'chat' && 'is-active')} aria-current=${s.id === activeId ? 'page' : undefined}>
          <span class="sb-title">${s.title || t.newChat}</span>
          ${s.status === 'running' || s.status === 'waiting-approval'
    ? html`<span class=${cx('sb-dot', s.status === 'waiting-approval' && 'is-wait')} aria-hidden="true" />`
    : html`<span class="sb-age">${age(s.updatedAt ?? s.createdAt ?? now, now)}</span>`}
          <button type="button" class="sb-del" aria-label=${t.deleteChat} title=${t.deleteChat}
            onClick=${(e) => { e.preventDefault(); e.stopPropagation(); deleteSession(s.id); }}><${IconTrash} size=${14} /></button>
        </a>`)}
      </div>`) : html`<div class="sb-empty">${t.noSessions}</div>`}
    </div>
    <div class="sb-foot">
      <button type="button" class=${cx('nav-item', view === 'settings' && 'is-active')} onClick=${openSettings}>
        <${IconGear} size=${16} /><span>${t.settings}</span>
      </button>
    </div>
  </aside>`;
}

function TopBar({ title, sub }) {
  const collapsed = useStore(store, (s) => s.sidebarCollapsed);
  return html`<header class=${cx('topbar', collapsed && 'is-collapsed')}>
    <button type="button" class="icon-btn topbar-toggle" aria-label=${t.toggleSidebar} title=${t.toggleSidebar} onClick=${toggleSidebar}>
      <${IconSidebar} size=${16} />
    </button>
    ${collapsed ? html`<button type="button" class="icon-btn topbar-new" aria-label=${t.newChat} title=${t.newChat} onClick=${newChat}><${IconCompose} size=${16} /></button>` : null}
    <div class="topbar-title">
      ${title ? html`<span class="tb-title">${title}</span>` : null}
      ${sub ? html`<span class="tb-sub">${sub}</span>` : null}
    </div>
  </header>`;
}

function Hero() {
  const cwd = useStore(store, (s) => s.cwd);
  const [open, setOpen] = useState(false);
  const anchor = useRef(null);
  const name = workspaceName(cwd);
  return html`<div class="scroll-wrap"><div class="scroll hero-scroll">
    <div class="hero">
      <span class="hero-mark"><${Mark} size=${36} /></span>
      <h1 class="hero-title">${name
    ? html`${t.heroIn[0]}<span class="chip-wrap hero-ws-wrap"><button type="button" ref=${anchor} class="hero-ws" onClick=${() => setOpen((v) => !v)}
            aria-haspopup="menu" aria-expanded=${open}>${name}</button>
            <${WorkspaceMenu} open=${open} onClose=${() => setOpen(false)} anchor=${anchor} side="bottom" align="center" /></span>${t.heroIn[1]}`
    : t.heroPlain}</h1>
    </div>
  </div></div>`;
}

function Chat() {
  const activeId = useStore(store, (s) => s.activeId);
  const events = useStore(store, (s) => s.events);
  const header = useStore(store, (s) => s.header);
  const status = useStore(store, (s) => s.status);
  const deltas = useStore(store, (s) => s.deltas);
  const pending = useStore(store, (s) => s.pending);
  const sessions = useStore(store, (s) => s.sessions);
  const tx = useMemo(() => buildTranscript(events), [events]);
  const hero = !activeId;
  const listed = sessions.find((s) => s.id === activeId);
  const title = hero ? '' : tx.title || listed?.title || (pending?.text ?? '');
  const cwd = header?.cwd ?? listed?.cwd ?? '';
  return html`<div class=${cx('chat', hero && 'is-hero')}>
    <${TopBar} title=${title} sub=${hero ? '' : workspaceName(cwd)} />
    ${hero ? html`<${Hero} />` : html`<${Transcript} tx=${tx} deltas=${deltas} status=${status} pending=${pending} cwd=${cwd} />`}
    <${ComposerSeat} hero=${hero} tx=${hero ? null : tx} />
  </div>`;
}

export function App() {
  const bootState = useStore(store, (s) => s.boot);
  const bootError = useStore(store, (s) => s.bootError);
  const view = useStore(store, (s) => s.view);
  const open = useStore(store, (s) => s.sidebarOpen);
  const collapsed = useStore(store, (s) => s.sidebarCollapsed);
  if (bootState === 'loading') return html`<div class="boot" />`;
  if (bootState === 'error') {
    const msg = bootError?.status === 401 || bootError?.status === 403 ? t.unauthorized : bootError?.status === 0 ? t.offline : String(bootError?.message ?? bootError);
    return html`<div class="boot boot-error"><${Mark} size=${36} /><p>${msg}</p></div>`;
  }
  return html`<div class=${cx('app', open && 'sidebar-open', collapsed && 'sidebar-collapsed')}>
    <${Sidebar} />
    <div class="scrim" onClick=${toggleSidebar} />
    <main class="main">${view === 'settings' ? html`<${Settings} />` : html`<${Chat} />`}</main>
  </div>`;
}
