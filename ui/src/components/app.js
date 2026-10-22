// App shell: Codex-like sidebar, chat header, start page or conversation,
// composer seat, and the ⌘K palette.
import { useMemo, useRef, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t } from '../i18n.js';
import { useStore } from '../store.js';
import { store, newChat, toggleSidebar, renameSession, togglePanel } from '../state.js';
import { buildTranscript } from '../reduce.js';
import { IconCompose, IconSidebar, IconMore, IconPanelRight, Mark } from '../icons.js';
import { Transcript } from './transcript.js';
import { ComposerSeat, WorkspaceMenu, workspaceName } from './composer.js';
import { Settings } from './settings.js';
import { Sidebar, ChatMenu } from './sidebar.js';
import { Palette } from './palette.js';
import { Panel } from './panel.js';
import { LiveFloat, useLiveTab } from './live.js';
import { useTeamTab } from './team.js';
import { PluginsPage } from './plugins.js';
import { AutomationsPage } from './automations.js';
import { ModeLabel, ModeGuide } from './modes.js';

/** Title that turns into an input on double-click. */
function EditableTitle({ row, title }) {
  const [editing, setEditing] = useState(false);
  const [value, setValue] = useState('');
  if (!editing || !row) {
    return html`<span class="tb-title" title=${row ? t.rename : undefined}
      onDblClick=${row ? () => { setValue(row.title || title); setEditing(true); } : undefined}>${title}</span>`;
  }
  const done = (save) => {
    setEditing(false);
    if (save && value.trim() && value.trim() !== row.title) renameSession(row.id, value);
  };
  return html`<input class="tb-rename" value=${value} aria-label=${t.rename} spellcheck="false"
    ref=${(el) => { if (el && !el.dataset.focused) { el.dataset.focused = '1'; el.focus(); el.select(); } }}
    onInput=${(e) => setValue(e.currentTarget.value)} onBlur=${() => done(true)}
    onKeyDown=${(e) => { if (e.key === 'Enter') done(true); if (e.key === 'Escape') done(false); }} />`;
}

function TopBar({ title, sub, row }) {
  const collapsed = useStore(store, (s) => s.sidebarCollapsed);
  const panelOpen = useStore(store, (s) => s.panel.open);
  const [menu, setMenu] = useState(false);
  const anchor = useRef(null);
  return html`<header class=${cx('topbar', collapsed && 'is-collapsed')}>
    <button type="button" class="icon-btn topbar-toggle" aria-label=${t.toggleSidebar} title=${`${t.toggleSidebar} (⌘B)`} onClick=${toggleSidebar}>
      <${IconSidebar} size=${16} />
    </button>
    ${collapsed ? html`<button type="button" class="icon-btn topbar-new" aria-label=${t.newChat} title=${t.newChat} onClick=${() => newChat()}><${IconCompose} size=${16} /></button>` : null}
    <div class="topbar-title">
      ${title ? html`<${EditableTitle} row=${row} title=${title} />` : null}
      ${sub ? html`<span class="tb-sub">${sub}</span>` : null}
      ${row ? html`<${ModeLabel} />` : null}
    </div>
    <span class="spacer" />
    ${row ? html`<button type="button" class=${cx('icon-btn', panelOpen && 'is-on')} aria-label=${t.sidePanel} title=${t.sidePanel}
        aria-pressed=${panelOpen} onClick=${togglePanel}><${IconPanelRight} size=${16} /></button>` : null}
    ${row ? html`<button type="button" ref=${anchor} class="icon-btn" aria-label=${t.more} title=${t.more} aria-haspopup="menu"
        onClick=${() => setMenu((v) => !v)}><${IconMore} size=${16} /></button>
      <${ChatMenu} s=${row} open=${menu} onClose=${() => setMenu(false)} anchor=${anchor} />` : null}
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
  // A renamed chat keeps its name; otherwise the log's latest title wins.
  const title = hero ? '' : listed?.title || tx.title || (pending?.text ?? '') || t.newChat;
  const cwd = header?.cwd ?? listed?.cwd ?? '';
  const sub = hero || listed?.scratch ? '' : workspaceName(cwd);
  const liveTab = useLiveTab();
  const teamTab = useTeamTab();
  const extraTabs = liveTab || teamTab ? { ...(liveTab ? { live: liveTab } : {}), ...(teamTab ? { team: teamTab } : {}) } : null;
  return html`<div class="workspace">
    <div class=${cx('chat', hero && 'is-hero')}>
      <${TopBar} title=${title} sub=${sub} row=${listed} />
      ${hero ? html`<${Hero} />` : html`<${Transcript} tx=${tx} deltas=${deltas} status=${status} pending=${pending} cwd=${cwd} />`}
      <${ComposerSeat} hero=${hero} tx=${hero ? null : tx} />
      ${hero ? null : html`<${LiveFloat} />`}
    </div>
    ${hero ? null : html`<${Panel} tx=${tx} extraTabs=${extraTabs} />`}
  </div>`;
}

export function App() {
  const bootState = useStore(store, (s) => s.boot);
  const bootError = useStore(store, (s) => s.bootError);
  const view = useStore(store, (s) => s.view);
  const open = useStore(store, (s) => s.sidebarOpen);
  const collapsed = useStore(store, (s) => s.sidebarCollapsed);
  const desktop = useStore(store, (s) => !!s.desktop);
  if (bootState === 'loading') return html`<div class="boot" />`;
  if (bootState === 'error') {
    const msg = bootError?.status === 401 || bootError?.status === 403 ? t.unauthorized : bootError?.status === 0 ? t.offline : String(bootError?.message ?? bootError);
    return html`<div class="boot boot-error"><${Mark} size=${36} /><p>${msg}</p></div>`;
  }
  return html`<div class=${cx('app', open && 'sidebar-open', collapsed && 'sidebar-collapsed', desktop && 'is-desktop')}>
    <${Sidebar} />
    <div class="scrim" onClick=${toggleSidebar} />
    <main class="main">${view === 'settings' ? html`<${Settings} />` : view === 'plugins' ? html`<${PluginsPage} />`
    : view === 'automations' ? html`<${AutomationsPage} />` : html`<${Chat} />`}</main>
    <${Palette} />
    <${ModeGuide} />
  </div>`;
}
