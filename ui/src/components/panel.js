// Right side panel, like the Codex app's: tabs for the chat's changes, its
// folder, live browser/computer work and open files. Files load through the
// server's read-only preview grant, so pages and PDFs render as themselves.
import { useEffect, useMemo, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t, compact } from '../i18n.js';
import { useStore } from '../store.js';
import { highlight } from '../highlight.js';
import { countChanges } from '../diff.js';
import { store, api, openFile, closeFile, closePanel, setPanelTab, setPanelWidth, relTo } from '../state.js';
import {
  IconX, IconFile, IconFolder, IconRefresh, IconExternal, IconCopy, IconChevronRight, IconDiff, IconPencil,
} from '../icons.js';
import { Diff, editHunks, editPath } from './tools.js';
import { Markdown } from './transcript.js';
import { copyText } from '../markdown.js';

const extOf = (p) => (p.match(/\.([a-z0-9+]+)$/i)?.[1] ?? '').toLowerCase();
const baseName = (p) => p.split('/').pop() || p;

// ---- preview grant ---------------------------------------------------------

const grants = new Map(); // session id → { at, promise }
const GRANT_MS = 90 * 60 * 1000;

function previewBase(id) {
  const hit = grants.get(id);
  if (hit && Date.now() - hit.at < GRANT_MS) return hit.promise;
  const promise = api.preview(id).then((r) => r.base);
  promise.catch(() => grants.delete(id));
  grants.set(id, { at: Date.now(), promise });
  return promise;
}

function useBase(id) {
  const [base, setBase] = useState(null);
  useEffect(() => {
    let live = true;
    setBase(null);
    if (id) previewBase(id).then((b) => live && setBase(b), () => live && setBase(false));
    return () => { live = false; };
  }, [id]);
  return base;
}

const fileUrl = (base, rel) => base + rel.split('/').map(encodeURIComponent).join('/');

// ---- changes ---------------------------------------------------------------

/** Files changed by edit/write in the given turns, newest change first. */
export function collectChanges(turns, cwd) {
  const byPath = new Map();
  let order = 0;
  for (const turn of turns) {
    for (const x of turn.items) {
      if (x.kind !== 'tool' || (x.name !== 'edit' && x.name !== 'write') || !x.result || x.result.isError) continue;
      const path = editPath(x);
      if (!path) continue;
      const hunks = editHunks(x);
      const f = byPath.get(path) ?? { path, rel: relTo(cwd, path) ?? path, hunks: [], created: false, order: 0 };
      f.hunks.push(...hunks);
      f.created ||= x.result.meta?.operation === 'create';
      f.order = order++;
      byPath.set(path, f);
    }
  }
  return [...byPath.values()]
    .map((f) => ({ ...f, ...countChanges(f.hunks.flat()) }))
    .sort((a, b) => b.order - a.order);
}

export function Counts({ add, del }) {
  if (!add && !del) return null;
  return html`<span class="counts">${add ? html`<span class="add">+${add}</span>` : null}${del ? html`<span class="del">−${del}</span>` : null}</span>`;
}

function ChangedFile({ f }) {
  const [open, setOpen] = useState(true);
  return html`<div class=${cx('pf', open && 'is-open')}>
    <div class="pf-head">
      <button type="button" class="pf-toggle" onClick=${() => setOpen((v) => !v)} aria-expanded=${open}>
        <span class="pf-chev"><${IconChevronRight} size=${12} /></span>
        <span class="pf-path" title=${f.rel}><span class="pf-dir">${f.rel.includes('/') ? f.rel.slice(0, f.rel.lastIndexOf('/') + 1) : ''}</span>${baseName(f.rel)}</span>
      </button>
      <${Counts} add=${f.add} del=${f.del} />
      <button type="button" class="icon-btn" title=${t.open} aria-label=${t.open} onClick=${() => openFile(f.path)}><${IconFile} size=${14} /></button>
    </div>
    ${open ? html`<div class="pf-body"><${Diff} hunks=${f.hunks} lang=${extOf(f.rel)} /></div>` : null}
  </div>`;
}

function ChangesView({ tx, cwd }) {
  const changes = useMemo(() => collectChanges(tx?.turns ?? [], cwd), [tx, cwd]);
  if (!changes.length) return html`<div class="panel-empty"><${IconDiff} size=${20} /><span>${t.noChanges}</span></div>`;
  const total = changes.reduce((n, f) => ({ add: n.add + f.add, del: n.del + f.del }), { add: 0, del: 0 });
  return html`<div class="panel-scroll">
    <div class="panel-summary">${t.filesChanged(changes.length)} <${Counts} ...${total} /></div>
    ${changes.map((f) => html`<${ChangedFile} key=${f.path} f=${f} />`)}
  </div>`;
}

// ---- folder ----------------------------------------------------------------

function FilesView({ id }) {
  const [dir, setDir] = useState('');
  const [state, setState] = useState({ loading: true });
  const [nonce, setNonce] = useState(0);
  useEffect(() => {
    let live = true;
    setState({ loading: true });
    api.files(id, dir).then((r) => live && setState(r), (e) => live && setState({ error: e.message }));
    return () => { live = false; };
  }, [id, dir, nonce]);
  const crumbs = dir ? dir.split('/') : [];
  return html`<div class="panel-scroll">
    <div class="pf-crumbs">
      <button type="button" class="crumb" onClick=${() => setDir('')}><${IconFolder} size=${14} /></button>
      ${crumbs.map((c, i) => html`<span class="crumb-sep">/</span><button type="button" class="crumb" onClick=${() => setDir(crumbs.slice(0, i + 1).join('/'))}>${c}</button>`)}
      <span class="spacer" />
      <button type="button" class="icon-btn" title=${t.reload} aria-label=${t.reload} onClick=${() => setNonce((n) => n + 1)}><${IconRefresh} size=${14} /></button>
    </div>
    ${state.loading ? html`<div class="panel-empty">${t.loading}</div>`
    : state.error ? html`<div class="panel-empty is-error">${state.error}</div>`
      : !state.entries?.length ? html`<div class="panel-empty">${t.emptyFolder}</div>`
        : html`<div class="tree">${state.entries.map((e) => html`<button type="button" key=${e.path} class=${cx('tree-row', e.kind === 'dir' && 'is-dir')}
            onClick=${() => (e.kind === 'dir' ? setDir(e.path) : openFile(e.path))}>
            <span class="tree-icon">${e.kind === 'dir' ? html`<${IconFolder} size=${14} />` : html`<${IconFile} size=${14} />`}</span>
            <span class="tree-name">${e.name}</span>
            ${e.kind === 'file' ? html`<span class="tree-size">${compact(e.size)}</span>` : null}
          </button>`)}</div>`}
  </div>`;
}

// ---- file viewer -----------------------------------------------------------

const KINDS = [
  ['image', /^(png|jpe?g|gif|webp|avif|bmp|ico|svg)$/],
  ['pdf', /^pdf$/],
  ['html', /^x?html?$/],
  ['markdown', /^(md|markdown|mdx)$/],
  ['table', /^(csv|tsv)$/],
  ['video', /^(mp4|webm|mov|m4v)$/],
  ['audio', /^(mp3|wav|ogg|m4a|flac)$/],
  ['json', /^json$/],
];
const kindOf = (rel) => KINDS.find(([, re]) => re.test(extOf(rel)))?.[0] ?? 'text';
const TEXT_LIMIT = 2 * 1024 * 1024;
const LINE_LIMIT = 5000;

/** Fetches a text file; tells binary, missing and oversized files apart. */
function useText(url, nonce) {
  const [state, setState] = useState({ loading: true });
  useEffect(() => {
    if (!url) return undefined;
    let live = true;
    setState({ loading: true });
    fetch(url, { cache: 'no-store' }).then(async (r) => {
      if (r.status === 404) return { error: t.notFound };
      if (r.status === 413) return { error: t.tooLarge };
      if (!r.ok) return { error: `HTTP ${r.status}` };
      const buf = new Uint8Array(await r.arrayBuffer());
      if (buf.subarray(0, 8192).includes(0)) return { error: t.binary };
      const text = new TextDecoder().decode(buf.byteLength > TEXT_LIMIT ? buf.subarray(0, TEXT_LIMIT) : buf);
      return { text, cut: buf.byteLength > TEXT_LIMIT };
    }).catch((e) => ({ error: e.message })).then((s) => { if (live) setState(s); });
    return () => { live = false; };
  }, [url, nonce]);
  return state;
}

function CodeView({ text, lang }) {
  const lines = useMemo(() => text.split('\n').slice(0, LINE_LIMIT), [text]);
  return html`<div class="pv-code"><table class="code-lines"><tbody>
    ${lines.map((l, i) => html`<tr key=${i}><td class="ln">${i + 1}</td><td class="lc" dangerouslySetInnerHTML=${{ __html: highlight(l, lang) || ' ' }} /></tr>`)}
  </tbody></table></div>`;
}

/** Small CSV/TSV reader: quoted fields, doubled quotes, CRLF. */
function parseTable(text, sep) {
  const rows = [];
  let row = [];
  let cell = '';
  let quoted = false;
  for (let i = 0; i < text.length && rows.length < 1001; i++) {
    const c = text[i];
    if (quoted) {
      if (c === '"' && text[i + 1] === '"') { cell += '"'; i++; } else if (c === '"') quoted = false; else cell += c;
    } else if (c === '"' && cell === '') quoted = true;
    else if (c === sep) { row.push(cell); cell = ''; }
    else if (c === '\n') { row.push(cell.replace(/\r$/, '')); rows.push(row); row = []; cell = ''; }
    else cell += c;
  }
  if (cell || row.length) { row.push(cell); rows.push(row); }
  return rows;
}

function TableView({ text, sep }) {
  const rows = useMemo(() => parseTable(text, sep), [text, sep]);
  const [head, ...body] = rows;
  return html`<div class="pv-table"><table><thead><tr>${(head ?? []).map((c, i) => html`<th key=${i}>${c}</th>`)}</tr></thead>
    <tbody>${body.slice(0, 1000).map((r, i) => html`<tr key=${i}>${r.map((c, j) => html`<td key=${j}>${c}</td>`)}</tr>`)}</tbody></table>
    ${body.length >= 1000 ? html`<div class="pv-note">${t.firstRows(1000)}</div>` : null}</div>`;
}

function TextBody({ url, rel, kind, mode, nonce }) {
  const state = useText(url, nonce);
  if (state.loading) return html`<div class="panel-empty">${t.loading}</div>`;
  if (state.error) return html`<div class="panel-empty">${state.error}</div>`;
  const lang = extOf(rel);
  if (kind === 'markdown' && mode !== 'source') return html`<div class="pv-doc"><${Markdown} text=${state.text} /></div>`;
  if (kind === 'table' && mode !== 'source') return html`<${TableView} text=${state.text} sep=${lang === 'tsv' ? '\t' : ','} />`;
  if (kind === 'json') {
    let pretty = state.text;
    try { pretty = JSON.stringify(JSON.parse(state.text), null, 2); } catch { /* show as is */ }
    return html`<${CodeView} text=${pretty} lang="json" />`;
  }
  return html`<${CodeView} text=${state.text} lang=${lang} />`;
}

function FileView({ id, rel, version }) {
  const base = useBase(id);
  const root = useStore(store, (s) => s.header?.cwd ?? '');
  const desktop = useStore(store, (s) => !!s.desktop);
  const kind = kindOf(rel);
  const togglable = kind === 'markdown' || kind === 'html' || kind === 'table';
  const [mode, setMode] = useState('preview');
  const [nonce, setNonce] = useState(0);
  const url = base ? fileUrl(base, rel) : null;
  const key = `${nonce}:${version}`;
  let body;
  if (base === false) body = html`<div class="panel-empty">${t.notFound}</div>`;
  else if (!url) body = html`<div class="panel-empty">${t.loading}</div>`;
  else if (kind === 'image') body = html`<div class="pv-media pv-checker"><img key=${key} src=${`${url}?v=${key}`} alt=${rel} /></div>`;
  else if (kind === 'pdf') body = html`<iframe key=${key} class="pv-frame" src=${url} title=${rel} />`;
  else if (kind === 'html' && mode === 'preview') {
    body = html`<iframe key=${key} class="pv-frame pv-page" src=${url} title=${rel} sandbox="allow-scripts allow-forms allow-popups allow-modals" />`;
  } else if (kind === 'video') body = html`<div class="pv-media"><video key=${key} src=${url} controls /></div>`;
  else if (kind === 'audio') body = html`<div class="pv-media"><audio key=${key} src=${url} controls /></div>`;
  else body = html`<${TextBody} url=${url} rel=${rel} kind=${kind} mode=${mode} nonce=${key} />`;
  return html`<div class="pv">
    <div class="pv-bar">
      <span class="pv-path" title=${rel}>${rel}</span>
      ${togglable ? html`<div class="segmented pv-mode" role="radiogroup">
        ${[['preview', t.preview], ['source', t.source]].map(([v, label]) => html`<button type="button" key=${v} role="radio" aria-checked=${mode === v}
          class=${cx('seg', mode === v && 'is-on')} onClick=${() => setMode(v)}>${label}</button>`)}
      </div>` : null}
      <button type="button" class="icon-btn" title=${t.reload} aria-label=${t.reload} onClick=${() => setNonce((n) => n + 1)}><${IconRefresh} size=${14} /></button>
      <button type="button" class="icon-btn" title=${t.copyPath} aria-label=${t.copyPath} onClick=${() => copyText(root ? `${root}/${rel}` : rel)}><${IconCopy} size=${14} /></button>
      ${desktop ? html`<button type="button" class="icon-btn" title=${t.openInEditor} aria-label=${t.openInEditor}
        onClick=${() => window.dshxDesktop?.openInEditor?.(`${root}/${rel}`)}><${IconPencil} size=${14} /></button>` : null}
      ${url ? html`<a class="icon-btn" href=${url} target="_blank" rel="noopener noreferrer" title=${t.openNewTab} aria-label=${t.openNewTab}><${IconExternal} size=${14} /></a>` : null}
    </div>
    <div class="pv-body">${body}</div>
  </div>`;
}

// ---- shell -----------------------------------------------------------------

function Resizer() {
  const onDown = (e) => {
    e.preventDefault();
    const panel = e.currentTarget.parentElement;
    const right = panel.getBoundingClientRect().right;
    const move = (ev) => {
      const w = Math.min(Math.max(320, right - ev.clientX), Math.round(innerWidth * 0.72));
      panel.style.width = `${w}px`;
    };
    const up = () => {
      removeEventListener('pointermove', move);
      removeEventListener('pointerup', up);
      document.body.classList.remove('is-resizing');
      setPanelWidth(panel.getBoundingClientRect().width);
    };
    document.body.classList.add('is-resizing');
    addEventListener('pointermove', move);
    addEventListener('pointerup', up);
  };
  return html`<div class="panel-resize" onPointerDown=${onDown} role="separator" aria-orientation="vertical" />`;
}

/** Bumps when the agent edits a file, so an open preview reloads itself. */
function useEditVersions(tx) {
  return useMemo(() => {
    const v = {};
    for (const turn of tx?.turns ?? []) {
      for (const x of turn.items) {
        if (x.kind === 'tool' && (x.name === 'edit' || x.name === 'write') && x.result) {
          const p = editPath(x);
          v[p] = (v[p] ?? 0) + 1;
        }
      }
    }
    return v;
  }, [tx]);
}

export function Panel({ tx, extraTabs }) {
  const panel = useStore(store, (s) => s.panel);
  const id = useStore(store, (s) => s.activeId);
  const cwd = useStore(store, (s) => s.header?.cwd ?? '');
  const width = useStore(store, (s) => s.panelWidth);
  const versions = useEditVersions(tx);
  const changes = useMemo(() => collectChanges(tx?.turns ?? [], cwd).length, [tx, cwd]);
  if (!panel.open || !id) return null;
  const fileTab = panel.tab.startsWith('file:') ? panel.tab.slice(5) : null;
  const tab = (key, label, extra) => html`<button type="button" key=${key} role="tab" aria-selected=${panel.tab === key}
    class=${cx('ptab', panel.tab === key && 'is-on')} onClick=${() => setPanelTab(key)}>${label}${extra}</button>`;
  let body;
  if (fileTab != null) {
    const abs = cwd ? `${cwd}/${fileTab}` : fileTab;
    body = html`<${FileView} key=${`${id}:${fileTab}`} id=${id} rel=${fileTab} version=${(versions[abs] ?? 0) + (versions[fileTab] ?? 0)} />`;
  } else if (panel.tab === 'files') body = html`<${FilesView} key=${id} id=${id} />`;
  else if (extraTabs?.[panel.tab]) body = extraTabs[panel.tab].body;
  else body = html`<${ChangesView} tx=${tx} cwd=${cwd} />`;
  return html`<aside class="panel" style=${width ? `width:${width}px` : undefined} aria-label=${t.sidePanel}>
    <${Resizer} />
    <div class="panel-tabs" role="tablist">
      ${tab('changes', t.changes, changes ? html`<span class="ptab-count">${changes}</span>` : null)}
      ${tab('files', t.filesTab)}
      ${Object.entries(extraTabs ?? {}).map(([k, v]) => tab(k, v.label, v.badge))}
      ${panel.files.map((f) => html`<span key=${f} class=${cx('ptab', 'ptab-file', panel.tab === `file:${f}` && 'is-on')} title=${f}>
        <button type="button" class="ptab-name" onClick=${() => setPanelTab(`file:${f}`)}>${baseName(f)}</button>
        <button type="button" class="ptab-x" aria-label=${t.closeTab} onClick=${() => closeFile(f)}><${IconX} size=${12} /></button>
      </span>`)}
      <span class="spacer" />
      <button type="button" class="icon-btn" aria-label=${t.closePanel} title=${t.closePanel} onClick=${closePanel}><${IconX} size=${14} /></button>
    </div>
    <div class="panel-body">${body}</div>
  </aside>`;
}

