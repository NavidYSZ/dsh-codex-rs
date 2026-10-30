// The Mac app's instance switcher (top left of the sidebar) with status
// dots, unread counts and ⌃1–⌃9; hovering an instance warms its connection
// up. The page also tells the app when it has rendered (`ready`) and which
// background it paints (`setAppearance`), so switching never flashes grey.
// Bridge contract: desktop/README.md.
import { useRef, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t } from '../i18n.js';
import { useStore } from '../store.js';
import { store, switchInstance } from '../state.js';
import { IconChevronDown, IconGear, IconPlus } from '../icons.js';
import { Menu, ActionItem } from './common.js';

// The Aether mark (brand/aether-mark.svg): a ring with a point on it, one path.
const MARK = 'M4.1 16A11.9 11.9 0 1 1 27.9 16A11.9 11.9 0 1 1 4.1 16ZM7.1 16A8.9 8.9 0 1 0 24.9 16A8.9 8.9 0 1 0 7.1 16ZM19.354 8.646A4 4 0 1 1 27.354 8.646A4 4 0 1 1 19.354 8.646Z';
const bridge = () => window.dshxDesktop;
const clock = (ms) => new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });

function Dot({ status }) {
  if (!status || status === 'idle') return html`<span class="ix-dot is-idle" />`;
  return html`<span class=${cx('ix-dot', `is-${status}`)} />`;
}

// Hover warms a connection up at most every 10 s per instance.
const warmed = new Map();
function prewarm(i) {
  if (i.status === 'ready' || i.status === 'error') return;
  const now = Date.now();
  if (now - (warmed.get(i.id) ?? 0) < 10_000) return;
  warmed.set(i.id, now);
  bridge()?.prewarm?.(i.id);
}

function InstanceRow({ i, n, checked, onPick }) {
  const status = t.instanceStatus[i.status] ?? i.kind;
  const hint = i.status === 'ready' && i.connectedAt ? t.connectedSince(clock(i.connectedAt)) : status;
  return html`<button type="button" role="menuitemradio" aria-checked=${checked} class="menu-item ix-row"
    onPointerEnter=${() => prewarm(i)} onFocus=${() => prewarm(i)} onClick=${onPick}>
    <span class="ix-lead"><${Dot} status=${i.status} /></span>
    <span class="menu-label">${i.name}<span class="menu-hint">${hint}</span></span>
    ${i.unread ? html`<span class="ix-unread" title=${t.unreadIn(i.unread)} aria-label=${t.unreadIn(i.unread)}>${i.unread}</span>` : null}
    ${n < 9 ? html`<kbd class="kbd">⌃${n + 1}</kbd>` : null}
    <span class="menu-check">${checked ? html`<svg width="16" height="16" viewBox="0 0 16 16"><path d="m3.5 8.25 2.9 2.9 6.1-6.3" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/></svg>` : null}</span>
  </button>`;
}

/** Brand in a browser; in the Mac app the current instance, with a switcher. */
export function InstanceSwitcher() {
  const desktop = useStore(store, (s) => s.desktop);
  const [open, setOpen] = useState(false);
  const anchor = useRef(null);
  const list = desktop?.instances ?? [];
  if (!list.length) return html`<span class="brand"><svg width="18" height="18" viewBox="0 0 32 32" aria-hidden="true"
    style="vertical-align:-4px;margin-right:7px"><path fill="currentColor" d=${MARK} /></svg>${t.brand}</span>`;
  // `current` is this page's own instance; `active` is the one the window
  // shows or is switching to.
  const current = list.find((i) => i.id === desktop.current) ?? list.find((i) => i.active) ?? list[0];
  const target = list.find((i) => i.active && i.id !== current.id);
  const pending = target && target.status !== 'ready' ? target : null;
  const others = list.reduce((sum, i) => sum + (i.id === current.id ? 0 : i.unread ?? 0), 0);
  return html`<span class="chip-wrap sb-instance-wrap">
    <button type="button" ref=${anchor} class="sb-instance" aria-haspopup="menu" aria-expanded=${open}
      title=${pending ? t.connectingTo(pending.name) : undefined} onClick=${() => setOpen((v) => !v)}>
      ${pending ? html`<span class="spinner ix-head-spin" role="status" aria-label=${t.connectingTo(pending.name)} />` : null}
      <span class="brand">${(pending ?? current).name}</span>
      ${others && !pending ? html`<span class="ix-head-dot" title=${t.unreadIn(others)} />` : null}
      <${IconChevronDown} size=${12} />
    </button>
    <${Menu} open=${open} onClose=${() => setOpen(false)} anchor=${anchor} fixed side="bottom" align="start" class="menu-wide">
      <div class="menu-head">${t.instances}</div>
      ${list.map((i, n) => html`<${InstanceRow} key=${i.id} i=${i} n=${n} checked=${i.id === (target ?? current).id}
        onPick=${() => { setOpen(false); if (i.id !== current.id || target) switchInstance(i.id); }} />`)}
      <div class="menu-sep" />
      <${ActionItem} icon=${html`<${IconPlus} size=${14} />`} onClick=${() => bridge()?.addInstance?.()}>${t.addInstance}</${ActionItem}>
      <${ActionItem} icon=${html`<${IconGear} size=${14} />`} onClick=${() => bridge()?.editInstances?.()}>${t.manageInstances}</${ActionItem}>
    </${Menu}>
  </span>`;
}

// ---- ready and appearance ---------------------------------------------------

/** `rgb(24, 24, 24)` → `#181818`. */
function hex(color) {
  const m = /^rgba?\((\d+),\s*(\d+),\s*(\d+)/.exec(color);
  return m ? `#${m.slice(1, 4).map((v) => Number(v).toString(16).padStart(2, '0')).join('')}` : null;
}

let lastAppearance = '';
function reportAppearance() {
  const d = bridge();
  if (!d?.setAppearance || !document.body) return;
  const background = hex(getComputedStyle(document.body).backgroundColor);
  const theme = document.documentElement.dataset.theme === 'dark' ? 'dark' : 'light';
  const key = `${background}|${theme}`;
  if (!background || key === lastAppearance) return;
  lastAppearance = key;
  d.setAppearance({ background, theme });
}

let readySent = false;
/** After the first full render (sidebar and the chat or start page) has been painted. */
function signalReady() {
  if (readySent) return;
  readySent = true;
  let sent = false;
  const send = () => { if (!sent) { sent = true; reportAppearance(); bridge()?.ready?.(); } };
  requestAnimationFrame(() => setTimeout(send, 0));
  setTimeout(send, 150); // a page in the background gets no animation frames
}

if (bridge()) {
  reportAppearance();
  new MutationObserver(reportAppearance).observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] });
  const opensChat = location.hash.startsWith('#/s/');
  const bootedAt = Date.now();
  const check = () => {
    const s = store.get();
    if (s.boot === 'loading') return;
    // A chat link waits for its conversation (at most 1.5 s).
    if (s.boot === 'ready' && opensChat && !s.header && Date.now() - bootedAt < 1500) {
      setTimeout(check, 100);
      return;
    }
    signalReady();
  };
  store.subscribe(check);
  check();
}
