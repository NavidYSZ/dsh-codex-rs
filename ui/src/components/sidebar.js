// Codex-like sidebar: instance name, new chat and search on top; Pinned,
// Projects and Chats (without a project) below. The bell switches to every
// chat by recent activity, with running, waiting and unread marks.
import { useMemo, useRef, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t, age } from '../i18n.js';
import { useStore } from '../store.js';
import {
  store, newChat, openSettings, deleteSession, toggleSidebar, setSidebarMode, setActivityFilter, toggleFolded,
  openPalette, setPinned, setArchived, setUnread, startRename, stopRename, renameSession, markAllRead,
  toast, openPlugins, openAutomations,
} from '../state.js';
import {
  IconCompose, IconGear, IconSidebar, IconTrash, IconFolder, IconSearch, IconBell, IconPin, IconArchive, IconMore,
  IconPencil, IconUnread, IconCopy, IconChevronDown, IconFolderPlus, IconChat, IconChevronLeft, IconJob, IconPlug,
} from '../icons.js';
import { Menu, ActionItem } from './common.js';
import { workspaceName } from './composer.js';
import { ViewModeItems } from './worklog.js';
import { openProject } from './folders.js';
import { InstanceSwitcher } from './instances.js';
import { ViewOptions, useSidebarView } from './sidebar-view.js';

const PREVIEW = 5; // chats per project before "Show more"
const enc = encodeURIComponent;
const when = (s) => s.updatedAt ?? s.createdAt ?? 0;
const byRecent = (a, b) => when(b) - when(a);

/** The chat's state as the row's leading mark. */
function mark(s) {
  if (s.status === 'waiting-approval') return 'wait';
  if (s.status === 'running') return 'run';
  if (s.unread) return 'unread';
  return null;
}

function Lead({ s, icon }) {
  const m = mark(s);
  const label = m === 'wait' ? t.statusWaiting : m === 'run' ? t.statusRunning : m === 'unread' ? t.statusUnread : undefined;
  return html`<span class=${cx('sb-lead', m && `is-${m}`)} title=${label} aria-label=${label}>
    ${m === 'run' ? html`<span class="spinner" />` : m ? html`<span class="sb-mark" />` : icon ?? null}
  </span>`;
}

function RenameInput({ s }) {
  const [value, setValue] = useState(s.title || '');
  const done = useRef(false);
  const commit = () => {
    if (done.current) return;
    done.current = true;
    stopRename();
    if (value.trim() !== (s.title || '')) renameSession(s.id, value);
  };
  return html`<input class="sb-rename" value=${value} aria-label=${t.rename} spellcheck="false"
    ref=${(el) => { if (el && !el.dataset.focused) { el.dataset.focused = '1'; el.focus(); el.select(); } }}
    onInput=${(e) => setValue(e.currentTarget.value)} onBlur=${commit}
    onKeyDown=${(e) => {
      if (e.key === 'Enter') { e.preventDefault(); commit(); }
      if (e.key === 'Escape') { done.current = true; stopRename(); }
    }} />`;
}

/** Rename, pin, read state, archive and delete: in the row's "…" and on right-click. */
/** `view` adds the work-details switch (the chat header's menu). */
export function ChatMenu({ s, open, onClose, anchor, at, side = 'bottom', view }) {
  const desktop = useStore(store, (st) => !!st.desktop);
  const copy = async () => {
    try { await navigator.clipboard.writeText(s.cwd); } catch { toast(s.cwd); }
  };
  return html`<${Menu} open=${open} onClose=${onClose} anchor=${anchor} at=${at} fixed side=${side} align="end" class="menu-actions">
    <${ActionItem} icon=${html`<${IconPencil} size=${14} />`} onClick=${() => startRename(s.id)}>${t.rename}</${ActionItem}>
    <${ActionItem} icon=${html`<${IconPin} size=${14} />`} onClick=${() => setPinned(s.id, !s.pinned)}>${s.pinned ? t.unpin : t.pin}</${ActionItem}>
    <${ActionItem} icon=${html`<${IconUnread} size=${14} />`} onClick=${() => setUnread(s.id, !s.unread)}>${s.unread ? t.markRead : t.markUnread}</${ActionItem}>
    ${s.cwd ? html`<${ActionItem} icon=${html`<${IconCopy} size=${14} />`} onClick=${copy}>${t.copyPath}</${ActionItem}>` : null}
    ${desktop && s.cwd ? html`<${ActionItem} icon=${html`<${IconFolder} size=${14} />`} onClick=${() => window.dshxDesktop?.revealInFinder?.(s.cwd)}>${t.revealInFinder}</${ActionItem}>
      <${ActionItem} icon=${html`<${IconPencil} size=${14} />`} onClick=${() => window.dshxDesktop?.openInEditor?.(s.cwd)}>${t.openInEditor}</${ActionItem}>` : null}
    ${view ? html`<${ViewModeItems} />` : null}
    <div class="menu-sep" />
    <${ActionItem} icon=${html`<${IconArchive} size=${14} />`} onClick=${() => setArchived(s.id, !s.archived)}>${s.archived ? t.unarchive : t.archive}</${ActionItem}>
    <${ActionItem} icon=${html`<${IconTrash} size=${14} />`} tone="danger" onClick=${() => deleteSession(s.id)}>${t.deleteChat}</${ActionItem}>
  </${Menu}>`;
}

function ChatRow({ s, activeId, now, project, icon }) {
  const renaming = useStore(store, (st) => st.renaming === s.id);
  // A chat with active reminders wears a clock, like dsh's sidebar.
  const scheduled = useStore(store, (st) => st.schedules.some((r) => r.sessionId === s.id && r.status !== 'inactive'));
  if (scheduled && !mark(s)) icon = html`<${IconJob} size=${13} />`;
  const [menu, setMenu] = useState(null);
  const anchor = useRef(null);
  const active = s.id === activeId;
  return html`<div class=${cx('sb-row', active && 'is-active', menu && 'has-menu', s.archived && 'is-archived')}
      onContextMenu=${(e) => { e.preventDefault(); setMenu({ x: e.clientX, y: e.clientY }); }}>
    ${renaming
    ? html`<div class="sb-link"><${Lead} s=${s} icon=${icon} /><${RenameInput} s=${s} /></div>`
    : html`<a class="sb-link" href=${`#/s/${enc(s.id)}`} aria-current=${active ? 'page' : undefined} title=${s.title || t.newChat}>
        <${Lead} s=${s} icon=${icon} />
        <span class=${cx('sb-title', s.unread && 'is-unread')}>${s.title || t.newChat}</span>
        ${project ? html`<span class="sb-proj">${project}</span>` : null}
        ${s.archived ? html`<span class="sb-tag">${t.archivedTag}</span>` : null}
        <span class="sb-age">${age(when(s) || now, now)}</span>
      </a>`}
    <button type="button" ref=${anchor} class="sb-more" aria-label=${t.more} title=${t.more} aria-haspopup="menu"
      onClick=${() => setMenu((v) => (v ? null : true))}><${IconMore} size=${14} /></button>
    <${ChatMenu} s=${s} open=${!!menu} onClose=${() => setMenu(null)} anchor=${anchor} at=${menu === true ? null : menu} />
  </div>`;
}

function Rows({ list, activeId, now, limit, project, icon }) {
  const [all, setAll] = useState(false);
  const shown = all || !limit || list.length <= limit + 1 ? list : list.slice(0, limit);
  return html`${shown.map((s) => html`<${ChatRow} key=${s.id} s=${s} activeId=${activeId} now=${now} project=${project?.(s)} icon=${icon} />`)}
    ${shown.length < list.length || all ? html`<button type="button" class="sb-showmore" onClick=${() => setAll((v) => !v)}>
      ${all ? t.fewerChats : t.moreChats(list.length - shown.length)}</button>` : null}`;
}

function Section({ id, title, folded, actions, children }) {
  return html`<section class="sb-section" data-section=${id}>
    <div class="sb-section-head">
      <button type="button" class="sb-section-title" onClick=${() => toggleFolded(`section:${id}`)} aria-expanded=${!folded}>
        <span>${title}</span><span class=${cx('sb-fold', folded && 'is-folded')}><${IconChevronDown} size=${12} /></span>
      </button>
      <span class="sb-section-actions">${actions}</span>
    </div>
    ${folded ? null : children}
  </section>`;
}

function ProjectGroup({ p, folded, activeId, now }) {
  return html`<div class="sb-project">
    <div class="sb-project-head">
      <button type="button" class="sb-project-name" title=${p.cwd} onClick=${() => toggleFolded(p.cwd)} aria-expanded=${!folded}>
        <span class="sb-lead"><${IconFolder} size=${14} /></span>
        <span class="sb-title">${p.name}</span>
        <span class=${cx('sb-fold', folded && 'is-folded')}><${IconChevronDown} size=${12} /></span>
      </button>
      <button type="button" class="sb-more is-shown" aria-label=${t.newChatIn(p.name)} title=${t.newChatIn(p.name)}
        onClick=${() => newChat(p.cwd)}><${IconCompose} size=${14} /></button>
    </div>
    ${folded ? null : html`<div class="sb-project-rows"><${Rows} list=${p.sessions} activeId=${activeId} now=${now} limit=${PREVIEW} /></div>`}
  </div>`;
}

/**
 * Projects: folders with chats (or added on purpose), most recent first.
 * `sessions` are those the view shows; `when` orders them.
 */
function useProjects(sessions, workspaces, view) {
  return useMemo(() => {
    const groups = new Map();
    const named = new Map(workspaces.map((w) => [w.path, w]));
    // Only archived chats: folders without one stay out.
    if (view.archived !== 'only') {
      for (const w of workspaces) groups.set(w.path, { cwd: w.path, sessions: [], latest: view.order === 'activity' ? w.lastUsed ?? 0 : 0 });
    }
    for (const s of sessions) {
      if (s.scratch || s.pinned) continue;
      if (!groups.has(s.cwd)) groups.set(s.cwd, { cwd: s.cwd, sessions: [], latest: 0 });
      const g = groups.get(s.cwd);
      g.sessions.push(s);
      g.latest = Math.max(g.latest, view.when(s));
    }
    const order = (a, b) => view.when(b) - view.when(a);
    for (const g of groups.values()) g.sessions.sort(order);
    return [...groups.values()]
      .sort((a, b) => b.latest - a.latest)
      .map((g) => ({ ...g, name: named.get(g.cwd)?.name || workspaceName(g.cwd) }));
  }, [sessions, workspaces, view.order, view.archived]);
}

/** Straight to the folder dialog; the projects are listed right below anyway. */
function AddProject() {
  return html`<button type="button" class="sb-more is-shown" data-dshx-command="open-project" aria-label=${t.addProject}
    title=${`${t.addProject} (⌘O)`} onClick=${openProject}><${IconFolderPlus} size=${14} /></button>`;
}

function ProjectsView({ sessions, workspaces, activeId, now, view }) {
  const folded = useStore(store, (s) => s.folded);
  const projects = useProjects(sessions, workspaces, view);
  const order = (a, b) => view.when(b) - view.when(a);
  const pinned = sessions.filter((s) => s.pinned).sort(order);
  const chats = sessions.filter((s) => s.scratch && !s.pinned).sort(order);
  const projectOf = (s) => (s.scratch ? '' : workspaceName(s.cwd));
  const isFolded = (key) => folded.includes(key);
  if (!sessions.length && !projects.length) return html`<div class="sb-empty">${view.archived === 'only' ? t.noArchived : t.noSessions}</div>`;
  return html`
    ${pinned.length ? html`<${Section} id="pinned" title=${t.pinned} folded=${isFolded('section:pinned')}>
      <${Rows} list=${pinned} activeId=${activeId} now=${now} project=${projectOf} icon=${html`<${IconPin} size=${13} />`} />
    </${Section}>` : null}
    <${Section} id="projects" title=${t.projects} folded=${isFolded('section:projects')} actions=${html`<${AddProject} />`}>
      ${projects.map((p) => html`<${ProjectGroup} key=${p.cwd} p=${p} folded=${isFolded(p.cwd)} activeId=${activeId} now=${now} />`)}
    </${Section}>
    <${Section} id="chats" title=${t.chats} folded=${isFolded('section:chats')}
      actions=${html`<button type="button" class="sb-more is-shown" aria-label=${t.newChatNoProject} title=${t.newChatNoProject}
        onClick=${() => newChat('')}><${IconCompose} size=${14} /></button>`}>
      ${chats.length
    ? html`<${Rows} list=${chats} activeId=${activeId} now=${now} limit=${8} icon=${html`<${IconChat} size=${13} />`} />`
    : html`<button type="button" class="sb-hint" onClick=${() => newChat('')}>${t.newChatNoProject}</button>`}
    </${Section}>`;
}

/** One list: pinned chats, then every other chat with its project's name. */
function FlatView({ sessions, activeId, now, view }) {
  const folded = useStore(store, (s) => s.folded);
  const order = (a, b) => view.when(b) - view.when(a);
  const pinned = sessions.filter((s) => s.pinned).sort(order);
  const rest = sessions.filter((s) => !s.pinned).sort(order);
  const projectOf = (s) => (s.scratch ? '' : workspaceName(s.cwd));
  if (!sessions.length) return html`<div class="sb-empty">${view.archived === 'only' ? t.noArchived : t.noSessions}</div>`;
  return html`
    ${pinned.length ? html`<${Section} id="pinned" title=${t.pinned} folded=${folded.includes('section:pinned')}>
      <${Rows} list=${pinned} activeId=${activeId} now=${now} project=${projectOf} icon=${html`<${IconPin} size=${13} />`} />
    </${Section}>` : null}
    <${Section} id="all" title=${t.chats} folded=${folded.includes('section:all')}
      actions=${html`<button type="button" class="sb-more is-shown" aria-label=${t.newChat} title=${t.newChat}
        onClick=${() => newChat()}><${IconCompose} size=${14} /></button>`}>
      <${Rows} list=${rest} activeId=${activeId} now=${now} limit=${20} project=${projectOf} />
    </${Section}>`;
}

const FILTERS = {
  all: () => true,
  unread: (s) => s.unread,
  active: (s) => s.status === 'running' || s.status === 'waiting-approval',
  pinned: (s) => s.pinned,
};

function ActivityView({ sessions, activeId, now }) {
  const filter = useStore(store, (s) => s.activityFilter);
  const live = sessions.filter((s) => !s.archived);
  const list = live.filter(FILTERS[filter] ?? FILTERS.all).sort(byRecent);
  const unread = live.some((s) => s.unread);
  return html`<div class="sb-activity">
    <div class="sb-filters" role="radiogroup" aria-label=${t.activity}>
      ${Object.entries(t.activityFilters).map(([k, label]) => html`<button type="button" key=${k} role="radio" aria-checked=${k === filter}
        class=${cx('sb-filter', k === filter && 'is-on')} onClick=${() => setActivityFilter(k)}>${label}</button>`)}
    </div>
    ${unread ? html`<button type="button" class="sb-hint sb-allread" onClick=${markAllRead}>${t.markAllRead}</button>` : null}
    ${list.length
    ? html`<${Rows} list=${list} activeId=${activeId} now=${now} project=${(s) => (s.scratch ? '' : workspaceName(s.cwd))} />`
    : html`<div class="sb-empty">${t.nothingHere}</div>`}
  </div>`;
}

export function Sidebar() {
  const sessions = useStore(store, (s) => s.sessions);
  const workspaces = useStore(store, (s) => s.workspaces);
  const activeId = useStore(store, (s) => s.activeId);
  const view = useStore(store, (s) => s.view);
  const mode = useStore(store, (s) => s.sidebarMode);
  const now = Date.now();
  const unread = sessions.some((s) => s.unread && !s.archived);
  const activity = mode === 'activity';
  const shownId = view === 'chat' ? activeId : null;
  const info = useStore(store, (s) => s.info);
  const active = useStore(store, (s) => s.schedules.filter((r) => r.status !== 'inactive').length);
  const sbView = useSidebarView();
  const listed = useMemo(() => sessions.filter(sbView.keep), [sessions, sbView.archived]);
  return html`<aside class="sidebar" aria-label="Sidebar">
    <div class="sb-head">
      <${InstanceSwitcher} />
      <span class="spacer" />
      ${activity ? null : html`<${ViewOptions} />`}
      <button type="button" class=${cx('icon-btn', 'sb-bell', activity && 'is-on')} aria-pressed=${activity}
        aria-label=${t.activity} title=${t.activity} onClick=${() => setSidebarMode(activity ? 'projects' : 'activity')}>
        <${IconBell} size=${16} />${unread ? html`<span class="sb-bell-dot" />` : null}
      </button>
      <button type="button" class="icon-btn sb-toggle" aria-label=${t.toggleSidebar} title=${`${t.toggleSidebar} (⌘B)`} onClick=${toggleSidebar}>
        <${IconSidebar} size=${16} />
      </button>
    </div>
    <nav class="sb-nav">
      <button type="button" class="nav-item" data-dshx-command="new-chat" onClick=${() => newChat()}>
        <${IconCompose} size=${16} /><span>${t.newChat}</span><kbd class="kbd">⇧⌘O</kbd>
      </button>
      <button type="button" class="nav-item" onClick=${openPalette}>
        <${IconSearch} size=${16} /><span>${t.search}</span><kbd class="kbd">⌘K</kbd>
      </button>
      ${info?.extensions ? html`<button type="button" class=${cx('nav-item', view === 'plugins' && 'is-active')} onClick=${openPlugins}>
        <${IconPlug} size=${16} /><span>${t.plugins}</span>
      </button>` : null}
      ${info?.automation?.schedules ? html`<button type="button" class=${cx('nav-item', view === 'automations' && 'is-active')} onClick=${openAutomations}>
        <${IconJob} size=${16} /><span>${t.automations}</span>${active ? html`<span class="nav-count">${active}</span>` : null}
      </button>` : null}
    </nav>
    ${activity ? html`<div class="sb-mode-head">
      <button type="button" class="sb-back" onClick=${() => setSidebarMode('projects')}><${IconChevronLeft} size=${14} />${t.activity}</button>
    </div>` : null}
    <div class="sb-list">
      ${activity
    ? html`<${ActivityView} sessions=${sessions} activeId=${shownId} now=${now} />`
    : sbView.group === 'flat' ? html`<${FlatView} sessions=${listed} activeId=${shownId} now=${now} view=${sbView} />`
    : html`<${ProjectsView} sessions=${listed} workspaces=${workspaces} activeId=${shownId} now=${now} view=${sbView} />`}
    </div>
    <div class="sb-foot">
      <button type="button" class=${cx('nav-item', view === 'settings' && 'is-active')} data-dshx-command="settings" onClick=${openSettings}>
        <${IconGear} size=${16} /><span>${t.settings}</span>
      </button>
    </div>
  </aside>`;
}

