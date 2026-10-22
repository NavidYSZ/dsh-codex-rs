// Settings: theme (local to this browser), defaults for new chats, model
// providers, appearance and the usage of all chats.
import { html, cx } from '../html.js';
import { t, age } from '../i18n.js';
import { useStore } from '../store.js';
import {
  store, setTheme, updateSettings, closeSettings, toggleSidebar, setArchived, deleteSession, setFollowup, setNotify,
} from '../state.js';
import { IconChevronLeft, IconSidebar, IconArchive } from '../icons.js';
import { workspaceName } from './composer.js';
import { ModeSetting } from './modes.js';
import { AppearanceCard } from './worklog.js';
import { UsageCard } from './usage.js';
import { ModelsSection, ModelSelect } from './models.js';
import { confirmFullAccess } from './confirm.js';

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

function Archived() {
  const sessions = useStore(store, (s) => s.sessions);
  const list = sessions.filter((s) => s.archived).sort((a, b) => (b.updatedAt ?? 0) - (a.updatedAt ?? 0));
  return html`<section class="card">
    <h2 class="card-title"><${IconArchive} size=${14} />${t.archived}</h2>
    ${list.length ? list.map((s) => html`<div class="field" key=${s.id}>
      <div class="field-text">
        <div class="field-label">${s.title || t.newChat}</div>
        <div class="field-hint">${[s.scratch ? t.noProject : workspaceName(s.cwd), age(s.updatedAt ?? s.createdAt ?? Date.now())].join(' · ')}</div>
      </div>
      <div class="field-control key-row">
        <button type="button" class="btn btn-small" onClick=${() => setArchived(s.id, false)}>${t.unarchive}</button>
        <button type="button" class="btn btn-small" onClick=${() => deleteSession(s.id)}>${t.deleteChat}</button>
      </div>
    </div>`) : html`<div class="field"><span class="field-hint">${t.noArchived}</span></div>`}
  </section>`;
}

export function Settings() {
  const info = useStore(store, (s) => s.info);
  const theme = useStore(store, (s) => s.theme);
  const followup = useStore(store, (s) => s.followup);
  const notify = useStore(store, (s) => s.notify);
  const desktop = useStore(store, (s) => !!s.desktop);
  const s = info?.settings ?? {};
  const models = info?.models ?? [];
  const model = models.find((m) => m.id === s.model) ?? models[0];
  const efforts = model?.reasoningEfforts ?? [];
  const tools = info?.tools ?? {};
  return html`<div class="settings-page">
    <header class="topbar">
      <button type="button" class="icon-btn topbar-toggle" aria-label=${t.toggleSidebar} onClick=${toggleSidebar}><${IconSidebar} size=${16} /></button>
      <button type="button" class="back-btn" onClick=${closeSettings}><${IconChevronLeft} size=${14} />${t.back}</button>
    </header>
    <div class="scroll"><div class="settings">
      <h1 class="settings-title">${t.settings}</h1>

      <${ModelsSection} />

      <section class="card">
        <h2 class="card-title">${t.defaults}</h2>
        <${ModeSetting} />
        <${Field} label=${t.model}>
          <${ModelSelect} value=${s.model} onChange=${(model) => updateSettings({ model })} />
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
        <${Field} label=${t.approval}>
          <select class="select" value=${s.approval ?? 'ask'} aria-label=${t.approval} onChange=${(e) => updateSettings({ approval: e.currentTarget.value })}>
            ${Object.entries(t.approvals).map(([k, v]) => html`<option key=${k} value=${k}>${v}</option>`)}
          </select>
        </${Field}>
      </section>

      <section class="card">
        <h2 class="card-title">${t.general}</h2>
        <${Field} label=${t.theme}>
          <${Segmented} label=${t.theme} value=${theme} onChange=${setTheme} options=${Object.entries(t.themes)} />
        </${Field}>
        <${Field} label=${t.followup} hint=${t.followupHint}>
          <${Segmented} label=${t.followup} value=${followup} onChange=${setFollowup} options=${Object.entries(t.followups)} />
        </${Field}>
        ${desktop ? null : html`<${Field} label=${t.notifications} hint=${t.notificationsHint}>
          <${Segmented} label=${t.notifications} value=${notify ? 'on' : 'off'} onChange=${(v) => setNotify(v === 'on')}
            options=${[['on', t.on], ['off', t.off]]} />
        </${Field}>`}
        <${Field} label=${t.language}><span class="field-value">${t.languageValue}</span></${Field}>
      </section>

      <${AppearanceCard} />
      <${UsageCard} />

      <${Archived} />

      <section class="card">
        <h2 class="card-title">${t.about}</h2>
        <${Field} label=${t.version}><span class="field-value mono">${info?.version ?? '–'}</span></${Field}>
        <${Field} label=${t.platform}><span class="field-value mono">${info?.platform ?? '–'}</span></${Field}>
        <${Field} label=${t.macTools}><span class="field-value">${Object.entries(t.toolNames)
    .map(([k, v]) => `${v} ${tools[k] ? t.on : t.off}`).join(' · ')}</span></${Field}>
      </section>
    </div></div>
  </div>`;
}
