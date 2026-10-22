// The composer's text field: a plain <textarea> (native IME, undo, paste
// and caret) over a mirror that paints pills behind `/skill`, `@server` and
// `@path` tokens, plus the `/` and `@` menu above the composer (dsh
// input-trigger: sections, ↑↓, ↩/Tab, Tab drills into folders, Esc).
// The sent text stays plain, so the server's parsing applies unchanged.
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t } from '../i18n.js';
import { useStore } from '../store.js';
import { store, api, openPlugins } from '../state.js';
import { IconFile, IconFolder, IconPlug, IconShield, IconList, IconArrowDown, IconChevronRight } from '../icons.js';
import { detectTrigger, highlight, known, learn, pills, rank, segments, typedCommand } from '../suggest.js';
import { COMMANDS, openDetails, runCommand, steerQueued, useQueue } from './composer-extras.js';
import { VIEW_MODES } from '../worklog.js';

const glyph = (body) => ({ size = 14 }) => html`<svg class="icon" width=${size} height=${size} viewBox="0 0 16 16" aria-hidden="true"
  fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round">${body}</svg>`;
const IconCompact = glyph(html`<path d="M5.5 2.5 8 5l2.5-2.5M5.5 13.5 8 11l2.5 2.5M2.75 8h10.5"/>`);
const IconTarget = glyph(html`<circle cx="8" cy="8" r="6.25"/><circle cx="8" cy="8" r="3.4"/><circle cx="8" cy="8" r=".6" fill="currentColor"/>`);
const IconSpark = glyph(html`<path d="M8 1.75 9.4 6.6l4.85 1.4-4.85 1.4L8 14.25 6.6 9.4 1.75 8l4.85-1.4L8 1.75Z"/>`);
const IconGauge = glyph(html`<path d="M2.4 11.5a5.75 5.75 0 1 1 11.2 0"/><path d="m8 9.25 2.6-3.1"/><circle cx="8" cy="9.6" r=".9" fill="currentColor"/>`);
const COMMAND_ICONS = { compact: IconCompact, goal: IconTarget, model: IconSpark, permission: IconShield, status: IconGauge, view: IconList, export: IconArrowDown };

/** Initials of a title ("Visual Brand Foundation" → "VB"). */
function initials(title) {
  const words = String(title).split(/[\s_-]+/).filter(Boolean);
  return (words.length > 1 ? words[0][0] + words[1][0] : (words[0] ?? '?').slice(0, 2)).toUpperCase();
}

const SECTION_TITLES = () => ({ commands: t.ciCommands, skills: t.ciSkills, apps: t.ciApps, files: t.ciFiles, view: t.workDetails });

function sourceLabel(item) {
  if (item.plugin) return t.ciSourcePlugin;
  return t.ciSources[item.source] ?? item.source ?? '';
}

function Trail({ item, cursor }) {
  if (item.type === 'skill') return html`<span class="ci-trail">${sourceLabel(item)}</span>`;
  if (item.type === 'server' || item.type === 'plugin') {
    if (item.needsLogin) return html`<span class="ci-trail is-warn">${t.ciLogin}</span>`;
    const tone = item.status === 'connected' || item.type === 'plugin' ? 'on' : item.status === 'error' ? 'error' : 'off';
    const label = item.active ? t.ciActiveTag : item.type === 'plugin' ? t.ciServers(item.servers?.length ?? 0)
      : item.status === 'error' ? t.ciError : item.status === 'connecting' ? t.ciConnecting : t.ciTools(item.tools ?? 0);
    return html`<span class="ci-trail"><span class=${cx('ci-dot', `is-${tone}`)} aria-hidden="true" />${label}</span>`;
  }
  if (item.type === 'folder' && cursor) return html`<span class="ci-trail ci-drill"><kbd class="kbd">Tab</kbd>${t.ciDrill}</span>`;
  if (item.type === 'mode' && item.current) return html`<span class="ci-trail">${t.ciCurrent}</span>`;
  return null;
}

function RowIcon({ item }) {
  if (item.type === 'command') {
    const I = COMMAND_ICONS[item.name] ?? IconSpark;
    return html`<span class="ci-icon"><${I} size=${14} /></span>`;
  }
  if (item.type === 'mode') return html`<span class="ci-icon"><${IconList} size=${14} /></span>`;
  if (item.type === 'file') return html`<span class="ci-icon"><${IconFile} size=${14} /></span>`;
  if (item.type === 'folder') return html`<span class="ci-icon"><${IconFolder} size=${14} /></span>`;
  if (item.type === 'plugin' && !item.brandColor) return html`<span class="ci-icon"><${IconPlug} size=${14} /></span>`;
  const style = item.brandColor ? `background:${item.brandColor};color:#fff` : '';
  return html`<span class=${cx('ci-icon', 'ci-initials', item.type === 'skill' && 'is-skill')} style=${style}>${initials(item.title ?? item.name)}</span>`;
}

function Row({ item, query, cursor, onPick, onHover, id }) {
  const name = item.type === 'command' ? `/${item.name}` : item.type === 'skill' ? item.title : item.type === 'file' || item.type === 'folder' ? item.name : item.title ?? item.name;
  const desc = item.type === 'command' ? t.ciCommandText[item.name] ?? item.description
    : item.type === 'skill' ? `/${item.name}${item.description ? ` · ${item.description}` : ''}`
      : item.type === 'file' || item.type === 'folder' ? item.dir || t.ciProjectRoot
        : item.type === 'plugin' ? item.description ?? t.ciPluginText(item.servers?.length ?? 0, item.skills ?? 0)
          : item.type === 'mode' ? t.workModeHints?.[item.name] ?? ''
            : item.description ?? `@${item.name}`;
  const q = item.type === 'command' ? `/${query}` : query.split('/').pop();
  return html`<button type="button" id=${id} role="option" aria-selected=${cursor} class=${cx('ci-row', cursor && 'is-cursor')}
    onMouseDown=${(e) => e.preventDefault()} onMouseMove=${onHover} onClick=${(e) => onPick(item, e)}>
    <${RowIcon} item=${item} />
    <span class="ci-body">
      <span class="ci-name">${highlight(q, name).map((p, i) => (p.hit ? html`<b key=${i}>${p.text}</b>` : p.text))}</span>
      ${desc ? html`<span class="ci-desc">${desc}</span>` : null}
    </span>
    <${Trail} item=${item} cursor=${cursor} />
  </button>`;
}

/** The `/` and `@` menu. */
function Popup({ state, query, cursor, setCursor, onPick, onRetry, list }) {
  const titles = SECTION_TITLES();
  const flat = list.flat;
  let n = -1;
  const empty = !state.loading && !state.error && !flat.length;
  const cursorId = flat[cursor] ? `ci-opt-${cursor}` : undefined;
  return html`<div class="ci-pop" role="listbox" aria-label=${t.ciMenu} aria-activedescendant=${cursorId}>
    <div class="ci-scroll">
      ${state.error ? html`<div class="ci-state is-error">${t.ciFailed} <button type="button" class="link-btn" onMouseDown=${(e) => e.preventDefault()} onClick=${onRetry}>${t.retry}</button></div>` : null}
      ${state.loading && !flat.length ? html`<div class="ci-state" aria-busy="true">${[0, 1, 2].map((i) => html`<span key=${i} class="ci-skeleton" />`)}</div>` : null}
      ${list.sections.map((s) => (s.items.length ? html`<div class="ci-section" key=${s.id} role="group" aria-label=${titles[s.id] ?? s.id}>
        <div class="ci-section-title">${titles[s.id] ?? s.id}${s.crumb ? html`<span class="ci-crumb"><${IconChevronRight} size=${10} />${s.crumb}</span>` : null}</div>
        ${s.items.map((item) => {
    n += 1;
    const i = n;
    return html`<${Row} key=${`${item.type}:${item.name}:${item.path ?? ''}`} id=${`ci-opt-${i}`} item=${item} query=${query} cursor=${i === cursor}
            onHover=${() => setCursor(i)} onPick=${onPick} />`;
  })}
        ${s.id === 'apps' && list.cacheMiss && s.items.some((x) => !x.active) ? html`<div class="ci-hint">${t.ciCacheHint}</div>` : null}
      </div>` : null))}
      ${empty ? html`<div class="ci-state">${list.kind === 'slash' ? t.ciNoSkills : t.ciNoMatches}
        <button type="button" class="link-btn" onMouseDown=${(e) => e.preventDefault()} onClick=${() => openPlugins()}>
          ${list.kind === 'slash' ? t.ciManageSkills : t.ciManageApps}</button></div>` : null}
    </div>
    <div class="ci-foot" aria-hidden="true"><span><kbd class="kbd">↑</kbd><kbd class="kbd">↓</kbd>${t.ciNavigate}</span>
      <span><kbd class="kbd">↩</kbd>${t.ciPick}</span><span><kbd class="kbd">esc</kbd>${t.ciClose}</span></div>
  </div>`;
}

/** Whether a row of an older answer still fits `query`. */
function stillMatches(item, query) {
  if (item.type === 'file' || item.type === 'folder') {
    const at = query.lastIndexOf('/');
    if (at >= 0 && !item.path.startsWith(query.slice(0, at + 1))) return false;
    const q = query.slice(at + 1);
    return rank(q, item.name) != null || (at < 0 && rank(q, item.path) != null);
  }
  return rank(query, item.name) != null || rank(query, item.title ?? '') != null;
}

/** Replaces `start…end` of the textarea with `text`, keeping the undo stack. */
function replaceRange(el, start, end, text) {
  el.focus();
  el.setSelectionRange(start, end);
  const ok = text ? document.execCommand('insertText', false, text) : start === end || document.execCommand('delete');
  if (!ok) {
    el.setRangeText(text, start, end, 'end');
    el.dispatchEvent(new Event('input', { bubbles: true }));
  }
}

/** Inserts `text` at the caret (or over the selection) of the composer's textarea. */
export function insertAtCaret(el, text) {
  if (!el) return;
  const start = el.selectionStart ?? el.value.length;
  const end = el.selectionEnd ?? start;
  const before = start > 0 && !/\s/.test(el.value[start - 1]) ? ' ' : '';
  replaceRange(el, start, end, before + text);
}

/** The prompts the user sent in this chat, newest first (↑ history). */
function usePromptHistory() {
  const events = useStore(store, (s) => s.events);
  return useMemo(() => {
    const out = [];
    for (let i = events.length - 1; i >= 0; i--) {
      const e = events[i];
      if (e.type !== 'user/message' || e.data?.source?.kind !== 'user') continue;
      const text = (e.data.content ?? []).filter((b) => b.type === 'text').map((b) => b.text).join('\n');
      if (text && out[out.length - 1] !== text) out.push(text);
    }
    return out;
  }, [events]);
}

/**
 * The composer's input. `areaRef` reaches the textarea (focus, height),
 * `onKeyDown` gets every key the menu and the pills do not take.
 */
export function ComposerInput({ areaRef, value, onValue, placeholder, onKeyDown, onPaste, onCompositionStart, onCompositionEnd, model, hero }) {
  const activeId = useStore(store, (s) => s.activeId);
  const cwd = useStore(store, (s) => s.cwd);
  const status = useStore(store, (s) => s.status);
  const names = useStore(known, (k) => k);
  const queued = useQueue();
  const history = usePromptHistory();
  const mirror = useRef(null);
  const composing = useRef(false);
  const [caret, setCaret] = useState(0);
  const [dismissed, setDismissed] = useState(null);
  const [focused, setFocused] = useState(false);
  const [answer, setAnswer] = useState({ loading: false, error: null, data: null, key: '' });
  const [options, setOptions] = useState(null);
  const [cursor, setCursor] = useState(0);
  const [attempt, setAttempt] = useState(0);
  const historyAt = useRef(-1);
  const running = status === 'running' || status === 'waiting-approval';

  const trigger = useMemo(() => detectTrigger(value, caret), [value, caret]);
  const live = trigger && !(dismissed && dismissed.start === trigger.start && dismissed.trigger === trigger.trigger) ? trigger : null;
  const kind = live ? (live.trigger === '@' ? 'mention' : 'slash') : null;
  const scope = activeId ? { session: activeId } : (cwd ? { cwd } : {});

  // Names for pills: one quiet fetch per chat or project.
  useEffect(() => {
    let alive = true;
    for (const k of ['slash', 'mention']) {
      api.suggest({ kind: k, q: '', ...scope }).then((a) => { if (alive) learn(a); }).catch(() => {});
    }
    return () => { alive = false; };
  }, [activeId, cwd]);

  // The menu's answer, debounced; late answers for older queries are dropped.
  const queryKey = live ? `${kind}:${live.query}:${activeId ?? cwd}:${model ?? ''}:${attempt}` : '';
  useEffect(() => {
    if (!live) return undefined;
    setAnswer((a) => ({ ...a, loading: true, error: null, key: queryKey }));
    const timer = setTimeout(async () => {
      try {
        const data = await api.suggest({ kind, q: live.query, ...scope, ...(model ? { model } : {}) });
        learn(data);
        setAnswer((a) => (a.key === queryKey ? { loading: false, error: null, data, key: queryKey, dataKey: queryKey } : a));
      } catch (error) {
        setAnswer((a) => (a.key === queryKey ? { ...a, loading: false, error } : a));
      }
    }, answer.data ? 60 : 0);
    return () => clearTimeout(timer);
  }, [queryKey]);
  useEffect(() => { setCursor(0); }, [live?.query, live?.trigger, options]);
  useEffect(() => { if (!live) setOptions(null); }, [!!live]);

  const list = useMemo(() => {
    if (options) return { kind: 'slash', sections: [{ id: 'view', items: options }], flat: options };
    const data = answer.data;
    let sections = (data?.sections ?? []).map((s) => ({ ...s, items: s.items ?? [] }));
    // An older answer while the new one loads: only what still matches the
    // query stays, so ↩ never picks a row of an earlier query.
    if (live && answer.dataKey !== queryKey) sections = sections.map((s) => ({ ...s, items: s.items.filter((x) => stillMatches(x, live.query)) }));
    if (live?.trigger === '$') sections = sections.filter((s) => s.id === 'skills');
    if (live && kind === 'mention' && live.query.includes('/')) {
      sections = sections.map((s) => (s.id === 'files' ? { ...s, crumb: live.query.slice(0, live.query.lastIndexOf('/') + 1) } : s));
    }
    if (data && data.kind !== kind) sections = [];
    return { kind, sections, flat: sections.flatMap((s) => s.items), cacheMiss: !!data?.cacheMiss };
  }, [answer, options, live?.trigger, live?.query, kind]);
  const open = focused && !!live && (list.flat.length > 0 || answer.loading || !!answer.error || (!!answer.data && !answer.loading));

  // Keep the cursor row in view.
  useEffect(() => {
    if (!open) return;
    document.getElementById(`ci-opt-${cursor}`)?.scrollIntoView?.({ block: 'nearest' });
  }, [cursor, open]);

  // The mirror follows the textarea's box and scroll position.
  useLayoutEffect(() => {
    const el = areaRef.current;
    const m = mirror.current;
    if (!el || !m) return;
    m.style.width = `${el.clientWidth}px`;
    m.style.height = `${el.clientHeight}px`;
    m.scrollTop = el.scrollTop;
  });

  const close = () => { if (live) setDismissed({ start: live.start, trigger: live.trigger }); setOptions(null); };

  const pick = (item, e) => {
    const el = areaRef.current;
    if (!el || !live) return;
    const drill = e?.key === 'Tab' && item.type === 'folder';
    if (item.type === 'mode') {
      replaceRange(el, live.start, live.end, '');
      setOptions(null);
      runCommand('view', item.name);
      return;
    }
    if (item.type === 'command') {
      if (item.name === 'view') {
        const current = store.get().info?.ui?.workDetails ?? 'standard';
        setOptions(VIEW_MODES.map((m) => ({ type: 'mode', name: m, title: t.workModes[m] ?? m, current: m === current })));
        return;
      }
      replaceRange(el, live.start, live.end, '');
      runCommand(item.name);
      return;
    }
    let insert = drill ? item.insert : item.insert.trimEnd();
    // A quoted folder stays open while drilling; picking it closes the quote.
    if (!drill && insert.startsWith('@"') && !insert.endsWith('"')) insert += '"';
    if (!drill) insert += ' ';
    replaceRange(el, live.start, live.end, insert);
    setCaret(live.start + insert.length);
    if (!drill) setDismissed(null);
  };

  const removePill = (el, back) => {
    const at = el.selectionStart;
    if (at !== el.selectionEnd) return false;
    const hit = pills(value, names).find((p) => (back ? at > p.start && at <= p.end : at >= p.start && at < p.end));
    if (!hit) return false;
    replaceRange(el, hit.start, hit.end, '');
    return true;
  };

  const keyDown = (e) => {
    const el = e.currentTarget;
    const ime = e.isComposing || composing.current || e.keyCode === 229;
    if (open && !ime) {
      const n = list.flat.length;
      if (e.key === 'ArrowDown' && !e.shiftKey && n) { e.preventDefault(); setCursor((c) => (c + 1) % n); return; }
      if (e.key === 'ArrowUp' && !e.shiftKey && n) { e.preventDefault(); setCursor((c) => (c - 1 + n) % n); return; }
      if ((e.key === 'Enter' && !e.shiftKey && !e.metaKey && !e.ctrlKey) || (e.key === 'Tab' && !e.shiftKey)) {
        if (list.flat[cursor]) { e.preventDefault(); pick(list.flat[cursor], e); return; }
      }
      if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); close(); return; }
    }
    if (ime) return onKeyDown?.(e);
    if ((e.key === 'Backspace' || e.key === 'Delete') && !e.altKey && !e.metaKey && !e.ctrlKey && removePill(el, e.key === 'Backspace')) {
      e.preventDefault();
      return;
    }
    // ↑ in an empty composer brings back this chat's last prompts; ↓ goes forward again.
    if ((e.key === 'ArrowUp' || e.key === 'ArrowDown') && !e.shiftKey && !e.altKey && !e.metaKey && !e.ctrlKey) {
      const at = historyAt.current;
      const browsing = at >= 0 && value === history[at];
      const firstLine = !value.slice(0, el.selectionStart).includes('\n');
      const lastLine = !value.slice(el.selectionEnd).includes('\n');
      if (e.key === 'ArrowUp' && firstLine && (value === '' || browsing) && (browsing ? at + 1 : 0) < history.length) {
        e.preventDefault();
        historyAt.current = browsing ? at + 1 : 0;
        onValue(history[historyAt.current]);
        return;
      }
      if (e.key === 'ArrowDown' && lastLine && browsing) {
        e.preventDefault();
        historyAt.current = at - 1;
        onValue(at > 0 ? history[at - 1] : '');
        return;
      }
    }
    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey) && !value.trim() && queued.length) {
      e.preventDefault();
      steerQueued();
      return;
    }
    if (e.key === 'Enter' && !e.shiftKey && !e.metaKey && !e.ctrlKey) {
      const command = typedCommand(value, COMMANDS);
      if (command && !(answer.data?.sections ?? []).some((s) => s.id === 'skills' && s.items.some((x) => x.name === command.name))) {
        e.preventDefault();
        onValue('');
        runCommand(command.name, command.arg);
        return;
      }
    }
    return onKeyDown?.(e);
  };

  const track = (e) => setCaret(e.currentTarget.selectionStart ?? 0);
  const segs = useMemo(() => segments(value, names), [value, names]);
  const shown = running && queued.length && !value ? t.ciSteerQueue : placeholder;

  return html`<div class="ci">
    ${open ? html`<${Popup} state=${answer} query=${live.query} cursor=${cursor} setCursor=${setCursor} list=${list}
      onPick=${pick} onRetry=${() => setAttempt((n) => n + 1)} />` : null}
    <div class=${cx('ci-field', hero && 'is-hero')}>
      <div class="ci-mirror composer-input" ref=${mirror} aria-hidden="true">${segs.map((s, i) => (s.pill
    ? html`<span key=${i} class=${cx('ci-pill', `is-${s.pill.kind}`)}>${s.text}</span>`
    : s.text))}${'​'}</div>
      <textarea ref=${areaRef} class="composer-input ci-text" rows="1" value=${value} placeholder=${shown} aria-label=${shown}
        role="combobox" aria-expanded=${open} aria-autocomplete="list" aria-haspopup="listbox"
        onInput=${(e) => { historyAt.current = -1; onValue(e.currentTarget.value); track(e); }}
        onKeyDown=${keyDown} onKeyUp=${track} onClick=${track} onSelect=${track}
        onFocus=${(e) => { setFocused(true); track(e); }} onBlur=${() => setFocused(false)}
        onScroll=${(e) => { if (mirror.current) mirror.current.scrollTop = e.currentTarget.scrollTop; }}
        onCompositionStart=${(e) => { composing.current = true; onCompositionStart?.(e); }}
        onCompositionEnd=${(e) => { composing.current = false; onCompositionEnd?.(e); }}
        onPaste=${onPaste} />
    </div>
  </div>`;
}

/** A sent prompt with the same pills; a click opens a skill's or server's details. */
export function PillText({ text }) {
  const names = useStore(known, (k) => k);
  const segs = useMemo(() => segments(text ?? '', names), [text, names]);
  if (!segs.some((s) => s.pill)) return text;
  return segs.map((s, i) => {
    if (!s.pill) return s.text;
    const p = s.pill;
    const icon = p.kind === 'file' ? html`<${IconFile} size=${12} />` : p.kind === 'folder' ? html`<${IconFolder} size=${12} />` : null;
    const label = p.kind === 'skill' || p.kind === 'command' ? `/${p.label}` : p.kind === 'server' || p.kind === 'plugin' ? `@${p.label}` : p.label;
    if (p.kind === 'skill' || p.kind === 'server' || p.kind === 'plugin') {
      return html`<button type="button" key=${i} class=${cx('pill', `is-${p.kind}`)} title=${s.text}
        onClick=${() => openDetails(p.kind === 'skill' ? 'skill' : 'server', p.name)}>${label}</button>`;
    }
    return html`<span key=${i} class=${cx('pill', `is-${p.kind}`)} title=${p.name}>${icon}${label}</span>`;
  });
}
