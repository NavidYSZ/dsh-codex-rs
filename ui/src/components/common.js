import { useEffect, useLayoutEffect, useRef, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { store, api, restoreFocus } from '../state.js';
import { useStore } from '../store.js';

/** Text with a moving highlight while work is in progress (Codex "shimmer"). */
export const Shimmer = ({ active, children }) => html`<span class=${cx('shimmer-wrap', active && 'shimmer')}>${children}</span>`;

/**
 * Popover anchored above or below its trigger; closes on outside click and Escape.
 * `fixed` places it in the viewport (for triggers inside scrolling lists) and
 * `at` ({x, y}) opens it at a point, as a context menu.
 */
export function Menu({ open, onClose, anchor, align = 'start', side = 'top', class: klass, children, fixed, at }) {
  const ref = useRef(null);
  // Who had the focus before the menu (and its search field) took it.
  const opener = useRef(null);
  const wasOpen = useRef(false);
  if (open && !wasOpen.current) opener.current = document.activeElement;
  wasOpen.current = open;
  useLayoutEffect(() => {
    const el = ref.current;
    if (!open || !el || !(fixed || at)) return;
    const box = el.getBoundingClientRect();
    const a = at ? { left: at.x, right: at.x, top: at.y, bottom: at.y } : anchor?.current?.getBoundingClientRect();
    if (!a) return;
    let left = align === 'end' && !at ? a.right - box.width : a.left;
    let top = side === 'top' ? a.top - box.height - 6 : a.bottom + 6;
    if (top + box.height > innerHeight - 8) top = Math.max(8, a.top - box.height - 6);
    if (top < 8) top = Math.min(innerHeight - box.height - 8, a.bottom + 6);
    left = Math.max(8, Math.min(left, innerWidth - box.width - 8));
    el.style.left = `${left}px`;
    el.style.top = `${top}px`;
  }, [open, at?.x, at?.y]);
  useEffect(() => {
    if (!open) return undefined;
    const down = (e) => {
      if (ref.current?.contains(e.target) || anchor?.current?.contains(e.target)) return;
      onClose();
    };
    const key = (e) => { if (e.key === 'Escape') onClose(); };
    document.addEventListener('pointerdown', down, true);
    document.addEventListener('keydown', key);
    return () => {
      document.removeEventListener('pointerdown', down, true);
      document.removeEventListener('keydown', key);
      const back = opener.current && opener.current !== document.body ? opener.current : anchor?.current;
      restoreFocus(back);
    };
  }, [open]);
  if (!open) return null;
  const placed = fixed || at;
  return html`<div ref=${ref} class=${cx('menu', placed ? 'menu-fixed' : [`menu-${side}`, `menu-${align}`].join(' '), klass)} role="menu"
    onClick=${placed ? (e) => { if (e.target.closest('.menu-item')) onClose(); } : undefined}>${children}</div>`;
}

/** The note of `snack()` (state.js) with its action, at the bottom of the window. */
export function Snackbar() {
  const s = useStore(store, (x) => x.snack);
  if (!s) return null;
  return html`<div class="snackbar" role="status" aria-live="polite" key=${s.id}>
    <span>${s.text}</span>
    ${s.action ? html`<button type="button" class="snackbar-btn" onClick=${s.action[1]}>${s.action[0]}</button>` : null}
  </div>`;
}

/** A command in a menu: icon, label, optional shortcut. */
export function ActionItem({ icon, onClick, children, tone, kbd, disabled }) {
  return html`<button type="button" role="menuitem" class=${cx('menu-item', 'menu-action', tone && `tone-${tone}`)} onClick=${onClick} disabled=${disabled}>
    ${icon ? html`<span class="menu-icon">${icon}</span>` : null}
    <span class="menu-label">${children}</span>
    ${kbd ? html`<kbd class="kbd">${kbd}</kbd>` : null}
  </button>`;
}

export function MenuItem({ checked, onClick, children, hint, tone, disabled }) {
  return html`<button type="button" role="menuitemradio" aria-checked=${!!checked} class=${cx('menu-item', tone && `tone-${tone}`)}
    onClick=${onClick} disabled=${disabled}>
    <span class="menu-label">${children}${hint ? html`<span class="menu-hint">${hint}</span>` : null}</span>
    <span class="menu-check">${checked ? html`<svg width="16" height="16" viewBox="0 0 16 16"><path d="m3.5 8.25 2.9 2.9 6.1-6.3" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/></svg>` : null}</span>
  </button>`;
}

/** Resolves an ImageAttachmentRef to a displayable URL through the API. */
export function useImage(ref) {
  const [url, setUrl] = useState(null);
  const id = ref?.attachmentId;
  useEffect(() => {
    if (!id) return undefined;
    let live = true;
    api.loadImage(ref).then((u) => { if (live) setUrl(u); }).catch(() => {});
    return () => { live = false; };
  }, [id]);
  return url;
}

export function Thumb({ image, onRemove, label }) {
  const url = useImage(image);
  return html`<span class="thumb">
    ${url ? html`<img src=${url} alt="" />` : html`<span class="thumb-ph" />`}
    ${onRemove ? html`<button type="button" class="thumb-x" aria-label=${label} onClick=${onRemove}>×</button>` : null}
  </span>`;
}
