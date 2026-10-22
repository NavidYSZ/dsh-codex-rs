// Long chats: only the latest turns render, "Load earlier" adds more above
// without moving what is on screen (dsh `chat.loadOlder`), and slim markers on
// the right edge jump to any turn, loading it first when needed (dsh
// `chat.turnNavigation.*`).
import { useEffect, useLayoutEffect, useReducer, useRef, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t } from '../i18n.js';
import { useStore } from '../store.js';
import { store } from '../state.js';

/** Turns rendered when a chat opens and added per "Load earlier". */
export const PAGE = 30;

/**
 * The first rendered turn of the open chat. It is fixed once the chat's log
 * arrived, so new turns grow the window at the bottom instead of pushing
 * old ones out above the reader. `jump(index)` scrolls a turn to the top,
 * rendering it first; `older()` keeps the view where it is.
 */
export function useTurnWindow(turns, scroller, pinned) {
  const activeId = useStore(store, (s) => s.activeId);
  const [, force] = useReducer((n) => n + 1, 0);
  const win = useRef({ id: null, from: 0 });
  const keep = useRef(null);
  const target = useRef(null);
  if (win.current.id !== activeId && turns.length) win.current = { id: activeId, from: Math.max(0, turns.length - PAGE) };
  const from = win.current.id === activeId ? Math.min(win.current.from, turns.length) : 0;

  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el) return;
    if (keep.current) {
      el.scrollTop = el.scrollHeight - keep.current.height + keep.current.top;
      keep.current = null;
    }
    if (target.current != null) {
      const section = el.querySelector(`section.turn[data-turn="${target.current}"]`);
      target.current = null;
      if (section) el.scrollTop += section.getBoundingClientRect().top - el.getBoundingClientRect().top - 12;
    }
  });

  const show = (index) => {
    if (index >= win.current.from) return;
    const el = scroller.current;
    if (el) keep.current = { height: el.scrollHeight, top: el.scrollTop };
    win.current = { id: activeId, from: Math.max(0, index) };
    pinned.current = false;
    force();
  };
  return {
    from,
    older: () => show(Math.max(0, from - PAGE)),
    /** Renders turns from `index` on (find in chat), keeping the view. */
    reveal: show,
    jump: (index) => {
      pinned.current = false;
      target.current = turns[index]?.turn ?? null;
      if (index < win.current.from) show(Math.max(0, index - 2));
      else force();
    },
  };
}

export function LoadOlder({ count, onClick }) {
  return html`<div class="load-older">
    <button type="button" class="btn btn-small" onClick=${onClick}>${t.loadOlder}</button>
    <span class="load-older-count">${t.olderTurns(count)}</span>
  </div>`;
}

/** The prompt a marker previews: the turn's first prompt line, or what started it. */
function promptOf(turn) {
  for (const x of turn.items) {
    if (x.kind === 'user' && x.text) return x.text.split('\n').find((l) => l.trim()) ?? '';
    if (x.kind === 'notice' && (x.prompt || x.text)) return x.prompt || x.text;
  }
  return '';
}

/** Which turn is at the top third of the view. */
function currentTurn(el) {
  const line = el.getBoundingClientRect().top + el.clientHeight / 3;
  let current = null;
  for (const s of el.querySelectorAll('section.turn[data-turn]')) {
    if (s.getBoundingClientRect().top > line) break;
    current = Number(s.dataset.turn);
  }
  return current;
}

const SPACING = 10; // px per marker before markers start to stand for several turns

export function TurnRail({ turns, from, scroller, onJump }) {
  const [current, setCurrent] = useState(null);
  const [slots, setSlots] = useState(40);
  const [tip, setTip] = useState(null);
  useEffect(() => {
    const el = scroller.current;
    if (!el) return undefined;
    let timer = 0;
    const update = () => { timer = 0; setCurrent(currentTurn(el)); };
    // Timers, not animation frames: background tabs keep them.
    const onScroll = () => { timer ||= setTimeout(update, 60); };
    const measure = () => setSlots(Math.max(6, Math.floor((el.clientHeight - 64) / SPACING)));
    el.addEventListener('scroll', onScroll, { passive: true });
    const ro = typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(measure);
    ro?.observe(el);
    measure();
    update();
    return () => { el.removeEventListener('scroll', onScroll); ro?.disconnect(); clearTimeout(timer); };
  }, [turns.length, from]);
  if (turns.length < 2) return null;
  // More turns than room: each marker stands for the turn it jumps to.
  const n = turns.length;
  const picks = n <= slots ? turns.map((_, i) => i)
    : [...new Set(Array.from({ length: slots }, (_, k) => Math.round((k * (n - 1)) / (slots - 1))))];
  const currentIndex = current == null ? n - 1 : turns.findIndex((x) => x.turn === current);
  let active = picks[0];
  for (const i of picks) if (i <= currentIndex) active = i;
  return html`<nav class="turn-rail" aria-label=${t.turnNav} onMouseLeave=${() => setTip(null)}>
    ${picks.map((i) => {
      const turn = turns[i];
      const loaded = i >= from;
      const show = (e) => setTip({ i, y: e.currentTarget.offsetTop + e.currentTarget.offsetHeight / 2 });
      return html`<button type="button" key=${turn.turn} class=${cx('turn-mark', i === active && 'is-current')}
        aria-label=${loaded ? t.turnJump(turn.turn) : t.turnJumpLoad(turn.turn)} aria-current=${i === active ? 'location' : undefined}
        onMouseEnter=${show} onFocus=${show} onBlur=${() => setTip(null)} onClick=${() => onJump(i)}><span /></button>`;
    })}
    ${tip ? html`<div class="turn-tip" style=${`top:${tip.y}px`} aria-hidden="true">
      <span class="turn-tip-n">${t.turnN(turns[tip.i].turn)}</span>
      ${promptOf(turns[tip.i]) ? html`<span class="turn-tip-text">${promptOf(turns[tip.i])}</span>` : null}
    </div>` : null}
  </nav>`;
}
