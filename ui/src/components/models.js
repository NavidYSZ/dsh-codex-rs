// Model providers: the Models section of Settings (provider cards, the
// add/edit dialogs, fetching a provider's model list) and the composer's
// model picker, grouped by provider. Copy and behaviour follow DSH's
// settings-models and model-selection. Keys only ever go to the server.
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t } from '../i18n.js';
import { useStore } from '../store.js';
import { store, api, fail, setComposer, openSettings, restoreFocus } from '../state.js';
import { IconChevronDown, IconChevronRight, IconSearch, IconPlus, IconTrash, IconX } from '../icons.js';
import { Menu } from './common.js';
import { useLoad } from './page.js';
import { usd } from './usage.js';

const DEEPSEEK = 'deepseek-official';
const EFFORTS = ['off', 'low', 'medium', 'high', 'xhigh', 'max'];

// ---- numbers ---------------------------------------------------------------

const CAPACITY = /^(\d+(?:\.\d+)?)([km])?$/i;

/** "256K" or "1M" → tokens; `undefined` when blank, NaN when unreadable. */
export function parseCapacity(text) {
  const trimmed = String(text ?? '').trim();
  if (!trimmed) return undefined;
  const m = CAPACITY.exec(trimmed);
  if (!m) return NaN;
  const scale = { k: 1e3, m: 1e6 }[m[2]?.toLowerCase()] ?? 1;
  const n = Math.round(Number(m[1]) * scale);
  return n > 0 ? n : NaN;
}

/** The shortest spelling that parses back: 1000000 → "1M", 131072 stays. */
export function formatCapacity(n) {
  if (!Number.isInteger(n) || n <= 0) return '';
  if (n % 1e6 === 0) return `${n / 1e6}M`;
  if (n % 1e3 === 0) return `${n / 1e3}K`;
  return String(n);
}

/** Context sizes in the picker: 1M, 262K. */
const tokens = (n) => (n >= 1e6 ? `${+(n / 1e6).toFixed(1)}M` : n >= 1e3 ? `${Math.round(n / 1e3)}K` : String(n));

// ---- recently used ------------------------------------------------------------

const RECENT_KEY = 'dshx-recent-models';
function readRecent() {
  try { return JSON.parse(localStorage.getItem(RECENT_KEY) ?? '[]') ?? []; } catch { return []; }
}
function remember(ref) {
  const list = [ref, ...readRecent().filter((r) => r !== ref)].slice(0, 4);
  try { localStorage.setItem(RECENT_KEY, JSON.stringify(list)); } catch { /* storage blocked */ }
}

/** `info.models` in `info.providers` order; models of an older server count as DeepSeek. */
export function modelGroups(info) {
  const models = info?.models ?? [];
  const order = info?.providers ?? [{ id: DEEPSEEK, name: 'DeepSeek', available: true }];
  const of = (m) => m.provider ?? DEEPSEEK;
  return order
    .map((p) => ({ ...p, models: models.filter((m) => of(m) === p.id) }))
    .filter((g) => g.models.length);
}

// ---- picker ---------------------------------------------------------------------

/** Input and output price per 1M tokens ("$0.30 / $1.20 per 1M"). */
const priceText = (cost) => t.pickerPrice(usd(cost.input), usd(cost.output));

/** Context, image input and the (peak) list price of a model. */
export function modelHint(m) {
  if (!m) return '';
  if (m.available === false) return t.pickerNoKey;
  const parts = [];
  if (m.contextWindow) parts.push(t.pickerContext(tokens(m.contextWindow)));
  if (m.inputModalities?.includes('image')) parts.push(t.pickerImage);
  if (m.cost) parts.push(priceText(m.cost));
  return parts.join(' · ');
}

/** DeepSeek's two clocks spelled out, for a tooltip; null for flat prices. */
export function offPeakTitle(m) {
  if (!m?.cost || !m.offPeakCost) return null;
  return t.pickerPeakPrices(priceText(m.cost), priceText(m.offPeakCost));
}

function PickerBody({ sel, onClose }) {
  const info = useStore(store, (s) => s.info);
  const [q, setQ] = useState('');
  const [cursor, setCursor] = useState(-1);
  const list = useRef(null);
  const groups = useMemo(() => modelGroups(info), [info]);
  const terms = q.trim().toLowerCase().split(/\s+/).filter(Boolean);
  const matches = (m) => terms.every((w) => `${m.name ?? ''} ${m.id} ${m.providerName ?? ''}`.toLowerCase().includes(w));
  const recent = terms.length || groups.length < 2 ? [] : readRecent()
    .map((r) => sel.models.find((m) => m.id === r))
    .filter((m) => m && m.available !== false)
    .slice(0, 3);
  const sections = [
    ...(recent.length ? [{ key: 'recent', title: t.pickerRecent, models: recent }] : []),
    ...groups.map((g) => ({ key: g.id, title: g.name, models: g.models.filter(matches) })),
  ].filter((s) => s.models.length);
  const flat = sections.flatMap((s) => s.models.map((m) => ({ section: s.key, m })));
  const pickable = (n) => flat[n] && flat[n].m.available !== false;
  const pick = (m, close) => {
    if (!m || m.available === false) return;
    setComposer({ model: m.id });
    remember(m.id);
    if (close || !m.reasoningEfforts?.length) onClose();
  };
  const move = (step) => {
    let n = cursor;
    for (let i = 0; i < flat.length; i += 1) {
      n = (n + step + flat.length) % flat.length;
      if (pickable(n)) { setCursor(n); return; }
    }
  };
  const onKey = (e) => {
    if (e.key === 'ArrowDown') { e.preventDefault(); move(1); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); move(-1); }
    else if (e.key === 'Enter') {
      e.preventDefault();
      const n = pickable(cursor) ? cursor : flat.findIndex((_, i) => pickable(i));
      if (n >= 0) pick(flat[n].m, true);
    }
  };
  useEffect(() => { list.current?.querySelector('.mp-row.is-cursor')?.scrollIntoView({ block: 'nearest' }); }, [cursor]);
  // On narrow screens the chip sits right of the left edge: keep the menu inside the viewport.
  useLayoutEffect(() => {
    const fit = () => {
      const menu = list.current?.closest('.menu');
      if (!menu) return;
      menu.style.translate = '';
      const box = menu.getBoundingClientRect();
      const over = box.right - (innerWidth - 8);
      if (over > 0) menu.style.translate = `${-Math.min(over, box.left - 8)}px 0`;
    };
    fit();
    // The chip row settles a frame later (fonts, the composer's own layout).
    const frame = requestAnimationFrame(fit);
    return () => cancelAnimationFrame(frame);
  }, []);
  const efforts = sel.modelInfo?.reasoningEfforts ?? [];
  let n = -1;
  return html`<div class="mp" onKeyDown=${onKey}>
    <label class="mp-search">
      <${IconSearch} size=${14} />
      <input value=${q} placeholder=${t.pickerSearch} aria-label=${t.pickerSearch} spellcheck="false" autocomplete="off"
        ref=${(el) => { if (el && !el.dataset.focused) { el.dataset.focused = '1'; el.focus(); } }}
        onInput=${(e) => { setQ(e.currentTarget.value); setCursor(-1); }} />
    </label>
    <div class="mp-list" role="listbox" aria-label=${t.model} ref=${list}>
      ${sections.length ? sections.map((s) => html`<div class="mp-group" key=${s.key} role="group" aria-label=${s.title}>
        <div class="menu-head mp-head">${s.title}${s.models.some((m) => m.offPeakCost)
    ? html`<span class="mp-head-note">${t.pickerOffPeak}</span>` : null}</div>
        ${s.models.map((m) => {
          n += 1;
          const i = n;
          const checked = m.id === sel.model;
          return html`<button type="button" key=${`${s.key}:${m.id}`} role="option" aria-selected=${checked}
            class=${cx('mp-row', i === cursor && 'is-cursor')} disabled=${m.available === false} title=${offPeakTitle(m)}
            onMouseMove=${() => { if (cursor !== i && m.available !== false) setCursor(i); }} onClick=${() => pick(m)}>
            <span class="mp-text"><span class="mp-name">${m.name ?? m.id}</span><span class="mp-hint">${modelHint(m)}</span></span>
            <span class="menu-check">${checked ? html`<svg width="16" height="16" viewBox="0 0 16 16"><path d="m3.5 8.25 2.9 2.9 6.1-6.3" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/></svg>` : null}</span>
          </button>`;
        })}
      </div>`) : html`<div class="mp-empty" role="status">${t.pickerEmpty}</div>`}
    </div>
    ${efforts.length ? html`<div class="mp-effort">
      <span class="mp-effort-label">${t.pickerEffort}</span>
      <div class="segmented" role="radiogroup" aria-label=${t.pickerEffort}>
        ${efforts.map((e) => html`<button type="button" key=${e} role="radio" aria-checked=${e === sel.effort}
          class=${cx('seg', e === sel.effort && 'is-on')} onClick=${() => { setComposer({ effort: e }); onClose(); }}>${t.efforts[e] ?? e}</button>`)}
      </div>
    </div>` : null}
    <button type="button" class="mp-manage" onClick=${() => { onClose(); openSettings('models'); }}>${t.pickerManage}</button>
  </div>`;
}

/** The composer's model chip: name and effort; ⌃⇧M opens the picker. */
export function ModelChip({ sel }) {
  const [open, setOpen] = useState(false);
  const anchor = useRef(null);
  useEffect(() => {
    const key = (e) => {
      if (e.ctrlKey && e.shiftKey && !e.metaKey && !e.altKey && e.code === 'KeyM') { e.preventDefault(); setOpen((v) => !v); }
    };
    window.addEventListener('keydown', key);
    return () => window.removeEventListener('keydown', key);
  }, []);
  const name = sel.modelInfo?.name ?? sel.model;
  // A model without levels ignores the effort; do not show one.
  const effort = sel.modelInfo && !sel.modelInfo.reasoningEfforts?.length ? null : sel.effort;
  return html`<span class="chip-wrap">
    <button type="button" ref=${anchor} class="chip chip-model" aria-haspopup="listbox" aria-expanded=${open}
      aria-label=${t.pickerLabel(name)} title="⌃⇧M" onClick=${() => setOpen((v) => !v)}>
      <span class="chip-label">${name}</span>
      ${effort ? html`<span class="chip-effort">${t.efforts[effort] ?? effort}</span>` : null}
      <span class="chip-chev"><${IconChevronDown} size=${12} /></span>
    </button>
    <${Menu} open=${open} onClose=${() => setOpen(false)} anchor=${anchor} class="mp-menu">
      <${PickerBody} sel=${sel} onClose=${() => setOpen(false)} />
    </${Menu}>
  </span>`;
}

/** Settings' default-model select, grouped by provider. */
export function ModelSelect({ value, onChange }) {
  const info = useStore(store, (s) => s.info);
  const groups = modelGroups(info);
  const option = (m) => html`<option key=${m.id} value=${m.id} disabled=${m.available === false}>${m.name ?? m.id}</option>`;
  return html`<select class="select" value=${value} aria-label=${t.model} onChange=${(e) => onChange(e.currentTarget.value)}>
    ${groups.length > 1
    ? groups.map((g) => html`<optgroup key=${g.id} label=${g.name}>${g.models.map(option)}</optgroup>`)
    : (info?.models ?? []).map(option)}
  </select>`;
}

// ---- dialogs --------------------------------------------------------------------

const openDialogs = [];

/** Modal sheet: Esc and the scrim close the topmost one; focus returns to the opener. */
export function Dialog({ title, description, onClose, footer, children, class: klass }) {
  const box = useRef(null);
  const id = useRef(Symbol('dialog'));
  useEffect(() => {
    const opener = document.activeElement;
    openDialogs.push(id.current);
    // The first field, else the first choice, else the safe footer button.
    const first = box.current?.querySelector('input:not([disabled]), select, textarea, .mo-choice:not([disabled]), .md-foot .btn');
    first?.focus({ preventScroll: true });
    const key = (e) => {
      if (e.key === 'Escape' && openDialogs[openDialogs.length - 1] === id.current) { e.preventDefault(); e.stopPropagation(); onClose(); }
    };
    document.addEventListener('keydown', key, true);
    return () => {
      document.removeEventListener('keydown', key, true);
      openDialogs.splice(openDialogs.indexOf(id.current), 1);
      restoreFocus(opener);
    };
  }, []);
  return html`<div class="md-scrim" onMouseDown=${(e) => { if (e.target === e.currentTarget) onClose(); }}>
    <div ref=${box} class=${cx('md-dialog', klass)} role="dialog" aria-modal="true" aria-label=${title}>
      <header class="md-head">
        <div class="md-heading"><h2 class="md-title">${title}</h2>${description ? html`<p class="md-desc">${description}</p>` : null}</div>
        <button type="button" class="icon-btn md-close" aria-label=${t.close} onClick=${onClose}><${IconX} size=${14} /></button>
      </header>
      <div class="md-body">${children}</div>
      ${footer ? html`<footer class="md-foot">${footer}</footer>` : null}
    </div>
  </div>`;
}

function Labeled({ label, hint, error, children, wide }) {
  return html`<label class=${cx('md-field', wide && 'is-wide')}>
    <span class="md-label">${label}</span>
    ${children}
    ${error ? html`<span class="md-error">${error}</span>` : hint ? html`<span class="md-hint">${hint}</span>` : null}
  </label>`;
}

// ---- model rows -----------------------------------------------------------------

let rowSeq = 0;
const blankCost = { input: '', output: '', cacheRead: '', cacheWrite: '' };

/** Page model → editable row (numbers as the text the user sees). */
function toRow(m) {
  rowSeq += 1;
  const price = (v) => (typeof v === 'number' ? String(v) : '');
  return {
    key: rowSeq,
    id: m.id ?? '',
    name: m.name && m.name !== m.id ? m.name : '',
    ctx: formatCapacity(m.contextWindow),
    max: formatCapacity(m.maxTokens),
    input: m.input?.length ? m.input : m.inputModalities?.length ? m.inputModalities : ['text'],
    efforts: m.reasoningEfforts ?? [],
    defaultEffort: m.defaultReasoningEffort ?? m.defaultEffort ?? '',
    thinking: m.thinking ?? '',
    cost: m.cost ? { input: price(m.cost.input), output: price(m.cost.output), cacheRead: price(m.cost.cacheRead), cacheWrite: price(m.cost.cacheWrite) } : { ...blankCost },
  };
}

function priceOf(text) {
  const s = String(text).trim();
  if (!s) return 0;
  const v = Number(s.replace(',', '.'));
  return Number.isFinite(v) && v >= 0 ? v : NaN;
}

/** The first problem of the rows, or null. */
function rowsError(rows) {
  const seen = new Set();
  for (const r of rows) {
    const id = r.id.trim();
    if (!id) return t.modelIdRequired;
    if (seen.has(id)) return t.modelIdDuplicate;
    seen.add(id);
    if (Number.isNaN(parseCapacity(r.ctx))) return t.modelContextInvalid;
    if (Number.isNaN(parseCapacity(r.max))) return t.modelMaxTokensInvalid;
    if (Object.values(r.cost).some((v) => Number.isNaN(priceOf(v)))) return t.priceInvalid;
  }
  return null;
}

/** Row → `[[providers.<id>.models]]` fields. */
function rowBody(r) {
  const out = { id: r.id.trim(), input: r.input };
  if (r.name.trim()) out.name = r.name.trim();
  const ctx = parseCapacity(r.ctx);
  if (ctx) out.contextWindow = ctx;
  const max = parseCapacity(r.max);
  if (max) out.maxTokens = max;
  if (r.efforts.length) out.reasoningEfforts = r.efforts;
  if (r.defaultEffort && r.efforts.includes(r.defaultEffort)) out.defaultEffort = r.defaultEffort;
  if (r.thinking) out.thinking = r.thinking;
  if (Object.values(r.cost).some((v) => String(v).trim())) {
    // A blank field is an unknown price (the server prices unknown cache
    // tokens like input), not a free one.
    out.cost = Object.fromEntries(Object.entries(r.cost).filter(([, v]) => String(v).trim()).map(([k, v]) => [k, priceOf(v)]));
  }
  return out;
}

function ModelRow({ row, n, open, onToggle, onChange, onRemove }) {
  const set = (patch) => onChange({ ...row, ...patch });
  const toggleList = (list, v) => (list.includes(v) ? list.filter((x) => x !== v) : EFFORTS.filter((e) => e === v || list.includes(e)));
  return html`<div class=${cx('mo-model', open && 'is-open')}>
    <div class="mo-model-row">
      <input class="input mono" value=${row.id} placeholder=${t.modelId} aria-label=${`${t.modelId} ${n}`} spellcheck="false"
        onInput=${(e) => set({ id: e.currentTarget.value })} />
      <input class="input" value=${row.name} placeholder=${t.modelNamePlaceholder} aria-label=${`${t.modelName} ${n}`}
        onInput=${(e) => set({ name: e.currentTarget.value })} />
      <button type="button" class="icon-btn" aria-expanded=${open} aria-label=${`${t.modelOptions} ${n}`} title=${t.modelOptions} onClick=${onToggle}>
        ${open ? html`<${IconChevronDown} size=${14} />` : html`<${IconChevronRight} size=${14} />`}
      </button>
      <button type="button" class="icon-btn mo-danger" aria-label=${`${t.removeModel} ${n}`} title=${t.removeModel} onClick=${onRemove}>
        <${IconTrash} size=${14} />
      </button>
    </div>
    ${open ? html`<div class="mo-model-more">
      <${Labeled} label=${t.contextWindow} error=${Number.isNaN(parseCapacity(row.ctx)) ? t.modelContextInvalid : null}>
        <input class="input" inputmode="numeric" value=${row.ctx} placeholder=${t.capacityPlaceholder} onInput=${(e) => set({ ctx: e.currentTarget.value })} />
      </${Labeled}>
      <${Labeled} label=${t.maxOutput} error=${Number.isNaN(parseCapacity(row.max)) ? t.modelMaxTokensInvalid : null}>
        <input class="input" inputmode="numeric" value=${row.max} placeholder=${t.capacityPlaceholder} onInput=${(e) => set({ max: e.currentTarget.value })} />
      </${Labeled}>
      <fieldset class="md-field is-wide mo-checks">
        <legend class="md-label">${t.inputTypes}</legend>
        ${['text', 'image'].map((m) => html`<label key=${m} class="mo-check">
          <input type="checkbox" checked=${row.input.includes(m)} disabled=${m === 'text'}
            onChange=${() => set({ input: row.input.includes(m) ? row.input.filter((x) => x !== m) : ['text', 'image'] })} />
          ${m === 'text' ? t.inputText : t.inputImage}
        </label>`)}
      </fieldset>
      <fieldset class="md-field is-wide mo-checks">
        <legend class="md-label">${t.reasoningLevels}</legend>
        ${EFFORTS.map((e) => html`<label key=${e} class="mo-check">
          <input type="checkbox" checked=${row.efforts.includes(e)} onChange=${() => set({ efforts: toggleList(row.efforts, e) })} />
          ${t.efforts[e] ?? e}
        </label>`)}
      </fieldset>
      <fieldset class="md-field is-wide mo-prices">
        <legend class="md-label">${t.price}</legend>
        ${[['input', t.priceInput], ['output', t.priceOutput], ['cacheRead', t.priceCacheRead], ['cacheWrite', t.priceCacheWrite]].map(([k, label]) => html`<label key=${k} class="mo-price">
          <span>${label}</span>
          <input class="input" inputmode="decimal" value=${row.cost[k]} placeholder="–" aria-label=${`${label} ${n}`}
            onInput=${(e) => set({ cost: { ...row.cost, [k]: e.currentTarget.value } })} />
        </label>`)}
        <span class="md-hint mo-price-hint">${t.priceBlankHint}</span>
      </fieldset>
    </div>` : null}
  </div>`;
}

function FetchDialog({ candidates, known, onAdopt, onClose }) {
  const [q, setQ] = useState('');
  const [picked, setPicked] = useState(() => new Set(candidates.filter((c) => !known.has(c.id)).map((c) => c.id)));
  const query = q.trim().toLowerCase();
  const visible = query ? candidates.filter((c) => c.id.toLowerCase().includes(query) || c.name?.toLowerCase().includes(query)) : candidates;
  const all = visible.length > 0 && visible.every((c) => picked.has(c.id));
  const toggle = (id) => setPicked((s) => { const next = new Set(s); if (!next.delete(id)) next.add(id); return next; });
  const toggleAll = () => setPicked((s) => {
    if (all) return new Set([...s].filter((id) => !visible.some((c) => c.id === id)));
    return new Set([...s, ...visible.map((c) => c.id)]);
  });
  return html`<${Dialog} title=${t.fetchTitle} description=${t.fetchDescription} onClose=${onClose} class="md-fetch"
    footer=${html`<button type="button" class="btn" onClick=${onClose}>${t.cancel}</button>
      <button type="button" class="btn btn-primary" disabled=${!picked.size} onClick=${() => onAdopt(candidates.filter((c) => picked.has(c.id)))}>${t.fetchAdopt}</button>`}>
    <div class="mo-fetch-bar">
      <input class="input" type="search" value=${q} placeholder=${t.fetchSearch} aria-label=${t.fetchSearch} onInput=${(e) => setQ(e.currentTarget.value)} />
      <button type="button" class="btn btn-small" disabled=${!visible.length} onClick=${toggleAll}>${all ? t.fetchDeselectAll : t.fetchSelectAll}</button>
    </div>
    ${visible.length ? html`<ul class="mo-fetch-list">${visible.map((c) => html`<li key=${c.id}>
      <label class="mo-check mo-fetch-row">
        <input type="checkbox" checked=${picked.has(c.id)} onChange=${() => toggle(c.id)} />
        <span class="mono">${c.id}</span>
        ${c.contextWindow ? html`<span class="mo-fetch-meta">${tokens(c.contextWindow)}</span>` : null}
      </label>
    </li>`)}</ul>` : html`<p class="md-hint" role="status">${t.fetchNoMatches}</p>`}
  </${Dialog}>`;
}

// ---- provider editor ------------------------------------------------------------

const ID_PATTERN = /^[a-z][a-z0-9-]{0,63}$/;
const isHttpUrl = (v) => { try { return ['http:', 'https:'].includes(new URL(v).protocol); } catch { return false; } };
const keyOk = (v) => /^[\x21-\x7e]*$/.test(v.trim());

/**
 * `mode`: `edit` (an existing provider), `catalog` (add a catalog entry: its
 * key, nothing else) or `custom` (a new endpoint with its models).
 */
function ProviderEditor({ mode, provider, entry, takenIds, onClose, onSaved }) {
  const custom = mode === 'custom' || (mode === 'edit' && !provider.catalog);
  const builtin = mode === 'edit' && provider.builtin;
  const source = provider ?? entry ?? {};
  const name = source.name ?? '';
  const keyState = provider?.key ?? { configured: false, required: mode !== 'custom', env: entry?.apiKeyEnv };
  const defaultUrl = provider?.defaultBaseUrl ?? entry?.baseUrl ?? '';
  const [form, setForm] = useState(() => ({
    id: '',
    name: custom ? source.name ?? '' : '',
    baseUrl: provider && provider.baseUrl !== defaultUrl ? provider.baseUrl : '',
    api: source.api ?? '',
    key: '',
  }));
  const [rows, setRows] = useState(() => (mode === 'catalog' ? [] : (provider?.models ?? []).map(toRow)));
  const [rowsTouched, setRowsTouched] = useState(false);
  const [open, setOpen] = useState(() => new Set());
  const [busy, setBusy] = useState(null);
  const [tried, setTried] = useState(false);
  const [failure, setFailure] = useState(null);
  const [candidates, setCandidates] = useState(null);
  const set = (patch) => setForm((f) => ({ ...f, ...patch }));
  const editRows = (next) => { setRows(next); setRowsTouched(true); };

  const idError = mode === 'custom'
    ? (!ID_PATTERN.test(form.id) ? t.providerIdInvalid : takenIds.has(form.id) ? t.providerIdTaken : null) : null;
  const urlError = custom && !form.baseUrl.trim() ? t.customNeedsBaseUrl
    : form.baseUrl.trim() && !isHttpUrl(form.baseUrl.trim()) ? t.customBaseUrlInvalid : null;
  const keyError = keyOk(form.key) ? null : t.keyIllegal;
  const modelsError = rowsError(rows) ?? (custom && !rows.length ? t.customNeedsModels : null);
  const protocolError = custom && !form.api ? t.protocolUnset : null;
  const invalid = idError || urlError || keyError || modelsError || protocolError;
  const editableModels = mode !== 'catalog' && !builtin;
  const targetId = mode === 'custom' ? form.id : (provider ?? entry).id;

  const save = async (e) => {
    e?.preventDefault();
    setTried(true);
    if (invalid) return;
    const body = {};
    if (custom) { body.api = form.api; body.displayName = form.name.trim() || null; }
    if (!builtin || !provider.baseUrlLocked) body.baseUrl = form.baseUrl.trim() || null;
    if (form.key.trim()) body.apiKey = form.key.trim();
    if (editableModels && (rowsTouched || custom)) body.models = rows.map(rowBody);
    setBusy('save');
    setFailure(null);
    try {
      const row = await api.saveProvider(targetId, body);
      onSaved(t.savedProvider(row?.name ?? name));
    } catch (err) {
      setFailure(err.message || String(err));
    } finally {
      setBusy(null);
    }
  };

  const fetchModels = async () => {
    setBusy('fetch');
    setFailure(null);
    try {
      const probe = {};
      if (form.api) probe.api = form.api;
      if (form.baseUrl.trim()) probe.baseUrl = form.baseUrl.trim();
      if (form.key.trim()) probe.apiKey = form.key.trim();
      const { models } = await api.fetchProviderModels(targetId || 'new', probe);
      if (!models?.length) setFailure(t.fetchEmpty);
      else setCandidates(models);
    } catch (err) {
      setFailure(err.message || String(err));
    } finally {
      setBusy(null);
    }
  };

  const removeKey = async () => {
    setBusy('key');
    try {
      await api.setProviderKey(targetId, null);
      onSaved(t.savedProvider(name));
    } catch (err) { setFailure(err.message || String(err)); } finally { setBusy(null); }
  };

  const keyPlaceholder = keyState.locked ? t.keyEnvLocked(keyState.env) : keyState.configured ? t.keyStored : t.keyPlaceholder;
  const askable = !!(form.baseUrl.trim() || (!custom && defaultUrl)) && (!custom || form.api);
  const urlField = html`<${Labeled} label=${t.baseUrl} wide error=${tried || form.baseUrl ? urlError : null}
    hint=${builtin && provider.baseUrlLocked ? t.baseUrlLocked('DEEPSEEK_BASE_URL') : null}>
    <input class="input mono" value=${builtin && provider.baseUrlLocked ? provider.baseUrl : form.baseUrl} spellcheck="false" autocomplete="off"
      disabled=${builtin && provider.baseUrlLocked}
      placeholder=${defaultUrl ? `${t.baseUrlDefault}: ${defaultUrl}` : form.api === 'anthropic-messages' ? 'https://gateway.example' : 'https://gateway.example/v1'}
      onInput=${(e) => set({ baseUrl: e.currentTarget.value })} />
  </${Labeled}>`;
  const keyField = html`<${Labeled} label=${t.keyInput} wide error=${keyError}
    hint=${!keyState.locked && keyState.required === false && !keyState.configured && !form.key ? t.keyOptional : null}>
    <span class="md-inline">
      <input class="input mono" type="password" value=${form.key} autocomplete="off" spellcheck="false"
        disabled=${keyState.locked} placeholder=${keyPlaceholder} onInput=${(e) => set({ key: e.currentTarget.value })} />
      ${keyState.configured && keyState.source === 'stored' && mode === 'edit' ? html`<button type="button" class="btn btn-small"
        disabled=${!!busy} onClick=${removeKey}>${t.keyRemove}</button>` : null}
    </span>
  </${Labeled}>`;
  const title = mode === 'custom' ? t.addCustom : mode === 'catalog' ? `${t.addProvider}: ${name}` : t.providerEditLabel(name);
  const footer = html`<button type="button" class="btn" onClick=${onClose}>${t.cancel}</button>
    <button type="submit" form="mo-editor" class="btn btn-primary" disabled=${!!busy || (tried && !!invalid)}>
      ${busy === 'save' ? (mode === 'edit' ? t.applying : t.creating) : mode === 'edit' ? t.apply : t.createProvider}
    </button>`;
  return html`<${Dialog} title=${title} description=${mode === 'custom' ? t.addCustomHint : null} onClose=${onClose} footer=${footer} class="md-editor">
    <form id="mo-editor" class="md-form" onSubmit=${save} novalidate>
      ${mode === 'custom' ? html`<${Labeled} label=${t.providerId} hint=${t.providerIdHint} error=${tried || form.id ? idError : null}>
        <input class="input mono" value=${form.id} placeholder="my-relay" spellcheck="false" autocomplete="off"
          onInput=${(e) => set({ id: e.currentTarget.value.trim() })} />
      </${Labeled}>` : null}
      ${custom ? html`<${Labeled} label=${t.displayName}>
        <input class="input" value=${form.name} placeholder=${form.id || targetId || ''} onInput=${(e) => set({ name: e.currentTarget.value })} />
      </${Labeled}>` : null}
      ${custom && mode === 'custom' ? html`<${Labeled} label=${t.protocol} error=${tried ? protocolError : null}>
        <select class="select" value=${form.api} onChange=${(e) => set({ api: e.currentTarget.value })}>
          <option value="" disabled>${t.protocolUnset}</option>
          ${Object.entries(t.protocols).map(([k, v]) => html`<option key=${k} value=${k}>${v}</option>`)}
        </select>
      </${Labeled}>` : null}
      ${custom ? [urlField, keyField] : [keyField, urlField]}
      ${mode === 'catalog' ? html`<p class="md-hint is-wide">${t.catalogModels(entry.models)}</p>` : html`<section class="mo-models is-wide" aria-label=${t.models}>
        <div class="mo-models-head">
          <span class="md-label">${t.models}</span>
          <span class="mo-models-meta">${builtin ? t.modelsFixed : provider?.catalog ? (provider.modelsCustomized || rowsTouched ? t.modelsCustomized : t.modelsInherited) : ''}</span>
          <span class="spacer" />
          ${provider?.catalog && !builtin && (provider.modelsCustomized || rowsTouched) ? html`<button type="button" class="link-btn"
            onClick=${async () => { setBusy('save'); try { await api.saveProvider(targetId, { models: null }); onSaved(t.savedProvider(name)); } catch (err) { setFailure(err.message); } finally { setBusy(null); } }}>${t.resetModels}</button>` : null}
          ${editableModels ? html`<button type="button" class="link-btn" disabled=${!!busy || !askable}
            title=${askable ? null : t.fetchNeedsBaseUrl} onClick=${fetchModels}>${busy === 'fetch' ? t.fetching : t.fetchModels}</button>` : null}
        </div>
        ${builtin ? html`<ul class="mo-fixed">${provider.models.map((m) => html`<li key=${m.id} title=${offPeakTitle(m)}>
          <span class="mo-fixed-name">${m.name}<span class="mono">${m.id}</span></span>
          ${m.cost ? html`<span class="mo-fixed-price">${priceText(m.cost)}</span>` : null}
        </li>`)}</ul>
          ${provider.models.some((m) => m.offPeakCost) ? html`<p class="md-hint">${t.offPeakHint}</p>` : null}`
        : html`${rows.length ? null : html`<p class="md-hint">${t.modelsEmpty}</p>`}
          <div class="mo-model-list">${rows.map((r, i) => html`<${ModelRow} key=${r.key} row=${r} n=${i + 1} open=${open.has(r.key)}
            onToggle=${() => setOpen((s) => { const next = new Set(s); if (!next.delete(r.key)) next.add(r.key); return next; })}
            onChange=${(next) => editRows(rows.map((x) => (x.key === r.key ? next : x)))}
            onRemove=${() => editRows(rows.filter((x) => x.key !== r.key))} />`)}</div>
          <button type="button" class="btn btn-small mo-add-model" onClick=${() => { const row = toRow({}); editRows([...rows, row]); setOpen((s) => new Set(s).add(row.key)); }}>
            <${IconPlus} size=${14} />${t.addModel}
          </button>
          ${tried && modelsError ? html`<p class="md-error" role="alert">${modelsError}</p>` : null}
          <p class="md-hint">${t.advancedHint}</p>`}
      </section>`}
      ${failure ? html`<p class="md-error is-wide" role="alert">${failure}</p>` : null}
    </form>
    ${candidates ? html`<${FetchDialog} candidates=${candidates} known=${new Set(rows.map((r) => r.id.trim()))}
      onClose=${() => setCandidates(null)}
      onAdopt=${(picked) => {
        const have = new Set(rows.map((r) => r.id.trim()));
        editRows([...rows, ...picked.filter((c) => !have.has(c.id)).map(toRow)]);
        setCandidates(null);
      }} />` : null}
  </${Dialog}>`;
}

function AddDialog({ catalog, onPick, onClose }) {
  const [mode, setMode] = useState(null);
  const open = catalog.filter((c) => !c.added);
  if (mode === 'catalog') {
    return html`<${Dialog} title=${t.addCatalog} description=${t.addCatalogHint} onClose=${onClose}
      footer=${html`<button type="button" class="btn" onClick=${() => setMode(null)}>${t.back}</button>`}>
      <div class="mo-catalog">
        ${catalog.map((c) => html`<button type="button" key=${c.id} class="mo-choice" disabled=${c.added} onClick=${() => onPick('catalog', c)}>
          <${Logo} id=${c.id} name=${c.name} />
          <span class="mo-choice-text"><span class="mo-choice-title">${c.name}</span>
            <span class="mo-choice-hint">${t.protocols[c.api] ?? c.api} · ${t.modelCount(c.models)}</span></span>
          ${c.added ? html`<span class="pill pill-ok">${t.catalogAdded}</span>` : html`<${IconChevronRight} size=${14} />`}
        </button>`)}
        ${open.length ? null : html`<p class="md-hint">${t.addCatalogExhausted}</p>`}
      </div>
    </${Dialog}>`;
  }
  return html`<${Dialog} title=${t.addProvider} onClose=${onClose}>
    <div class="mo-catalog" role="group" aria-label=${t.addMode}>
      <button type="button" class="mo-choice" onClick=${() => setMode('catalog')}>
        <span class="mo-choice-text"><span class="mo-choice-title">${t.addCatalog}</span><span class="mo-choice-hint">${t.addCatalogHint}</span></span>
        <${IconChevronRight} size=${14} />
      </button>
      <button type="button" class="mo-choice" onClick=${() => onPick('custom')}>
        <span class="mo-choice-text"><span class="mo-choice-title">${t.addCustom}</span><span class="mo-choice-hint">${t.addCustomHint}</span></span>
        <${IconChevronRight} size=${14} />
      </button>
    </div>
  </${Dialog}>`;
}

function DeleteDialog({ provider, onClose, onDeleted }) {
  const [busy, setBusy] = useState(false);
  const remove = async () => {
    setBusy(true);
    try {
      await api.deleteProvider(provider.id);
      onDeleted();
    } catch (e) {
      fail(e);
      setBusy(false);
    }
  };
  return html`<${Dialog} title=${t.providerRemoveTitle(provider.name)} description=${t.providerRemoveText(provider.name)} onClose=${onClose} class="md-small"
    footer=${html`<button type="button" class="btn" onClick=${onClose}>${t.cancel}</button>
      <button type="button" class="btn btn-danger" disabled=${busy} onClick=${remove}>${busy ? t.providerRemoving(provider.name) : t.providerRemoveConfirm(provider.name)}</button>`} />`;
}

// ---- Models section -------------------------------------------------------------

const BRAND = { 'deepseek-official': '#4d6bfe', anthropic: '#c96442' };
function Logo({ id, name }) {
  const hue = [...id].reduce((h, c) => (h * 31 + c.charCodeAt(0)) % 360, 0);
  return html`<span class="mo-logo" aria-hidden="true" style=${`background:${BRAND[id] ?? `hsl(${hue} 32% 46%)`}`}>${(name || id).slice(0, 1).toUpperCase()}</span>`;
}

function ProviderRow({ p, onEdit, onDelete }) {
  const key = p.key ?? {};
  const keyText = key.configured ? (key.source === 'env' ? t.keyFromEnv(key.env) : t.keyConfigured)
    : key.required === false ? t.keyNone : t.keyMissing;
  const tone = key.configured ? 'is-ok' : key.required === false ? 'is-none' : 'is-missing';
  return html`<div class="mo-row">
    <${Logo} id=${p.id} name=${p.name} />
    <div class="mo-text">
      <div class="mo-name">${p.name}
        ${p.builtin ? html`<span class="mo-tag">${t.builtIn}</span>` : p.catalog ? null : html`<span class="mo-tag">${t.customTag}</span>`}
      </div>
      <div class="mo-meta"><span class=${cx('mo-dot', tone)} aria-hidden="true" />${keyText}<span class="mo-sep" aria-hidden="true">·</span>${t.modelCount(p.models?.length ?? 0)}</div>
      ${p.error ? html`<div class="md-error">${p.error}</div>` : null}
    </div>
    <div class="mo-actions">
      <button type="button" class="btn btn-small" aria-label=${t.providerEditLabel(p.name)} onClick=${onEdit}>${t.providerEdit}</button>
      ${p.builtin ? null : html`<button type="button" class="icon-btn mo-danger" aria-label=${t.providerRemoveLabel(p.name)}
        title=${t.providerRemove} onClick=${onDelete}><${IconTrash} size=${15} /></button>`}
    </div>
  </div>`;
}

/** Settings → Models with one provider's editor open (failure notices link here). */
export function editProvider(id) {
  store.set({ editProvider: id || DEEPSEEK });
  openSettings('models');
}

/** Settings → Models: the providers with their keys and models. */
export function ModelsSection({ titled = true }) {
  const { data, loading, error, reload } = useLoad(() => api.providers());
  const [dialog, setDialog] = useState(null);
  const [status, setStatus] = useState('');
  const timer = useRef(0);
  useEffect(() => () => clearTimeout(timer.current), []);
  const providers = data?.providers ?? [];
  const wanted = useStore(store, (s) => s.editProvider);
  useEffect(() => {
    if (!wanted || !data) return;
    store.set({ editProvider: null });
    const p = providers.find((x) => x.id === wanted);
    if (p) setDialog({ kind: 'edit', provider: p });
  }, [wanted, data]);
  const done = async (message) => {
    setDialog(null);
    reload();
    try { store.set({ info: await api.info() }); } catch { /* the next boot shows it */ }
    if (message) {
      setStatus(message);
      clearTimeout(timer.current);
      timer.current = setTimeout(() => setStatus(''), 3000);
    }
  };
  const taken = new Set([DEEPSEEK, ...providers.map((p) => p.id)]);
  let body;
  if (loading && !data) {
    body = html`<div class="mo-list" aria-busy="true">${[0, 1].map((i) => html`<div key=${i} class="mo-row mo-skeleton"><span class="mo-logo" /><span class="mo-bar" /></div>`)}</div>`;
  } else if (error && !data) {
    body = html`<div class="mo-empty"><span>${t.loadFailed}</span><button type="button" class="btn btn-small" onClick=${reload}>${t.retry}</button></div>`;
  } else {
    body = html`<div class="mo-list">${providers.map((p) => html`<${ProviderRow} key=${p.id} p=${p}
      onEdit=${() => setDialog({ kind: 'edit', provider: p })} onDelete=${() => setDialog({ kind: 'delete', provider: p })} />`)}</div>`;
  }
  return html`<section class="card mo-card" aria-label=${t.models}>
    ${titled ? html`<h2 class="card-title">${t.models}</h2>
    <p class="mo-intro">${t.modelsIntro}</p>` : null}
    ${body}
    <div class="mo-foot">
      <button type="button" class="btn btn-small" disabled=${!data} onClick=${() => setDialog({ kind: 'add' })}><${IconPlus} size=${14} />${t.addProvider}</button>
      <span class="mo-status" role="status">${status}</span>
    </div>
    ${dialog?.kind === 'add' ? html`<${AddDialog} catalog=${data?.catalog ?? []} onClose=${() => setDialog(null)}
      onPick=${(mode, entry) => setDialog({ kind: mode, entry })} />` : null}
    ${dialog?.kind === 'catalog' ? html`<${ProviderEditor} mode="catalog" entry=${dialog.entry} takenIds=${taken} onClose=${() => setDialog(null)} onSaved=${done} />` : null}
    ${dialog?.kind === 'custom' ? html`<${ProviderEditor} mode="custom" takenIds=${taken} onClose=${() => setDialog(null)} onSaved=${done} />` : null}
    ${dialog?.kind === 'edit' ? html`<${ProviderEditor} mode="edit" provider=${dialog.provider} takenIds=${taken} onClose=${() => setDialog(null)} onSaved=${done} />` : null}
    ${dialog?.kind === 'delete' ? html`<${DeleteDialog} provider=${dialog.provider} onClose=${() => setDialog(null)} onDeleted=${() => done()} />` : null}
  </section>`;
}
