// Choosing a project folder. The start page's project menu, ⌘O, "Add
// project" in the sidebar and folders dropped on the Mac app all end here.
// Dialogs, best first: the Mac app's own sheet (local instance), the server's
// macOS dialog (a browser on the same Mac), else the folder browser below,
// fed by /api/fs (docs/rust/SPEC.md §6). The browser's wording follows DSH's
// "Select Workspace Directory".
import { render } from 'preact';
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t } from '../i18n.js';
import { createStore, useStore } from '../store.js';
import { store, api, pickWorkspace, newChat, toast, fail } from '../state.js';
import { IconFolder, IconFolderPlus, IconChevronRight, IconChevronLeft, IconPencil, IconX } from '../icons.js';
import { Menu, MenuItem, ActionItem } from './common.js';

const LAST_KEY = 'dshx-folder-last';
const RECENT = 6; // recent projects in the browser's suggestions

const S = { fill: 'none', stroke: 'currentColor', 'stroke-width': 1.3, 'stroke-linecap': 'round', 'stroke-linejoin': 'round' };
const IconGit = ({ size = 16 }) => html`<svg class="icon" width=${size} height=${size} viewBox="0 0 16 16" aria-hidden="true">
  <circle cx="4.75" cy="3.75" r="1.5" ...${S}/><circle cx="4.75" cy="12.25" r="1.5" ...${S}/><circle cx="11.25" cy="5.25" r="1.5" ...${S}/>
  <path d="M4.75 5.25v5.5M11.25 6.75c0 2.6-2.2 3.4-5.2 4.1" ...${S}/></svg>`;
const IconHome = ({ size = 16 }) => html`<svg class="icon" width=${size} height=${size} viewBox="0 0 16 16" aria-hidden="true">
  <path d="M2.75 7.1 8 2.75l5.25 4.35v5.65a1 1 0 0 1-1 1H9.6V10.2H6.4v3.55H3.75a1 1 0 0 1-1-1V7.1Z" ...${S}/></svg>`;

const folderName = (path) => path.split('/').filter(Boolean).pop() || path;
const joinPath = (dir, name) => (dir === '/' ? `/${name}` : `${dir}/${name}`);

function readLocal(key) {
  try { return localStorage.getItem(key) ?? ''; } catch { return ''; }
}
function writeLocal(key, value) {
  try { localStorage.setItem(key, value); } catch { /* storage blocked */ }
}

const CODES = {
  'not-found': () => t.folderGone, 'permission-denied': () => t.folderDenied, 'not-a-folder': () => t.notAFolder,
  'outside-roots': () => t.folderOutside, exists: () => t.folderExists, invalid: () => t.folderInvalid,
};
// POST /api/workspaces answers with the OS error text only.
const MESSAGES = [
  [/os error 2\b|no such file|does not exist/i, () => t.folderGone],
  [/os error 1\b|os error 13\b|permission denied|not permitted/i, () => t.folderDenied],
  [/not a folder|not a directory/i, () => t.notAFolder],
];

/** A folder error in the user's words. */
function folderError(e) {
  if (CODES[e?.code]) return CODES[e.code]();
  const message = e?.message ?? String(e);
  return MESSAGES.find(([re]) => re.test(message))?.[1]() ?? message;
}

/** Adds `path` as a project and selects it (a known one is just selected); null on failure. */
export async function addProject(path) {
  try {
    const ws = await api.addWorkspace(path);
    store.set((s) => ({ workspaces: [ws, ...s.workspaces.filter((w) => w.path !== ws.path)] }));
    pickWorkspace(ws.path);
    return ws;
  } catch (e) {
    // A folder error (outside the roots, denied) has a code; 401/403 without one is the session.
    if (e?.status === 0 || ((e?.status === 401 || e?.status === 403) && !CODES[e?.code])) fail(e);
    else toast(folderError(e));
    return null;
  }
}

let nativeDialog = true; // false once the server said it has none
let picking = false;

/** A folder from the best dialog available here; null when cancelled. */
export async function chooseFolder() {
  if (picking) return null;
  picking = true;
  try {
    const d = window.dshxDesktop;
    if (d) {
      if (d.capabilities?.pickFolder && d.pickFolder) {
        return (await d.pickFolder({ title: t.pickTitle, prompt: t.pickPrompt })) || null;
      }
      // Another machine's folders: a dialog on this Mac would show the wrong disk.
      return await browse();
    }
    if (nativeDialog) {
      try {
        return (await api.fsPick({ prompt: t.pickTitle }))?.path ?? null;
      } catch (e) {
        if (e?.code === 'busy') return null;
        if (e?.status !== 501 && e?.status !== 404) toast(folderError(e));
        nativeDialog = false;
      }
    }
    return await browse();
  } finally {
    picking = false;
  }
}

/** ⌘O, "Add project" and the Mac app's File menu: pick a folder and start a chat there. */
export async function openProject() {
  const path = await chooseFolder();
  if (!path) return;
  const ws = await addProject(path);
  if (ws) newChat(ws.path);
}

// ---- project menu (start page chip and title) -------------------------------

/** ↑/↓, Home and End move between a menu's items; the checked (or first) one has focus on open. */
function MenuKeys({ anchor, children }) {
  const ref = useRef(null);
  const items = () => [...(ref.current?.querySelectorAll('.menu-item:not(:disabled)') ?? [])];
  useLayoutEffect(() => {
    const all = items();
    (all.find((b) => b.getAttribute('aria-checked') === 'true') ?? all[0])?.focus();
    return () => { if (ref.current?.contains(document.activeElement)) anchor?.current?.focus(); };
  }, []);
  const onKeyDown = (e) => {
    const all = items();
    const i = all.indexOf(document.activeElement);
    const next = { ArrowDown: all[(i + 1) % all.length], ArrowUp: all[(i - 1 + all.length) % all.length], Home: all[0], End: all[all.length - 1] }[e.key];
    if (next) { e.preventDefault(); next.focus(); }
  };
  return html`<div ref=${ref} class="fp-keys" onKeyDown=${onKeyDown}>${children}</div>`;
}

/** Renders its children in a layer on <body>, out of reach of clipping or masked ancestors. */
function Portal({ children }) {
  const host = useRef(null);
  if (!host.current) host.current = document.createElement('div');
  useLayoutEffect(() => {
    const el = host.current;
    document.body.appendChild(el);
    return () => { render(null, el); el.remove(); };
  }, []);
  useLayoutEffect(() => { render(children, host.current); });
  return null;
}

/**
 * "No project", the recent projects and "Choose folder…". `onPick(path)`
 * runs after a choice ('' = no project); by default it only selects it.
 */
export function ProjectMenu({ open, onClose, anchor, side = 'top', align = 'start', onPick = pickWorkspace }) {
  const workspaces = useStore(store, (s) => s.workspaces);
  const cwd = useStore(store, (s) => s.cwd);
  const recent = useMemo(() => [...workspaces].sort((a, b) => (b.lastUsed ?? 0) - (a.lastUsed ?? 0)), [workspaces]);
  const pick = (path) => { onClose(); onPick(path); };
  const choose = async () => {
    onClose();
    const path = await chooseFolder();
    const ws = path ? await addProject(path) : null;
    if (ws) onPick(ws.path);
  };
  // On <body> and placed in the viewport: the start page's title sits in a
  // scrolling, masked column that would clip the menu.
  return html`<${Portal}><${Menu} open=${open} onClose=${onClose} anchor=${anchor} side=${side} align=${align} fixed class="menu-wide fp-menu">
    <${MenuKeys} anchor=${anchor}>
      <${MenuItem} checked=${!cwd} hint=${t.noProjectHint} onClick=${() => pick('')}>${t.noProject}</${MenuItem}>
      ${recent.length ? html`<div class="menu-sep" /><div class="menu-head">${t.recentProjects}</div>
        <div class="fp-recent">${recent.map((w) => html`<${MenuItem} key=${w.path} checked=${w.path === cwd} hint=${w.path}
          onClick=${() => pick(w.path)}>${w.name || folderName(w.path)}</${MenuItem}>`)}</div>` : null}
      <div class="menu-sep" />
      <${ActionItem} icon=${html`<${IconFolderPlus} size=${14} />`} kbd="⌘O" onClick=${choose}>${t.chooseFolder}</${ActionItem}>
    </${MenuKeys}>
  </${Menu}></${Portal}>`;
}

// ---- folder browser ---------------------------------------------------------

const ui = createStore({ browse: null, dropping: false });
let opened = 0;

function browse() {
  mount();
  return new Promise((resolve) => ui.set({
    browse: { id: ++opened, resolve, start: readLocal(LAST_KEY), before: document.activeElement },
  }));
}

function closeBrowser(path) {
  const b = ui.get().browse;
  ui.set({ browse: null });
  if (b?.before?.isConnected) b.before.focus({ preventScroll: true });
  b?.resolve(path || null);
}

/** Breadcrumbs from the root (home shows as "Home") down to `path`. */
function crumbs({ path, root, home }) {
  const first = { path: root, label: root === home ? t.folderHome : root, home: root === home };
  if (path === root) return [first];
  let at = root;
  return [first, ...path.slice(root === '/' ? 1 : root.length + 1).split('/').map((name) => {
    at = joinPath(at, name);
    return { path: at, label: name };
  })];
}

function Crumbs({ listing, onGo }) {
  const ref = useRef(null);
  // Long paths keep the current folder in view.
  useLayoutEffect(() => { if (ref.current) ref.current.scrollLeft = ref.current.scrollWidth; }, [listing.path]);
  const list = crumbs(listing);
  return html`<nav ref=${ref} class="fp-crumbs" aria-label=${t.pickTitle}>
    ${list.map((c, i) => html`${i ? html`<span class="fp-crumb-sep"><${IconChevronRight} size=${12} /></span>` : null}
      <button type="button" key=${c.path} class=${cx('fp-crumb', i === list.length - 1 && 'is-current')}
        aria-current=${i === list.length - 1 ? 'location' : undefined} onClick=${() => onGo(c.path)}>
        ${c.home ? html`<${IconHome} size=${14} />` : null}<span>${c.label}</span>
      </button>`)}
  </nav>`;
}

function Row({ row, index, selected, onSelect, onActivate }) {
  const Icon = row.git ? IconGit : IconFolder;
  return html`<div id=${`fp-row-${index}`} role="option" aria-selected=${selected}
    class=${cx('fp-row', selected && 'is-sel', row.hidden && 'is-hidden')}
    onMouseDown=${(e) => { e.preventDefault(); onSelect(row.key); }} onDblClick=${() => onActivate(row)}>
    <span class=${cx('fp-icon', row.git && 'is-git')} title=${row.git ? t.gitRepo : undefined}><${Icon} size=${15} /></span>
    <span class="fp-name">${row.name}</span>
    ${row.hint ? html`<span class="fp-hint">${row.hint}</span>` : null}
    ${row.kind === 'dir' ? html`<button type="button" class="fp-enter" tabindex="-1" aria-label=${row.name}
      onMouseDown=${(e) => e.stopPropagation()} onClick=${() => onActivate(row)}><${IconChevronRight} size=${14} /></button>` : null}
  </div>`;
}

function NewFolderRow({ onCreate, onCancel, error }) {
  const [name, setName] = useState(t.untitledFolder);
  return html`<div class="fp-new">
    <div class="fp-row fp-new-row">
      <span class="fp-icon"><${IconFolderPlus} size=${15} /></span>
      <input class="input fp-new-input" value=${name} aria-label=${t.folderName} spellcheck="false" autocomplete="off"
        ref=${(el) => { if (el && !el.dataset.focused) { el.dataset.focused = '1'; el.focus(); el.select(); } }}
        onInput=${(e) => setName(e.currentTarget.value)}
        onKeyDown=${(e) => {
          if (e.key === 'Enter') { e.preventDefault(); onCreate(name.trim()); }
          if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); onCancel(); }
        }} />
      <button type="button" class="btn btn-small btn-primary" onClick=${() => onCreate(name.trim())}>${t.createFolder}</button>
    </div>
    ${error ? html`<div class="fp-row-error" role="alert">${error}</div>` : null}
  </div>`;
}

/** Folder rows under an optional section head; `start` is the first row's index in the list. */
function Section({ title, rows, start, sel, onSelect, onActivate }) {
  if (!rows.length) return null;
  return html`${title ? html`<div class="menu-head fp-section">${title}</div>` : null}
    ${rows.map((row, i) => html`<${Row} key=${row.key} row=${row} index=${start + i} selected=${row.key === sel}
      onSelect=${onSelect} onActivate=${onActivate} />`)}`;
}

function Browser({ start }) {
  const workspaces = useStore(store, (s) => s.workspaces);
  const [listing, setListing] = useState(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState(null);
  const [unsupported, setUnsupported] = useState(false);
  const [hidden, setHidden] = useState(false);
  const [sel, setSel] = useState(null);
  const [initial, setInitial] = useState(true);
  const [repos, setRepos] = useState([]);
  const [editing, setEditing] = useState(false);
  const [typed, setTyped] = useState('');
  const [creating, setCreating] = useState(false);
  const [createError, setCreateError] = useState(null);
  const seq = useRef(0);
  const list = useRef(null);
  const typeahead = useRef({ text: '', at: 0 });

  /** Shows `path`; resolves to the listing, or null after an error. */
  const load = async (path, { select = null, fallback = false } = {}) => {
    const n = ++seq.current;
    setLoading(true);
    try {
      const r = await api.fsDirs(path, hidden);
      if (n !== seq.current) return null;
      setListing(r);
      setError(null);
      setSel(select);
      writeLocal(LAST_KEY, r.path);
      return r;
    } catch (e) {
      if (n !== seq.current) return null;
      if (e?.code === 'unsupported' || (e?.status === 404 && !e?.code)) {
        setUnsupported(true);
        setEditing(true);
        setError(t.browseUnavailable);
      } else if (fallback && path) {
        // The last folder is gone: start at home instead.
        return load('', { select });
      } else {
        setError(folderError(e));
      }
      return null;
    } finally {
      if (n === seq.current) setLoading(false);
    }
  };

  useEffect(() => {
    load(start, { fallback: true });
    api.fsSuggestions().then((r) => setRepos(r?.repos ?? [])).catch(() => {});
  }, []);
  useEffect(() => { if (listing) load(listing.path, { select: sel }); }, [hidden]);

  const focusList = () => list.current?.focus({ preventScroll: true });
  const go = (path) => { setInitial(false); setEditing(false); setCreating(false); load(path); };
  const known = new Set(workspaces.map((w) => w.path));
  const show = initial && listing && !editing;
  const recent = !show ? [] : [...workspaces].sort((a, b) => (b.lastUsed ?? 0) - (a.lastUsed ?? 0)).slice(0, RECENT)
    .map((w) => ({ key: `r:${w.path}`, kind: 'recent', name: w.name || folderName(w.path), path: w.path, hint: w.path }));
  const found = !show ? [] : repos.filter((r) => !known.has(r.path)).slice(0, RECENT)
    .map((r) => ({ key: `g:${r.path}`, kind: 'repo', name: r.name, path: r.path, hint: r.path, git: true }));
  const dirs = (listing?.entries ?? []).map((e) => ({ ...e, key: `d:${e.path}`, kind: 'dir' }));
  const rows = [...recent, ...found, ...dirs];
  const index = rows.findIndex((r) => r.key === sel);
  const current = index >= 0 ? rows[index] : null;

  useLayoutEffect(() => { list.current?.querySelector('.fp-row.is-sel')?.scrollIntoView({ block: 'nearest' }); }, [sel, listing]);

  const choose = (row = current) => closeBrowser(row?.path ?? listing?.path);
  const activate = (row) => (row.kind === 'dir' ? go(row.path) : choose(row));
  const select = (key) => { setSel(key); focusList(); };
  const move = (delta) => {
    if (!rows.length) return;
    const next = index < 0 ? (delta > 0 ? 0 : rows.length - 1) : Math.max(0, Math.min(rows.length - 1, index + delta));
    setSel(rows[next].key);
  };

  const create = async (name) => {
    try {
      const r = await api.fsMkdir(listing.path, name);
      setCreating(false);
      setCreateError(null);
      await load(listing.path, { select: `d:${r.path}` });
      focusList();
    } catch (e) {
      setCreateError(folderError(e));
    }
  };

  /** Edit path: ↩ goes there; "Open" takes the typed folder right away. */
  const submitPath = async (open) => {
    const path = typed.trim();
    if (!path) return;
    // Without a listing the typed path is the answer, as the old text field was.
    if (unsupported) { closeBrowser(path); return; }
    setInitial(false);
    const r = await load(path);
    if (!r) return;
    if (open) { closeBrowser(r.path); return; }
    setEditing(false);
    focusList();
  };

  const onListKey = (e) => {
    if (e.target !== e.currentTarget) return; // typing in the new folder's name
    const mod = e.metaKey || e.ctrlKey;
    const key = e.key;
    if (key === 'ArrowDown' && mod) { if (current?.kind === 'dir') go(current.path); }
    else if (key === 'ArrowUp' && mod) { if (listing?.parent) go(listing.parent); }
    else if (key === 'ArrowDown') move(1);
    else if (key === 'ArrowUp') move(-1);
    else if (key === 'ArrowRight') { if (current?.kind === 'dir') go(current.path); }
    else if (key === 'ArrowLeft' || key === 'Backspace') { if (listing?.parent) go(listing.parent); }
    else if (key === 'Home') { if (rows.length) setSel(rows[0].key); }
    else if (key === 'End') { if (rows.length) setSel(rows[rows.length - 1].key); }
    else if (key === 'Enter') choose();
    else if (key.length === 1 && !mod && !e.altKey) {
      // Type-ahead: the first folder starting with what was typed.
      const now = Date.now();
      const ta = typeahead.current;
      ta.text = (now - ta.at > 800 ? '' : ta.text) + key.toLowerCase();
      ta.at = now;
      const hit = dirs.find((r) => r.name.toLowerCase().startsWith(ta.text));
      if (hit) setSel(hit.key);
      else return;
    } else return;
    e.preventDefault();
  };

  const onKey = (e) => {
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      if (creating) { setCreating(false); focusList(); }
      else if (editing && !unsupported) { setEditing(false); focusList(); }
      else closeBrowser(null);
    } else if (e.key === 'Tab') {
      // Focus stays inside the dialog.
      const items = [...e.currentTarget.querySelectorAll('button:not(:disabled):not([tabindex="-1"]), input:not(:disabled), [tabindex="0"]')];
      const first = items[0];
      const last = items[items.length - 1];
      if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
      else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
    }
  };

  const here = !listing ? '' : listing.path === listing.home ? t.folderHome : folderName(listing.path) || '/';
  return html`<div class="fp-scrim" onMouseDown=${(e) => { if (e.target === e.currentTarget) closeBrowser(null); }}>
    <div class="fp-dialog" role="dialog" aria-modal="true" aria-labelledby="fp-title" onKeyDown=${onKey}>
      <div class="fp-head">
        <span id="fp-title" class="fp-title">${t.pickTitle}</span>
        ${loading && listing ? html`<span class="spinner" role="status" aria-label=${t.loading} />` : null}
        <span class="spacer" />
        <button type="button" class="icon-btn" aria-label=${t.cancel} title=${t.cancel} onClick=${() => closeBrowser(null)}><${IconX} size=${16} /></button>
      </div>
      <div class="fp-bar">
        <button type="button" class="icon-btn" aria-label=${t.parentFolder} title=${`${t.parentFolder} (⌘↑)`}
          disabled=${!listing?.parent || editing} onClick=${() => go(listing.parent)}><${IconChevronLeft} size=${16} /></button>
        ${editing ? html`<form class="fp-path" onSubmit=${(e) => { e.preventDefault(); submitPath(false); }}>
            <input class="input" value=${typed} placeholder=${t.addWorkspaceHint} aria-label=${t.editPath} spellcheck="false" autocomplete="off"
              ref=${(el) => { if (el && !el.dataset.focused) { el.dataset.focused = '1'; el.focus(); el.select(); } }}
              onInput=${(e) => setTyped(e.currentTarget.value)} />
          </form>`
    : listing ? html`<${Crumbs} listing=${listing} onGo=${go} />` : html`<span class="fp-crumbs is-loading">${t.loading}</span>`}
        ${unsupported ? null : html`<button type="button" class=${cx('icon-btn', editing && 'is-on')} aria-pressed=${editing}
          aria-label=${t.editPath} title=${t.editPath} disabled=${!listing}
          onClick=${() => { if (editing) { setEditing(false); focusList(); } else { setTyped(listing?.path ?? ''); setEditing(true); } }}>
          <${IconPencil} size=${15} /></button>`}
      </div>
      ${listing && listing.roots.length > 1 && !editing ? html`<div class="fp-roots">
        ${listing.roots.map((r) => html`<button type="button" key=${r} class=${cx('fp-root', listing.root === r && 'is-on')} onClick=${() => go(r)}>
          ${r === listing.home ? html`<${IconHome} size=${13} /><span>${t.folderHome}</span>` : html`<${IconFolder} size=${13} /><span>${r}</span>`}
        </button>`)}
      </div>` : null}
      ${error ? html`<div class="fp-error" role="alert">${error}</div>` : null}
      <div class="fp-list" role="listbox" tabindex="0" aria-label=${listing?.path ?? t.pickTitle}
        aria-activedescendant=${index >= 0 ? `fp-row-${index}` : undefined} onKeyDown=${onListKey}
        ref=${(el) => { list.current = el; if (el && !el.dataset.focused) { el.dataset.focused = '1'; el.focus(); } }}>
        ${creating ? html`<${NewFolderRow} error=${createError} onCreate=${create}
          onCancel=${() => { setCreating(false); setCreateError(null); focusList(); }} />` : null}
        ${!listing && loading ? html`<div class="fp-empty"><span class="spinner" />${t.loading}</div>` : null}
        <${Section} title=${t.recentProjects} rows=${recent} start=${0} sel=${sel} onSelect=${select} onActivate=${activate} />
        <${Section} title=${t.suggestedRepos} rows=${found} start=${recent.length} sel=${sel} onSelect=${select} onActivate=${activate} />
        <${Section} title=${recent.length || found.length ? here : null} rows=${dirs} start=${recent.length + found.length}
          sel=${sel} onSelect=${select} onActivate=${activate} />
        ${listing && !dirs.length && !creating ? html`<div class="fp-empty">${t.noSubfolders}</div>` : null}
        ${listing?.truncated ? html`<div class="fp-note">${t.foldersTruncated(listing.entries.length)}</div>` : null}
      </div>
      <div class="fp-foot">
        <span class="fp-tools">
          <button type="button" class="btn btn-small" disabled=${!listing || editing || creating}
            onClick=${() => { setCreateError(null); setCreating(true); }}><${IconFolderPlus} size=${14} />${t.newFolder}</button>
          <label class="fp-toggle">
            <input type="checkbox" checked=${hidden} disabled=${!listing || editing} onChange=${(e) => setHidden(e.currentTarget.checked)} />
            <span>${t.showHidden}</span>
          </label>
        </span>
        <span class="fp-actions">
          <button type="button" class="btn btn-small" onClick=${() => closeBrowser(null)}>${t.cancel}</button>
          <button type="button" class="btn btn-small btn-primary" disabled=${editing ? !typed.trim() : !listing}
            title=${editing ? typed : current?.path ?? listing?.path ?? ''}
            onClick=${() => (editing ? submitPath(true) : choose())}>${t.open}</button>
        </span>
      </div>
    </div>
  </div>`;
}

// ---- drop target (Mac app) and the layer both live in ------------------------

function Layer() {
  const browsing = useStore(ui, (s) => s.browse);
  const dropping = useStore(ui, (s) => s.dropping);
  return html`${browsing ? html`<${Browser} key=${browsing.id} start=${browsing.start} />` : null}
    ${dropping ? html`<div class="fp-drop" aria-hidden="true">
      <div class="fp-drop-card"><${IconFolderPlus} size=${22} /><span>${t.dropProject}</span></div>
    </div>` : null}`;
}

let root = null;
function mount() {
  if (root) return;
  root = document.createElement('div');
  root.id = 'dshx-folders';
  document.body.appendChild(root);
  render(html`<${Layer} />`, root);
}

window.addEventListener('keydown', (e) => {
  if (e.key.toLowerCase() !== 'o' || !(e.metaKey || e.ctrlKey) || e.shiftKey || e.altKey) return;
  if (store.get().boot !== 'ready' || ui.get().browse) return;
  e.preventDefault();
  openProject();
});
// File → Open project… (⌘O) in the Mac app.
window.addEventListener('dshx-desktop-command', (e) => {
  if (e.detail?.command !== 'open-project') return;
  e.preventDefault();
  if (!ui.get().browse) openProject();
});
// Folders dragged from the Finder onto the Mac app (local instance only).
window.addEventListener('dshx-desktop-drag', (e) => {
  mount();
  ui.set({ dropping: !!e.detail?.active });
});
window.addEventListener('dshx-desktop-drop', async (e) => {
  ui.set({ dropping: false });
  let last = null;
  for (const path of e.detail?.paths ?? []) last = (await addProject(path)) ?? last;
  if (last) newChat(last.path);
});
