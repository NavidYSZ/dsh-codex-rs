// Agent modes (dsh agent presets): the mode chip on the start page, the
// read-only mode of a running chat in the top bar, the "When to choose?"
// guide and the default in Settings. A chat keeps the mode it started with;
// another mode means a new chat. Copy follows dsh's agent-preset guide.
import { useEffect, useRef, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t } from '../i18n.js';
import { useStore } from '../store.js';
import { store, setComposer, newChat, setDraft, updateSettings } from '../state.js';
import { IconCopy, IconCheck, IconX } from '../icons.js';
import { Menu, MenuItem, ActionItem } from './common.js';

const S = { fill: 'none', stroke: 'currentColor', 'stroke-width': 1.3, 'stroke-linecap': 'round', 'stroke-linejoin': 'round' };
/** Stacked layers: a mode composes tools and prompt. */
export const IconMode = ({ size = 16 } = {}) => html`<svg class="icon" width=${size} height=${size} viewBox="0 0 16 16" aria-hidden="true">
  <path d="M8 2 14 5.1 8 8.2 2 5.1 8 2Z" ...${S}/><path d="m2 8.1 6 3.1 6-3.1M2 11l6 3.1 6-3.1" ...${S}/></svg>`;
const IconHelp = ({ size = 14 } = {}) => html`<svg class="icon" width=${size} height=${size} viewBox="0 0 16 16" aria-hidden="true">
  <circle cx="8" cy="8" r="6.25" ...${S}/><path d="M6.2 6.2a1.85 1.85 0 1 1 2.6 1.7c-.5.25-.8.6-.8 1.15v.35" ...${S}/><circle cx="8" cy="11.4" r=".75" fill="currentColor"/></svg>`;

/** The server's modes; none when it predates presets (then nothing shows). */
function usePresets() {
  return useStore(store, (s) => s.info?.presets ?? []);
}

/** Localized name and texts of a mode; unknown ids use the server's copy. */
export function modeText(id, presets = store.get().info?.presets ?? []) {
  const own = t.modes[id];
  const server = presets.find((p) => p.id === id);
  return {
    name: own?.name ?? server?.name ?? id,
    hint: own?.hint ?? server?.description ?? '',
    description: own?.description ?? server?.description ?? '',
  };
}

/** The mode the next new chat starts with: the start-page pick, else the default. */
export function useStagedMode() {
  const pick = useStore(store, (s) => s.composer.preset);
  const fallback = useStore(store, (s) => s.info?.settings?.defaultPreset ?? 'standard');
  return pick ?? fallback;
}

/** The mode of the open chat (sessions from before modes run Standard). */
export function useChatMode() {
  return useStore(store, (s) => s.header?.agentPreset
    ?? s.sessions.find((x) => x.id === s.activeId)?.preset ?? 'standard');
}

/** A new chat in `id` mode, in the open chat's folder (or without a project). */
function newChatWith(id) {
  const s = store.get();
  const row = s.sessions.find((x) => x.id === s.activeId);
  const cwd = row?.scratch ? '' : s.header?.cwd ?? row?.cwd ?? s.cwd;
  newChat(cwd);
  setComposer({ preset: id });
}

export function openModeGuide(id, from = 'new') {
  store.set({ modeGuide: { id, from, opener: document.activeElement } });
}

/** Start page: the mode of the new chat, beside the folder and access chips. */
export function ModeChip() {
  const presets = usePresets();
  const staged = useStagedMode();
  const [open, setOpen] = useState(false);
  const anchor = useRef(null);
  if (presets.length < 2) return null;
  const current = modeText(staged, presets);
  return html`<span class="chip-wrap">
    <button type="button" ref=${anchor} class="chip chip-plain chip-mode" aria-haspopup="menu" aria-expanded=${open}
      title=${t.modeSeat} aria-label=${`${t.mode}: ${current.name}`} onClick=${() => setOpen((v) => !v)}>
      <${IconMode} size=${14} /><span class="chip-label">${current.name}</span>
    </button>
    <${Menu} open=${open} onClose=${() => setOpen(false)} anchor=${anchor} fixed class="menu-wide mode-menu">
      <div class="menu-head">${t.mode}</div>
      ${presets.map((p) => {
    const text = modeText(p.id, presets);
    return html`<${MenuItem} key=${p.id} checked=${p.id === staged} hint=${text.hint}
          onClick=${() => { setComposer({ preset: p.id }); setOpen(false); }}>${text.name}</${MenuItem}>`;
  })}
      <div class="menu-sep" />
      <${ActionItem} icon=${html`<${IconHelp} />`} onClick=${() => { setOpen(false); openModeGuide(staged, 'new'); }}>${t.modeWhen}</${ActionItem}>
    </${Menu}>
  </span>`;
}

/** Top bar of a chat: its mode, read-only, with a way to another mode. */
export function ModeLabel() {
  const presets = usePresets();
  const mode = useChatMode();
  const loaded = useStore(store, (s) => !!s.header);
  const [open, setOpen] = useState(false);
  const anchor = useRef(null);
  if (presets.length < 2 || !loaded) return null;
  const current = modeText(mode, presets);
  return html`<span class="chip-wrap mode-label-wrap">
    <button type="button" ref=${anchor} class=${cx('mode-label', mode === 'standard' && 'is-standard')} aria-haspopup="menu"
      aria-expanded=${open} title=${t.modeOfChat} aria-label=${`${t.mode}: ${current.name}`} onClick=${() => setOpen((v) => !v)}>
      <${IconMode} size=${13} /><span>${current.name}</span>
    </button>
    <${Menu} open=${open} onClose=${() => setOpen(false)} anchor=${anchor} side="bottom" align="start" fixed class="menu-wide mode-menu">
      <div class="menu-head">${t.modeOfChat}</div>
      <div class="mode-current">
        <div class="mode-current-name">${current.name}</div>
        <div class="mode-current-text">${current.description}</div>
        <div class="mode-current-note">${t.modeFixed}</div>
      </div>
      <div class="menu-sep" />
      ${presets.map((p) => html`<${ActionItem} key=${p.id} icon=${html`<${IconMode} size=${14} />`}
        onClick=${() => { setOpen(false); newChatWith(p.id); }}>${t.modeNewChat(modeText(p.id, presets).name)}</${ActionItem}>`)}
      <${ActionItem} icon=${html`<${IconHelp} />`} onClick=${() => { setOpen(false); openModeGuide(mode, 'chat'); }}>${t.modeWhen}</${ActionItem}>
    </${Menu}>
  </span>`;
}

/** Settings → defaults for new chats: the mode. */
export function ModeSetting() {
  const presets = usePresets();
  const value = useStore(store, (s) => s.info?.settings?.defaultPreset ?? 'standard');
  if (presets.length < 2) return null;
  return html`<div class="field">
    <div class="field-text">
      <div class="field-label">${t.mode}</div>
      <div class="field-hint">${modeText(value, presets).hint}
        <button type="button" class="link-btn mode-why" onClick=${() => openModeGuide(value, 'settings')}>${t.modeWhen}</button></div>
    </div>
    <div class="field-control">
      <div class="segmented" role="radiogroup" aria-label=${t.mode}>
        ${presets.map((p) => html`<button type="button" key=${p.id} role="radio" aria-checked=${p.id === value}
          class=${cx('seg', p.id === value && 'is-on')} onClick=${() => updateSettings({ defaultPreset: p.id })}>${modeText(p.id, presets).name}</button>`)}
      </div>
    </div>
  </div>`;
}

function CopyButton({ text }) {
  const [done, setDone] = useState(false);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setDone(true);
      setTimeout(() => setDone(false), 1500);
    } catch { /* clipboard blocked */ }
  };
  return html`<button type="button" class="mode-mini" onClick=${copy} aria-label=${done ? t.copied : t.copy}>
    ${done ? html`<${IconCheck} size=${13} />` : html`<${IconCopy} size=${13} />`}<span>${done ? t.copied : t.copy}</span>
  </button>`;
}

/** Keeps Tab inside the guide (dsh `trapPresetReaderTab`). */
function trapTab(e) {
  if (e.key !== 'Tab') return;
  const items = [...e.currentTarget.querySelectorAll('button:not([disabled]), [tabindex="0"]')];
  const first = items[0];
  const last = items[items.length - 1];
  if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last?.focus(); }
  else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first?.focus(); }
}

/** "When to choose?": what each mode includes and example tasks to try. */
export function ModeGuide() {
  const guide = useStore(store, (s) => s.modeGuide);
  const presets = usePresets();
  const [tab, setTab] = useState(null);
  const dialog = useRef(null);
  const id = tab ?? guide?.id;
  useEffect(() => {
    setTab(null);
    if (guide) requestAnimationFrame(() => dialog.current?.querySelector('[aria-selected="true"]')?.focus());
  }, [guide]);
  if (!guide || presets.length < 2) return null;
  const close = () => {
    const opener = guide.opener;
    store.set({ modeGuide: null });
    if (opener?.isConnected) opener.focus();
  };
  const text = modeText(id, presets);
  const copy = t.modeGuides[id];
  const server = presets.find((p) => p.id === id)?.help;
  const pick = () => {
    close();
    if (guide.from === 'new') setComposer({ preset: id });
    else if (guide.from === 'settings') updateSettings({ defaultPreset: id });
    else newChatWith(id);
  };
  const tryIt = (prompt) => {
    close();
    newChatWith(id);
    setDraft(prompt);
  };
  const onKey = (e) => {
    if (e.key === 'Escape') { e.preventDefault(); close(); return; }
    if ((e.key === 'ArrowRight' || e.key === 'ArrowLeft') && e.target.getAttribute('role') === 'tab') {
      e.preventDefault();
      const i = presets.findIndex((p) => p.id === id);
      const next = presets[(i + (e.key === 'ArrowRight' ? 1 : presets.length - 1)) % presets.length].id;
      setTab(next);
      requestAnimationFrame(() => dialog.current?.querySelector(`[data-mode="${next}"]`)?.focus());
      return;
    }
    trapTab(e);
  };
  const action = guide.from === 'new' ? t.modeUse(text.name)
    : guide.from === 'settings' ? t.modeMakeDefault(text.name) : t.modeNewChat(text.name);
  return html`<div class="mode-scrim" onMouseDown=${(e) => { if (e.target === e.currentTarget) close(); }}>
    <div ref=${dialog} class="mode-guide" role="dialog" aria-modal="true" aria-labelledby="mode-guide-title" onKeyDown=${onKey}>
      <header class="mode-guide-head">
        <h2 id="mode-guide-title" class="mode-guide-title">${t.modeGuideTitle}</h2>
        <button type="button" class="icon-btn" aria-label=${t.close} title=${t.close} onClick=${close}><${IconX} size=${16} /></button>
      </header>
      <div class="mode-tabs" role="tablist" aria-label=${t.mode}>
        ${presets.map((p) => html`<button type="button" key=${p.id} role="tab" data-mode=${p.id} aria-selected=${p.id === id}
          tabindex=${p.id === id ? 0 : -1} class=${cx('mode-tab', p.id === id && 'is-on')} onClick=${() => setTab(p.id)}>
          <${IconMode} size=${14} />${modeText(p.id, presets).name}</button>`)}
      </div>
      <div class="mode-guide-body" role="tabpanel" aria-label=${text.name} key=${id}>
        ${copy ? html`
          <p class="mode-intro">${copy.intro}</p>
          ${copy.sections.map(([h, p]) => html`<h3>${h}</h3><p>${p}</p>`)}
          <h3>${t.modeHowTo}</h3>
          ${copy.examples.map((ex) => html`<div class="mode-example">
            <div class="mode-example-head">
              <span class="mode-example-title">${ex.title}</span>
              <${CopyButton} text=${ex.prompt} />
              <button type="button" class="mode-mini" onClick=${() => tryIt(ex.prompt)}>${t.modeTry}</button>
            </div>
            <blockquote>${ex.prompt}</blockquote>
            <p class="mode-example-note">${ex.note}</p>
          </div>`)}`
    : html`<p class="mode-intro">${server?.intro ?? text.description}</p>`}
      </div>
      <footer class="mode-guide-foot">
        <span class="mode-guide-fixed">${t.modeFixed}</span>
        <button type="button" class="btn btn-small btn-primary" onClick=${pick}>${action}</button>
      </footer>
    </div>
  </div>`;
}
