// Small parts the Plugins page's tabs share: logos, the "…" menu, actions
// that reload their lists, status pills and waiting for a sign-in.
import { useEffect, useRef, useState } from 'preact/hooks';
import { html } from '../html.js';
import { t } from '../i18n.js';
import { useStore } from '../store.js';
import { store, api, fail, toast } from '../state.js';
import { IconMore } from '../icons.js';
import { Menu, ActionItem } from './common.js';
import { Pill } from './page.js';
import { Portal } from './drawer.js';

/** Runs one change, then reloads; `busy` names the change while it runs. */
export function useAction(...reloads) {
  const [busy, setBusy] = useState(null);
  const run = async (key, fn) => {
    setBusy(key);
    try { return await fn(); } catch (e) { fail(e); return null; } finally {
      setBusy(null);
      for (const r of reloads) r.reload();
      api.info().then((info) => store.set({ info })).catch(() => {});
    }
  };
  return [busy, run];
}

/** A calm colour per name for initials without a brand colour. */
export function tint(name) {
  let h = 0;
  for (const ch of name || '') h = (h * 31 + ch.charCodeAt(0)) % 360;
  return `hsl(${h} 38% 48%)`;
}

/** Plugin or server logo; without one, `letters` initials on `color`. */
export function Logo({ url, color, name, size, letters = 1 }) {
  const [broken, setBroken] = useState(false);
  const box = size ? `width:${size}px;height:${size}px;font-size:${Math.round(size * 0.4)}px` : '';
  if (url && !broken) return html`<img class="plug-logo" style=${box || undefined} src=${api.assetUrl(url)} alt="" onError=${() => setBroken(true)} />`;
  const words = (name || '?').split(/[^\p{L}\p{N}]+/u).filter(Boolean);
  const initials = (letters > 1 && words.length > 1 ? words.slice(0, letters).map((w) => w[0]).join('') : (words[0] ?? '?').slice(0, 1)).toUpperCase();
  const style = [color ? `background:${color}` : '', box].filter(Boolean).join(';');
  return html`<span class="plug-logo plug-initial" style=${style || undefined} aria-hidden="true">${initials}</span>`;
}

/** "…" button with a menu of `[icon, label, onClick, tone]` items (null items are skipped). */
export function MoreMenu({ items, label }) {
  const [open, setOpen] = useState(false);
  const anchor = useRef(null);
  return html`<span class="chip-wrap">
    <button type="button" ref=${anchor} class="icon-btn" aria-label=${label || t.more} title=${label || t.more} aria-haspopup="menu"
      aria-expanded=${open} onClick=${(e) => { e.stopPropagation(); setOpen((v) => !v); }}>
      <${IconMore} size=${15} />
    </button>
    <${Menu} open=${open} onClose=${() => setOpen(false)} anchor=${anchor} fixed side="bottom" align="end" class="menu-actions">
      ${items.filter(Boolean).map(([icon, text, fn, tone, disabled]) => html`<${ActionItem} key=${text} icon=${icon} tone=${tone}
        disabled=${disabled} onClick=${(e) => { e.stopPropagation(); setOpen(false); fn(); }}>${text}</${ActionItem}>`)}
    </${Menu}>
  </span>`;
}

const STATUS = {
  connected: ['ok', 'connected'], 'needs-login': ['warn', 'needsLogin'], connecting: ['busy', 'connecting'],
  error: ['error', 'statusError'], disabled: ['off', 'statusDisabled'], stopped: ['off', 'statusStopped'],
};

export const statusPill = (status) => {
  const [tone, key] = STATUS[status] ?? ['off', 'notAdded'];
  return html`<${Pill} tone=${tone}>${t[key]}</${Pill}>`;
};

/** A dot and the status in words ("● Connected"); a failed server that retries is a warning, not an error. */
export function StatusDot({ status, retrying }) {
  const [tone, key] = STATUS[status] ?? ['off', 'notAdded'];
  return html`<span class=${`mcp-status is-${retrying ? 'warn' : tone}`}><span class="mcp-dot" aria-hidden="true" />${retrying ? t.mcpRetrying : t[key]}</span>`;
}

/** Opens a sign-in page; the Mac app hands it to the default browser. */
export function openExternal(url) {
  window.open(url, '_blank', 'noopener');
}

/**
 * After a sign-in page opened: `wait(server)` polls the server list until
 * that server is connected (3 min), then calls `done()`.
 */
export function useLoginWait(done) {
  const [waiting, setWaiting] = useState(null);
  useEffect(() => {
    if (!waiting) return undefined;
    const until = Date.now() + 180_000;
    const id = setInterval(async () => {
      try {
        const servers = await api.mcpServers();
        const s = servers.find((x) => x.name === waiting.server);
        if (s?.status === 'connected' || Date.now() > until) { setWaiting(null); done(); }
      } catch { /* keep polling */ }
    }, 2000);
    return () => clearInterval(id);
  }, [waiting]);
  /** Starts the OAuth login of `server` and waits for it; `key` marks what waits. */
  const login = async (server, key = server) => {
    const r = await api.mcpAction(server, 'login');
    if (r?.authorizationUrl) {
      openExternal(r.authorizationUrl);
      toast(t.loginOpened);
      setWaiting({ server, key });
    }
    return r;
  };
  return { waiting, login };
}

const routes = new Map();

/**
 * Whether the server has the write-only route `path`: the UI ships with
 * its server, so yes, until a write answered 404/405 ([`routeMissing`]).
 * No probe: it cost a 405 and a console error on every visit.
 */
export function useRoute(path) {
  return routes.get(path) ?? true;
}

/** Remembers that a settings route answered 404/405 after all. */
export function routeMissing(path) {
  routes.set(path, false);
}

/** A route this server does not have (not an unknown skill or server). */
export const isMissingRoute = (e) => e?.status === 405 || (e?.status === 404 && !/^not found:/i.test(e?.message ?? ''));

/**
 * Toasts of the Plugins page (results and errors), above its drawers; the
 * chat shows them in the composer, so they are dropped when the page closes.
 */
export function PageToast() {
  const text = useStore(store, (s) => s.toast);
  useEffect(() => () => store.set({ toast: null }), []);
  return text ? html`<${Portal}><div class="xe-toast" role="status" aria-live="polite">${text}</div></${Portal}>` : null;
}

const FOCUS_LABEL = { skill: (n) => `/${n}`, server: (n) => `@${n}`, plugin: (n) => `@${n}` };

/**
 * A pill or chip asked for one skill, server or plugin (`store.pluginFocus`,
 * set by `openDetails`): once `list` has loaded, `find(name)` opens it and
 * returns whether it exists; a missing one gets a toast. A failed load drops
 * the request.
 */
export function usePluginFocus(kind, list, find) {
  const focus = useStore(store, (s) => s.pluginFocus);
  useEffect(() => {
    if (focus?.kind !== kind || list.loading) return;
    store.set({ pluginFocus: null });
    if (list.data && !find(focus.name)) toast(t.focusMissing(FOCUS_LABEL[kind](focus.name)));
  }, [focus, list.loading, list.data]);
}
