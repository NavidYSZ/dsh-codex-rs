// Layers of the Skills and MCP pages: the detail drawer (right side; a
// bottom sheet on a phone) and modal dialogs. Both live on <body>, keep
// Tab inside, close the topmost layer on Esc (an open menu or the folder
// browser first) and give focus back to the element that opened them.
// Dialogs reuse the look of the Models dialogs (styles/models.css).
import { render } from 'preact';
import { useEffect, useLayoutEffect, useRef } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t } from '../i18n.js';
import { restoreFocus } from '../state.js';
import { IconX } from '../icons.js';

/** Renders its children in a layer on <body>. */
export function Portal({ children }) {
  const host = useRef(null);
  if (!host.current) host.current = document.createElement('div');
  useLayoutEffect(() => {
    const el = host.current;
    document.body.appendChild(el);
    return () => { render(null, el); el.remove(); };
  }, []);
  useLayoutEffect(() => { render(children, host.current); });
  return null;
}

const FOCUSABLE = 'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';
const layers = [];

/**
 * Keeps focus in `ref` while it is the topmost layer and closes it on Esc.
 * `initial` picks the element focused on open (selector inside the layer).
 */
export function useLayer(ref, onClose, initial) {
  const id = useRef(Symbol('layer'));
  const close = useRef(onClose);
  close.current = onClose;
  useEffect(() => {
    const opener = document.activeElement;
    layers.push(id.current);
    const root = ref.current;
    const first = (initial && root?.querySelector(initial)) || root?.querySelector(FOCUSABLE);
    first?.focus({ preventScroll: true });
    const top = () => layers[layers.length - 1] === id.current;
    const key = (e) => {
      if (!top()) return;
      if (e.key === 'Escape') {
        // A menu or the folder browser above us closes first.
        if (document.querySelector('.menu, .fp-scrim')) return;
        e.preventDefault();
        e.stopPropagation();
        close.current();
      } else if (e.key === 'Tab' && root) {
        const all = [...root.querySelectorAll(FOCUSABLE)].filter((el) => el.offsetParent !== null || el === document.activeElement);
        if (!all.length) return;
        // Focus lost with a removed element (a step changed): back inside.
        if (!root.contains(document.activeElement)) { e.preventDefault(); all[0].focus(); return; }
        const i = all.indexOf(document.activeElement);
        if (e.shiftKey && i <= 0) { e.preventDefault(); all[all.length - 1].focus(); }
        else if (!e.shiftKey && i === all.length - 1) { e.preventDefault(); all[0].focus(); }
      }
    };
    document.addEventListener('keydown', key, true);
    return () => {
      document.removeEventListener('keydown', key, true);
      layers.splice(layers.indexOf(id.current), 1);
      restoreFocus(opener);
    };
  }, []);
}

/**
 * Detail drawer: `head` (icon, title, meta), optional `tabs`
 * (`{key: label}` with `tab`/`onTab`), then the scrolling body.
 */
export function Drawer({ label, head, actions, tabs, tab, onTab, onClose, footer, children }) {
  const box = useRef(null);
  useLayer(box, onClose, '.dr-close');
  return html`<${Portal}>
    <div class="dr-layer">
      <div class="dr-scrim" onClick=${onClose} />
      <aside ref=${box} class="dr-panel" role="dialog" aria-modal="true" aria-label=${label}>
        <span class="dr-grip" aria-hidden="true" />
        <header class="dr-head">
          <div class="dr-headline">${head}</div>
          <div class="dr-actions">
            ${actions}
            <button type="button" class="icon-btn dr-close" aria-label=${t.close} title=${`${t.close} (Esc)`} onClick=${onClose}><${IconX} size=${14} /></button>
          </div>
        </header>
        ${tabs ? html`<div class="dr-tabs" role="tablist">
          ${Object.entries(tabs).map(([k, text]) => html`<button type="button" key=${k} role="tab" aria-selected=${k === tab}
            class=${cx('dr-tab', k === tab && 'is-on')} onClick=${() => onTab(k)}>${text}</button>`)}
        </div>` : null}
        <div class="dr-body">${children}</div>
        ${footer ? html`<footer class="dr-foot">${footer}</footer>` : null}
      </aside>
    </div>
  </${Portal}>`;
}

/** The first field of a dialog, else its first choice, else the footer. */
const FIRST_FIELD = 'input:not([disabled]):not([type="checkbox"]), textarea, select, .mo-choice, .md-foot .btn';

/**
 * Modal dialog in the Models dialogs' look; `wide` for forms. A new `step`
 * (multi-step dialogs) focuses the first field again.
 */
export function Dialog({ title, description, onClose, footer, children, wide, back, step }) {
  const box = useRef(null);
  useLayer(box, onClose, FIRST_FIELD);
  const first = useRef(true);
  useEffect(() => {
    if (first.current) { first.current = false; return; }
    box.current?.querySelector(FIRST_FIELD)?.focus({ preventScroll: true });
  }, [step]);
  return html`<${Portal}>
    <div class="md-scrim xd-scrim" onMouseDown=${(e) => { if (e.target === e.currentTarget) onClose(); }}>
      <div ref=${box} class=${cx('md-dialog', wide && 'md-editor')} role="dialog" aria-modal="true" aria-label=${title}>
        <header class="md-head">
          ${back ? html`<button type="button" class="icon-btn xd-back" aria-label=${t.back} title=${t.back} onClick=${back}>
            <svg width="14" height="14" viewBox="0 0 16 16" aria-hidden="true"><path d="M9.75 4.5 6.25 8l3.5 3.5" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"/></svg>
          </button>` : null}
          <div class="md-heading"><h2 class="md-title">${title}</h2>${description ? html`<p class="md-desc">${description}</p>` : null}</div>
          <button type="button" class="icon-btn md-close" aria-label=${t.close} onClick=${onClose}><${IconX} size=${14} /></button>
        </header>
        <div class="md-body">${children}</div>
        ${footer ? html`<footer class="md-foot">${footer}</footer>` : null}
      </div>
    </div>
  </${Portal}>`;
}

/** A labelled form field (label, control, hint or error). */
export function Field({ label, hint, error, children, wide, as = 'label' }) {
  const body = html`<span class="md-label">${label}</span>${children}
    ${error ? html`<span class="md-error" role="alert">${error}</span>` : hint ? html`<span class="md-hint">${hint}</span>` : null}`;
  return as === 'label'
    ? html`<label class=${cx('md-field', wide && 'is-wide')}>${body}</label>`
    : html`<div class=${cx('md-field', wide && 'is-wide')} role="group" aria-label=${label}>${body}</div>`;
}

/** Segmented choice: `options` is `[[value, label], …]`. */
export function Segmented({ value, options, onChange, label }) {
  const onKeyDown = (e) => {
    const i = options.findIndex(([v]) => v === value);
    const step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[e.key];
    if (!step) return;
    e.preventDefault();
    const next = options[(i + step + options.length) % options.length][0];
    onChange(next);
    e.currentTarget.querySelector(`[data-value="${CSS.escape(next)}"]`)?.focus();
  };
  return html`<div class="xd-seg" role="radiogroup" aria-label=${label} onKeyDown=${onKeyDown}>
    ${options.map(([v, text]) => html`<button type="button" key=${v} role="radio" data-value=${v} aria-checked=${v === value}
      tabindex=${v === value ? 0 : -1} class=${cx('xd-seg-item', v === value && 'is-on')} onClick=${() => onChange(v)}>${text}</button>`)}
  </div>`;
}
