// Shared frame of the full-width pages (Plugins, Automations): top bar with
// back button, a title, a lead line and a scrolling body.
import { useEffect, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t } from '../i18n.js';
import { closeSettings, toggleSidebar, fail } from '../state.js';
import { IconChevronLeft, IconSidebar } from '../icons.js';

export function PageShell({ title, lead, actions, children, wide }) {
  return html`<div class="settings-page">
    <header class="topbar">
      <button type="button" class="icon-btn topbar-toggle" aria-label=${t.toggleSidebar} onClick=${toggleSidebar}><${IconSidebar} size=${16} /></button>
      <button type="button" class="back-btn" onClick=${closeSettings}><${IconChevronLeft} size=${14} />${t.back}</button>
    </header>
    <div class="scroll"><div class=${cx('settings', wide && 'is-wide')}>
      <div class="page-head">
        <div><h1 class="settings-title">${title}</h1>${lead ? html`<p class="page-lead">${lead}</p>` : null}</div>
        ${actions ? html`<div class="page-actions">${actions}</div>` : null}
      </div>
      ${children}
    </div></div>
  </div>`;
}

/** Tabs as a segmented row; `value` is one of the keys of `labels`. */
export function Tabs({ value, labels, onChange }) {
  return html`<div class="page-tabs" role="tablist">
    ${Object.entries(labels).map(([k, label]) => html`<button type="button" key=${k} role="tab" aria-selected=${k === value}
      class=${cx('page-tab', k === value && 'is-on')} onClick=${() => onChange(k)}>${label}</button>`)}
  </div>`;
}

/** Loads with `fn` on mount and on `reload()`; errors become a toast. */
export function useLoad(fn, deps = []) {
  const [state, setState] = useState({ loading: true, data: null });
  const [nonce, setNonce] = useState(0);
  useEffect(() => {
    let live = true;
    fn().then(
      (data) => { if (live) setState({ loading: false, data }); },
      (e) => { if (live) { setState({ loading: false, data: null, error: e }); if (e?.status !== 404) fail(e); } },
    );
    return () => { live = false; };
  }, [nonce, ...deps]);
  return { ...state, reload: () => setNonce((n) => n + 1) };
}

/** On/off switch. */
export function Toggle({ on, onChange, label, disabled }) {
  return html`<button type="button" role="switch" aria-checked=${!!on} aria-label=${label} disabled=${disabled}
    class=${cx('toggle', on && 'is-on')} onClick=${() => onChange(!on)}><span class="toggle-knob" /></button>`;
}

/** Status pill: `tone` is ok, warn, error, busy or off. */
export const Pill = ({ tone, children }) => html`<span class=${cx('pill', `pill-${tone}`)}>${children}</span>`;
