// Settings: one pane per section behind a navigation like the Codex app's
// (a rail beside the pane on wide pages, a scrolling tab row on narrow ones),
// each at `#/settings/<section>`: general, appearance, model providers,
// defaults for new chats, usage of all chats, archived chats and about.
import { useEffect, useRef, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t, age } from '../i18n.js';
import { useStore } from '../store.js';
import {
  store, setTheme, updateSettings, closeSettings, toggleSidebar, setArchived, deleteSession, setFollowup, setNotify, openSettings,
} from '../state.js';
import {
  IconChevronLeft, IconSidebar, IconArchive, IconGear, IconSun, IconCube, IconCompose, IconChart, IconInfo,
} from '../icons.js';
import { workspaceName } from './composer.js';
import { ModeSetting } from './modes.js';
import { AppearanceCard } from './worklog.js';
import { UsageCard } from './usage.js';
import { ModelsSection, ModelSelect, modelHint } from './models.js';
import { confirmFullAccess, FullAccessSetting } from './confirm.js';

export function Segmented({ value, options, onChange, label }) {
  return html`<div class="segmented" role="radiogroup" aria-label=${label}>
    ${options.map(([v, text]) => html`<button type="button" key=${v} role="radio" aria-checked=${v === value}
      class=${cx('seg', v === value && 'is-on', v === 'danger-full-access' && 'is-warn')} onClick=${() => onChange(v)}>${text}</button>`)}
  </div>`;
}

export function Field({ label, hint, children }) {
  return html`<div class="field">
    <div class="field-text"><div class="field-label">${label}</div>${hint ? html`<div class="field-hint">${hint}</div>` : null}</div>
    <div class="field-control">${children}</div>
  </div>`;
}

// ---- panes -----------------------------------------------------------------------

function General() {
  const theme = useStore(store, (s) => s.theme);
  const followup = useStore(store, (s) => s.followup);
  const notify = useStore(store, (s) => s.notify);
  const desktop = useStore(store, (s) => !!s.desktop);
  return html`<section class="card">
    <${Field} label=${t.theme}>
      <${Segmented} label=${t.theme} value=${theme} onChange=${setTheme} options=${Object.entries(t.themes)} />
    </${Field}>
    <${Field} label=${t.followup} hint=${t.followupHint}>
      <${Segmented} label=${t.followup} value=${followup} onChange=${setFollowup} options=${Object.entries(t.followups)} />
    </${Field}>
    ${desktop ? null : html`<${Field} label=${t.notifications} hint=${t.notificationsHint}>
      <${Segmented} label=${t.notifications} value=${notify ? 'on' : 'off'} onChange=${(v) => setNotify(v === 'on')}
        options=${[['on', t.enabled], ['off', t.disabledLabel]]} />
    </${Field}>`}
    <${Field} label=${t.language}><span class="field-value">${t.languageValue}</span></${Field}>
  </section>`;
}

function Defaults() {
  const info = useStore(store, (s) => s.info);
  const s = info?.settings ?? {};
  const models = info?.models ?? [];
  const model = models.find((m) => m.id === s.model) ?? models[0];
  const efforts = model?.reasoningEfforts ?? [];
  return html`<section class="card">
    <${ModeSetting} />
    <${Field} label=${t.model} hint=${[modelHint(model), model?.offPeakCost ? t.pickerOffPeak : null].filter(Boolean).join(' · ')}>
      <${ModelSelect} value=${s.model} onChange=${(m) => updateSettings({ model: m })} />
    </${Field}>
    ${efforts.length ? html`<${Field} label=${t.effort}>
      <${Segmented} label=${t.effort} value=${s.effort ?? model?.defaultReasoningEffort} onChange=${(v) => updateSettings({ effort: v })}
        options=${efforts.map((e) => [e, t.efforts[e] ?? e])} />
    </${Field}>` : null}
    <${Field} label=${t.sandbox} hint=${t.access[s.sandbox]?.[1]}>
      <${Segmented} label=${t.sandbox} value=${s.sandbox} onChange=${async (v) => {
        if (v === 'danger-full-access' && s.sandbox !== v && !(await confirmFullAccess('defaults'))) return;
        updateSettings({ sandbox: v });
      }}
        options=${Object.entries(t.access).map(([k, v]) => [k, v[0]])} />
    </${Field}>
    <${FullAccessSetting} />
    <${Field} label=${t.approval}>
      <select class="select" value=${s.approval ?? 'ask'} aria-label=${t.approval} onChange=${(e) => updateSettings({ approval: e.currentTarget.value })}>
        ${Object.entries(t.approvals).map(([k, v]) => html`<option key=${k} value=${k}>${v}</option>`)}
      </select>
    </${Field}>
  </section>`;
}

function Archived() {
  const sessions = useStore(store, (s) => s.sessions);
  const list = sessions.filter((s) => s.archived).sort((a, b) => (b.updatedAt ?? 0) - (a.updatedAt ?? 0));
  if (!list.length) {
    return html`<div class="page-empty st-empty"><${IconArchive} size=${18} /><p>${t.noArchived}</p><p class="st-empty-hint">${t.noArchivedHint}</p></div>`;
  }
  return html`<section class="card">
    ${list.map((s) => html`<div class="field" key=${s.id}>
      <div class="field-text">
        <div class="field-label">${s.title || t.newChat}</div>
        <div class="field-hint">${[s.scratch ? t.noProject : workspaceName(s.cwd), age(s.updatedAt ?? s.createdAt ?? Date.now())].join(' · ')}</div>
      </div>
      <div class="field-control key-row">
        <button type="button" class="btn btn-small" onClick=${() => setArchived(s.id, false)}>${t.unarchive}</button>
        <button type="button" class="btn btn-small" onClick=${() => deleteSession(s.id)}>${t.deleteChat}</button>
      </div>
    </div>`)}
  </section>`;
}

function About() {
  const info = useStore(store, (s) => s.info);
  const tools = info?.tools ?? {};
  return html`<section class="card">
    <${Field} label=${t.version}><span class="field-value mono">${info?.version ?? '–'}</span></${Field}>
    <${Field} label=${t.platform}><span class="field-value mono">${info?.platform ?? '–'}</span></${Field}>
    <${Field} label=${t.macTools}><span class="field-value">${Object.entries(t.toolNames)
    .map(([k, v]) => `${v} ${tools[k] ? t.on : t.off}`).join(' · ')}</span></${Field}>
  </section>`;
}

/** The panes in navigation order: key (`#/settings/<key>`), icon and body. */
const SECTIONS = [
  ['general', IconGear, General],
  ['appearance', IconSun, () => html`<${AppearanceCard} titled=${false} />`],
  ['models', IconCube, () => html`<${ModelsSection} titled=${false} />`],
  ['defaults', IconCompose, Defaults],
  ['usage', IconChart, () => html`<${UsageCard} titled=${false} />`],
  ['archived', IconArchive, Archived],
  ['about', IconInfo, About],
];

function Nav({ current }) {
  const row = useRef(null);
  const [more, setMore] = useState(false);
  // The tab row scrolls sideways on phones: fade the cut edge, keep the current tab in view.
  const measure = () => {
    const el = row.current;
    setMore(!!el && el.scrollLeft + el.clientWidth < el.scrollWidth - 2);
  };
  useEffect(() => {
    row.current?.querySelector('[aria-current="page"]')?.scrollIntoView?.({ block: 'nearest', inline: 'nearest' });
    measure();
  }, [current]);
  useEffect(() => {
    const watch = new ResizeObserver(measure);
    watch.observe(row.current);
    return () => watch.disconnect();
  }, []);
  return html`<nav class=${cx('st-nav', more && 'is-more')} aria-label=${t.settings} ref=${row} onScroll=${measure}>
    ${SECTIONS.map(([key, Icon]) => html`<a key=${key} href=${`#/settings/${key}`} class=${cx('st-nav-item', key === current && 'is-on')}
      aria-current=${key === current ? 'page' : null}
      onClick=${(e) => { if (e.metaKey || e.ctrlKey || e.shiftKey) return; e.preventDefault(); openSettings(key, { replace: true }); }}>
      <${Icon} size=${16} /><span>${t.settingsNav[key]}</span>
    </a>`)}
  </nav>`;
}

export function Settings() {
  const wanted = useStore(store, (s) => s.settingsSection);
  const current = SECTIONS.some(([key]) => key === wanted) ? wanted : SECTIONS[0][0];
  const Body = SECTIONS.find(([key]) => key === current)[2];
  const scroll = useRef(null);
  useEffect(() => { scroll.current?.scrollTo?.({ top: 0 }); }, [current]);
  return html`<div class="settings-page">
    <header class="topbar">
      <button type="button" class="icon-btn topbar-toggle" aria-label=${t.toggleSidebar} onClick=${toggleSidebar}><${IconSidebar} size=${16} /></button>
      <button type="button" class="back-btn" onClick=${closeSettings}><${IconChevronLeft} size=${14} />${t.back}</button>
    </header>
    <div class="scroll" ref=${scroll}><div class="settings st-layout">
      <aside class="st-side">
        <div class="st-side-title">${t.settings}</div>
        <${Nav} current=${current} />
      </aside>
      <div class="st-pane" key=${current}>
        <header class="st-pane-head">
          <h1 class="settings-title">${t.settingsNav[current]}</h1>
          ${t.settingsLeads[current] ? html`<p class="page-lead">${t.settingsLeads[current]}</p>` : null}
        </header>
        <${Body} />
      </div>
    </div></div>
  </div>`;
}
