// ⌘F: find in the open chat (Codex). Matches are painted with the CSS
// Custom Highlight API, so the transcript's DOM stays untouched; turns that
// are not rendered yet but contain the text load first.
import { useEffect, useRef, useState } from 'preact/hooks';
import { html } from '../html.js';
import { t } from '../i18n.js';
import { useStore } from '../store.js';
import { store, restoreFocus } from '../state.js';
import { IconSearch, IconChevronDown, IconX } from '../icons.js';

const SKIP = '.msg-actions, .find-bar, .turn-rail, .load-older, .chev, svg';
const canPaint = () => typeof CSS !== 'undefined' && CSS.highlights && typeof Highlight === 'function';

/** Opens the bar (or focuses it again with its text selected). */
export function openFind() {
  const s = store.get();
  if (s.view !== 'chat' || !s.activeId) return false;
  store.set({ find: s.find ?? '', findFocus: (s.findFocus ?? 0) + 1 });
  return true;
}

export function closeFind() {
  store.set({ find: null });
  restoreFocus(null);
}

// The bar closes when the chat changes.
let lastChat = store.get().activeId;
store.subscribe(() => {
  const s = store.get();
  if (s.activeId !== lastChat) {
    lastChat = s.activeId;
    if (s.find != null) store.set({ find: null });
  }
});

/** The index of the first turn whose prompts or answers contain `query`. */
export function firstMatch(turns, query) {
  const q = query.trim().toLocaleLowerCase();
  if (!q) return -1;
  return turns.findIndex((turn) => turn.items.some((x) => (x.kind === 'user' || x.kind === 'text')
    && x.text?.toLocaleLowerCase().includes(q)));
}

function collect(root, query) {
  const needle = query.toLocaleLowerCase();
  const ranges = [];
  if (!root || !needle) return ranges;
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode: (n) => (n.parentElement?.closest(SKIP) ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT),
  });
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    const hay = node.data.toLocaleLowerCase();
    for (let i = hay.indexOf(needle); i >= 0 && i + needle.length <= node.data.length; i = hay.indexOf(needle, i + needle.length)) {
      const r = document.createRange();
      r.setStart(node, i);
      r.setEnd(node, i + needle.length);
      ranges.push(r);
    }
  }
  return ranges;
}

function paint(ranges, current) {
  if (!canPaint()) return;
  if (!ranges.length) {
    CSS.highlights.delete('dshx-find');
    CSS.highlights.delete('dshx-find-current');
    return;
  }
  CSS.highlights.set('dshx-find', new Highlight(...ranges));
  if (ranges[current]) CSS.highlights.set('dshx-find-current', new Highlight(ranges[current]));
}

/** The bar in the transcript's top right; `onQuery` lets the transcript load matching turns. */
export function FindBar({ scroller, pinned, onQuery }) {
  const query = useStore(store, (s) => s.find);
  const focus = useStore(store, (s) => s.findFocus);
  const input = useRef(null);
  const ranges = useRef([]);
  const cur = useRef(0);
  const [state, setState] = useState({ count: 0, current: 0 });
  const open = query != null;

  useEffect(() => {
    if (!open) return undefined;
    const take = () => { input.current?.focus(); input.current?.select(); };
    take();
    // Again after the keystroke's own focus changes settle.
    const id = setTimeout(take, 0);
    return () => clearTimeout(id);
  }, [open, focus]);

  const reveal = (i) => {
    const r = ranges.current[i];
    const el = scroller.current;
    if (!r || !el) return;
    const box = r.getBoundingClientRect();
    const view = el.getBoundingClientRect();
    if (box.top < view.top + 40 || box.bottom > view.bottom - 40) {
      pinned.current = false;
      el.scrollTop += box.top - view.top - el.clientHeight / 3;
    }
  };

  // Searches again as the transcript changes (streaming, loading, unfolding).
  useEffect(() => {
    if (!open) { paint([], 0); return undefined; }
    onQuery?.(query);
    let timer = 0;
    const run = (keep) => {
      const before = ranges.current[cur.current];
      const list = collect(scroller.current?.querySelector('.transcript'), query.trim() ? query : '');
      ranges.current = list;
      if (keep) {
        // The same match, even when turns loaded above it.
        const same = before ? list.findIndex((r) => r.startContainer === before.startContainer && r.startOffset === before.startOffset) : -1;
        cur.current = same >= 0 ? same : Math.min(cur.current, Math.max(0, list.length - 1));
      } else {
        // Start at the first match from the top of the view, else the last one.
        const top = scroller.current?.getBoundingClientRect().top ?? 0;
        const next = list.findIndex((r) => r.getBoundingClientRect().bottom >= top);
        cur.current = next >= 0 ? next : Math.max(0, list.length - 1);
      }
      paint(list, cur.current);
      if (!keep) reveal(cur.current);
      setState({ count: list.length, current: cur.current });
    };
    run(false);
    const root = scroller.current;
    const mo = root ? new MutationObserver(() => { clearTimeout(timer); timer = setTimeout(() => run(true), 150); }) : null;
    mo?.observe(root, { childList: true, subtree: true, characterData: true });
    return () => { mo?.disconnect(); clearTimeout(timer); };
  }, [open, query]);

  useEffect(() => () => paint([], 0), []);
  if (!open) return null;

  const step = (d) => {
    const n = ranges.current.length;
    if (!n) return;
    cur.current = (cur.current + d + n) % n;
    paint(ranges.current, cur.current);
    reveal(cur.current);
    setState({ count: n, current: cur.current });
  };
  const onKey = (e) => {
    if (e.key === 'Enter') { e.preventDefault(); step(e.shiftKey ? -1 : 1); }
    else if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); closeFind(); }
  };
  const count = !query.trim() ? '' : state.count ? t.findCount(state.current + 1, state.count) : t.findNone;
  return html`<div class="find-bar" role="search">
    <span class="find-icon"><${IconSearch} size=${14} /></span>
    <input ref=${input} class="find-input" value=${query} placeholder=${t.findInChat} aria-label=${t.findInChat}
      spellcheck=${false} autocomplete="off" onInput=${(e) => store.set({ find: e.currentTarget.value })} onKeyDown=${onKey} />
    <span class="find-count" role="status" aria-live="polite">${count}</span>
    <button type="button" class="icon-btn find-step is-up" aria-label=${t.findPrev} title=${`${t.findPrev} (⇧↩)`}
      disabled=${!state.count} onClick=${() => step(-1)}><${IconChevronDown} size=${14} /></button>
    <button type="button" class="icon-btn find-step" aria-label=${t.findNext} title=${`${t.findNext} (↩)`}
      disabled=${!state.count} onClick=${() => step(1)}><${IconChevronDown} size=${14} /></button>
    <button type="button" class="icon-btn" aria-label=${t.findClose} title=${`${t.findClose} (esc)`} onClick=${closeFind}><${IconX} size=${14} /></button>
  </div>`;
}
