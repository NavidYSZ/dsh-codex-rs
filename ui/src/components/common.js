import { useEffect, useRef, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { api } from '../state.js';

/** Text with a moving highlight while work is in progress (Codex "shimmer"). */
export const Shimmer = ({ active, children }) => html`<span class=${cx('shimmer-wrap', active && 'shimmer')}>${children}</span>`;

/** Popover anchored above or below its trigger; closes on outside click and Escape. */
export function Menu({ open, onClose, anchor, align = 'start', side = 'top', class: klass, children }) {
  const ref = useRef(null);
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
    };
  }, [open]);
  if (!open) return null;
  return html`<div ref=${ref} class=${cx('menu', `menu-${side}`, `menu-${align}`, klass)} role="menu">${children}</div>`;
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
