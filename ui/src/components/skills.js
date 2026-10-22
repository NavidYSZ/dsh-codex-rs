// Skills tab of the Plugins page: search, source filters, a readable list
// with switches, the detail drawer (rendered SKILL.md, files, path) and
// "New skill" (with the agent, or a blank template). Skills reach a chat
// only through /name unless "Let the model load it" is on (ux-plan §1, §8).
import { useMemo, useRef, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t, quote } from '../i18n.js';
import { useStore } from '../store.js';
import { store, api, fail, toast, newChat, setDraft, closeSettings } from '../state.js';
import { renderMarkdown, onMarkdownClick, copyText } from '../markdown.js';
import { IconSearch, IconPlus, IconFolder, IconPencil, IconCopy, IconFile, IconChat, IconAlert, IconCompose, IconRefresh } from '../icons.js';
import { Menu, ActionItem, MenuItem } from './common.js';
import { useLoad, Toggle, Pill } from './page.js';
import { Logo, tint, useRoute, routeMissing, isMissingRoute } from './plugin-kit.js';
import { Drawer, Dialog, Field, Segmented } from './drawer.js';
import { workspaceName } from './composer.js';

// ---- sources and settings -----------------------------------------------------

const KIND = {
  'project-dsh': 'project', 'project-agents': 'project',
  'user-dsh-codex': 'personal', 'user-dsh': 'personal', 'user-agents': 'personal',
  plugin: 'plugin', custom: 'plugin', bundled: 'system',
};
const KINDS = ['project', 'personal', 'plugin', 'system'];
const kindOf = (s) => (s.plugin ? 'plugin' : KIND[s.source] ?? 'system');
const keyOf = (s) => `${s.source}:${s.path}`;

/** The Mac app shows files of this instance (local instances only). */
const localDesktop = () => !!window.dshxDesktop?.capabilities?.revealInFinder;

/** The source as a pill label ("Project", "Personal", the plugin's name). */
function sourceLabel(s) {
  if (s.plugin) return s.plugin.split('@')[0];
  return t.skKinds[kindOf(s)];
}

const SETTINGS_ROUTE = '/api/skills/_/settings';

/**
 * The per-skill switches (`[skills.entries."<name>"]`) need the settings
 * route; rows with `enabled` or the route itself tell that it exists.
 */
function useSettings(rows, reload) {
  const [unsupported, setUnsupported] = useState(false);
  const [busy, setBusy] = useState(null);
  const route = useRoute(SETTINGS_ROUTE);
  const supported = !unsupported && (route === true || rows.some((r) => 'enabled' in r));
  const change = async (s, patch) => {
    setBusy(s.name);
    try {
      await api.skillSettings(s.name, patch);
    } catch (e) {
      if (isMissingRoute(e)) {
        routeMissing(SETTINGS_ROUTE);
        setUnsupported(true);
        toast(t.skSettingsUnsupported);
      } else fail(e);
    } finally {
      setBusy(null);
      reload();
    }
  };
  return { supported, busy, change };
}

// ---- rows ------------------------------------------------------------------------

function SkillIcon({ s, size = 32 }) {
  const title = s.title || s.name;
  return html`<${Logo} url=${s.logoUrl} name=${title} color=${tint(s.name)} size=${size} letters=${2} />`;
}

function Note({ s }) {
  if (s.invalid) return html`<div class="sk-warn"><${IconAlert} size=${13} /><span>${t.skInvalid(s.error)}</span></div>`;
  if (s.active === false && kindOf(s) === 'plugin') return html`<div class="sk-note">${t.skPluginOff}</div>`;
  if (s.active === false) return html`<div class="sk-note">${t.skShadowed}</div>`;
  return null;
}

function RowMenu({ s, settings, onUse, onOpen }) {
  const [open, setOpen] = useState(false);
  const anchor = useRef(null);
  const local = localDesktop();
  const model = s.model === true;
  return html`<span class="chip-wrap">
    <button type="button" ref=${anchor} class="icon-btn" aria-label=${t.more} title=${t.more} aria-haspopup="menu" aria-expanded=${open}
      onClick=${() => setOpen((v) => !v)}><svg width="15" height="15" viewBox="0 0 16 16" aria-hidden="true"><circle cx="3.5" cy="8" r="1.1" fill="currentColor"/><circle cx="8" cy="8" r="1.1" fill="currentColor"/><circle cx="12.5" cy="8" r="1.1" fill="currentColor"/></svg></button>
    <${Menu} open=${open} onClose=${() => setOpen(false)} anchor=${anchor} fixed side="bottom" align="end" class="menu-actions sk-menu">
      <${MenuItem} checked=${model} disabled=${!settings.supported || s.invalid || settings.busy === s.name}
        hint=${settings.supported ? t.skModelHint : t.skSettingsUnsupported}
        onClick=${() => { setOpen(false); settings.change(s, { model: !model }); }}>${t.skModel}</${MenuItem}>
      <div class="menu-sep" />
      <${ActionItem} icon=${html`<${IconChat} size=${14} />`} disabled=${s.invalid || s.userInvocable === false} onClick=${() => { setOpen(false); onUse(s); }}>${t.skUse}</${ActionItem}>
      <${ActionItem} icon=${html`<${IconFile} size=${14} />`} onClick=${() => { setOpen(false); onOpen(s); }}>${t.skDetails}</${ActionItem}>
      ${local ? html`<${ActionItem} icon=${html`<${IconFolder} size=${14} />`} onClick=${() => { setOpen(false); window.dshxDesktop.revealInFinder(s.path); }}>${t.revealInFinder}</${ActionItem}>
        <${ActionItem} icon=${html`<${IconPencil} size=${14} />`} onClick=${() => { setOpen(false); window.dshxDesktop.openInEditor(s.path); }}>${t.openInEditor}</${ActionItem}>` : null}
      <${ActionItem} icon=${html`<${IconCopy} size=${14} />`} onClick=${() => { setOpen(false); copyText(s.path).then(() => toast(t.copied)); }}>${t.copyPath}</${ActionItem}>
    </${Menu}>
  </span>`;
}

function SkillRow({ s, settings, onOpen, onUse }) {
  const on = s.enabled !== false;
  const off = !on || s.active === false || s.invalid;
  return html`<div class=${cx('sk-row', off && 'is-off')}>
    <button type="button" class="sk-main" onClick=${() => onOpen(s)} aria-label=${`${s.title || s.name}: ${t.skDetails}`}>
      <${SkillIcon} s=${s} />
      <span class="sk-text">
        <span class="sk-titleline"><span class="sk-title">${s.title || s.name}</span><span class="sk-slug">/${s.name}</span></span>
        ${s.description ? html`<span class="sk-desc">${s.description}</span>` : null}
        <${Note} s=${s} />
      </span>
    </button>
    <div class="sk-side">
      <${Pill} tone="off">${sourceLabel(s)}</${Pill}>
      <span class="sk-switch" title=${settings.supported ? t.skActiveHint : t.skSettingsUnsupported}>
        <${Toggle} on=${on && !s.invalid} label=${t.skActiveFor(s.title || s.name)} disabled=${!settings.supported || s.invalid || settings.busy === s.name}
          onChange=${(v) => settings.change(s, { enabled: v })} />
      </span>
      <${RowMenu} s=${s} settings=${settings} onUse=${onUse} onOpen=${onOpen} />
    </div>
  </div>`;
}

function Skeleton() {
  return html`<div class="sk-list" aria-busy="true" aria-label=${t.loading}>
    ${[0, 1, 2, 3].map((i) => html`<div key=${i} class="sk-row sk-skeleton"><span class="sk-main">
      <span class="plug-logo sk-ghost" /><span class="sk-text"><span class="sk-bar" style="width:38%" /><span class="sk-bar" style="width:82%" /></span>
    </span></div>`)}
  </div>`;
}

/** Back to the chat (or the start page) with `/name ` in the composer. */
function insertInChat(s) {
  closeSettings();
  setDraft(`/${s.name} `);
}

// ---- detail drawer ------------------------------------------------------------------

const size = (n) => (n == null ? '' : n < 1024 ? `${n} B` : n < 1048576 ? `${(n / 1024).toFixed(n < 10240 ? 1 : 0)} KB` : `${(n / 1048576).toFixed(1)} MB`);

function Files({ files, dir, truncated }) {
  const local = localDesktop();
  return html`<ul class="sk-files">
    ${files.map((f) => {
    const depth = f.path.split('/').length - 1;
    const name = f.path.split('/').pop();
    return html`<li key=${f.path} class="sk-file" style=${`--depth:${depth}`}>
        <span class="sk-file-icon">${f.dir ? html`<${IconFolder} size=${14} />` : html`<${IconFile} size=${14} />`}</span>
        ${local && !f.dir ? html`<button type="button" class="sk-file-name link-btn" title=${t.openInEditor}
          onClick=${() => window.dshxDesktop.openInEditor(`${dir}/${f.path}`)}>${name}</button>`
    : html`<span class="sk-file-name">${name}</span>`}
        <span class="sk-file-size">${f.dir ? '' : size(f.size)}</span>
      </li>`;
  })}
    ${truncated ? html`<li class="sk-note">${t.skFilesTruncated}</li>` : null}
  </ul>`;
}

function SkillDoc({ body, dir }) {
  const htmlText = useMemo(() => renderMarkdown(body), [body]);
  // Relative links point into the skill's folder, not the chat's.
  const onClick = (e) => {
    const ref = e.target.closest?.('[data-path]');
    if (ref) {
      e.preventDefault();
      if (localDesktop()) window.dshxDesktop.openInEditor(`${dir}/${ref.dataset.path.replace(/^\.\//, '')}`);
      return;
    }
    onMarkdownClick(e);
  };
  return html`<div class="md sk-doc" onClick=${onClick} dangerouslySetInnerHTML=${{ __html: htmlText }} />`;
}

function SkillDrawer({ s, cwd, settings, onClose }) {
  const detail = useLoad(() => api.skill(s.name, cwd, s.path), [s.path]);
  const d = detail.data;
  const local = localDesktop();
  const on = s.enabled !== false;
  const use = () => { onClose(); insertInChat(s); };
  const head = html`<${SkillIcon} s=${s} size=${40} />
    <div class="dr-titles">
      <h2 class="dr-title">${s.title || s.name}</h2>
      <div class="dr-meta"><span class="sk-slug">/${s.name}</span><${Pill} tone="off">${sourceLabel(s)}</${Pill}></div>
    </div>`;
  const footer = html`
    ${local ? html`<button type="button" class="btn btn-small" onClick=${() => window.dshxDesktop.revealInFinder(s.path)}><${IconFolder} size=${14} />${t.revealInFinder}</button>
      <button type="button" class="btn btn-small" onClick=${() => window.dshxDesktop.openInEditor(s.path)}><${IconPencil} size=${14} />${t.openInEditor}</button>` : null}
    <span class="spacer" />
    <button type="button" class="btn btn-small btn-primary" disabled=${s.invalid || s.userInvocable === false || !on} onClick=${use}>
      <${IconChat} size=${14} />${t.skUse}</button>`;
  return html`<${Drawer} label=${s.title || s.name} head=${head} onClose=${onClose} footer=${footer}>
    ${s.description ? html`<p class="dr-lead">${s.description}</p>` : null}
    <${Note} s=${s} />
    <section class="dr-section">
      <div class="dr-setting">
        <div class="dr-setting-text"><span class="dr-setting-title">${t.skActive}</span><span class="dr-setting-hint">${t.skActiveHint}</span></div>
        <span title=${settings.supported ? '' : t.skSettingsUnsupported}><${Toggle} on=${on && !s.invalid} label=${t.skActive}
          disabled=${!settings.supported || s.invalid || settings.busy === s.name} onChange=${(v) => settings.change(s, { enabled: v })} /></span>
      </div>
      <div class="dr-setting">
        <div class="dr-setting-text"><span class="dr-setting-title">${t.skModel}</span><span class="dr-setting-hint">${t.skModelHint}</span></div>
        <span title=${settings.supported ? '' : t.skSettingsUnsupported}><${Toggle} on=${s.model === true} label=${t.skModel}
          disabled=${!settings.supported || s.invalid || settings.busy === s.name} onChange=${(v) => settings.change(s, { model: v })} /></span>
      </div>
    </section>
    ${detail.loading ? html`<div class="dr-loading"><span class="spinner" />${t.loading}</div>` : null}
    ${detail.error ? html`<div class="dr-error">${t.skDetailFailed}<button type="button" class="link-btn" onClick=${detail.reload}>${t.retry}</button></div>` : null}
    ${d ? html`
      <section class="dr-section">
        <h3 class="dr-h">${t.skInstructions}</h3>
        ${d.body?.trim() ? html`<${SkillDoc} body=${d.body} dir=${d.dir} />` : html`<p class="sk-note">${t.skNoBody}</p>`}
        ${d.contentTruncated ? html`<p class="sk-note">${t.skBodyTruncated}</p>` : null}
      </section>
      <section class="dr-section">
        <h3 class="dr-h">${t.skFiles}<span class="dr-count">${d.files?.filter((f) => !f.dir).length ?? 0}</span></h3>
        <${Files} files=${d.files ?? []} dir=${d.dir} truncated=${d.filesTruncated} />
      </section>` : null}
    <section class="dr-section">
      <h3 class="dr-h">${t.skLocation}</h3>
      <div class="dr-path">
        <code class="mono" title=${s.path}>${s.path}</code>
        <button type="button" class="icon-btn" aria-label=${t.copyPath} title=${t.copyPath}
          onClick=${() => copyText(s.path).then(() => toast(t.copied))}><${IconCopy} size=${14} /></button>
      </div>
    </section>
  </${Drawer}>`;
}

// ---- new skill ----------------------------------------------------------------------

const slug = (text) => text.toLowerCase().normalize('NFKD').replace(/[̀-ͯ]/g, '')
  .replace(/ß/g, 'ss').replace(/[^a-z0-9]+/g, '-').replace(/^-+/, '').replace(/-{2,}/g, '-').slice(0, 64);
const SKILL_NAME = /^[a-z0-9]+(-[a-z0-9]+)*$/;

function BlankSkillDialog({ cwd, scope: initial, onClose, onCreated }) {
  const [name, setName] = useState('');
  const [description, setDescription] = useState('');
  const [scope, setScope] = useState(cwd ? initial ?? 'personal' : 'personal');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(null);
  const clean = name.replace(/-+$/, '');
  const invalid = clean && !SKILL_NAME.test(clean);
  const where = scope === 'project' ? `${workspaceName(cwd)}/.agents/skills/${clean || '…'}/SKILL.md` : `~/.agents/skills/${clean || '…'}/SKILL.md`;
  const submit = async (e) => {
    e?.preventDefault();
    if (!clean || invalid || busy) return;
    setBusy(true);
    setError(null);
    try {
      const made = await api.createSkill({ name: clean, scope, cwd: cwd || undefined, description: description.trim() });
      toast(t.skCreated(clean));
      onCreated(made);
    } catch (err) {
      if (err?.status === 0 || err?.status === 401) fail(err);
      else setError(err?.message ?? String(err));
    } finally {
      setBusy(false);
    }
  };
  const footer = html`<button type="button" class="btn btn-small" onClick=${onClose}>${t.cancel}</button>
    <button type="submit" form="sk-blank" class="btn btn-small btn-primary" disabled=${!clean || invalid || busy}>
      ${busy ? html`<span class="spinner" />` : null}${t.skCreate}</button>`;
  return html`<${Dialog} title=${t.skBlankTitle} description=${t.skBlankLead} onClose=${onClose} footer=${footer}>
    <form id="sk-blank" class="md-form" onSubmit=${submit}>
      <${Field} label=${t.skName} wide error=${invalid ? t.skNameInvalid : error}
        hint=${clean ? html`${t.skNameHint} <span class="mono">/${clean}</span>` : t.skNameHint}>
        <input class="input mono" value=${name} spellcheck="false" autocomplete="off" placeholder="release-notes" maxlength="64"
          onInput=${(e) => { setName(slug(e.currentTarget.value)); setError(null); }} />
      </${Field}>
      <${Field} label=${t.skDescription} wide hint=${t.skDescriptionHint}>
        <textarea class="input sk-textarea" rows="3" value=${description} placeholder=${t.skDescriptionPlaceholder}
          onInput=${(e) => setDescription(e.currentTarget.value)}
          onKeyDown=${(e) => { if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) submit(e); }} />
      </${Field}>
      <${Field} label=${t.skWhere} wide as="div" hint=${html`<span class="mono">${where}</span>`}>
        <${Segmented} label=${t.skWhere} value=${scope} onChange=${setScope}
          options=${[['personal', t.skKinds.personal], ...(cwd ? [['project', `${t.skKinds.project} · ${workspaceName(cwd)}`]] : [])]} />
      </${Field}>
    </form>
  </${Dialog}>`;
}

function NewSkillButton({ rows, onBlank }) {
  const [open, setOpen] = useState(false);
  const anchor = useRef(null);
  const creator = rows.find((r) => r.name === 'skill-creator' && !r.invalid && r.userInvocable !== false && r.enabled !== false);
  const withAgent = () => {
    setOpen(false);
    newChat();
    setDraft(creator ? `/skill-creator ${t.skAgentPrompt}` : t.skAgentPromptPlain);
  };
  return html`<span class="chip-wrap">
    <button type="button" ref=${anchor} class="btn btn-small btn-primary" aria-haspopup="menu" aria-expanded=${open}
      onClick=${() => setOpen((v) => !v)}><${IconPlus} size=${14} />${t.skNew}</button>
    <${Menu} open=${open} onClose=${() => setOpen(false)} anchor=${anchor} fixed side="bottom" align="end" class="menu-actions sk-new-menu">
      <${ActionItem} icon=${html`<${IconCompose} size=${14} />`} onClick=${withAgent}>
        <span>${t.skWithAgent}</span><span class="menu-hint">${creator ? t.skWithAgentHint : t.skWithAgentPlain}</span></${ActionItem}>
      <${ActionItem} icon=${html`<${IconFile} size=${14} />`} onClick=${() => { setOpen(false); onBlank(); }}>
        <span>${t.skBlank}</span><span class="menu-hint">${t.skBlankHint}</span></${ActionItem}>
    </${Menu}>
  </span>`;
}

// ---- the tab -----------------------------------------------------------------------

export function SkillsTab() {
  const cwd = useStore(store, (s) => s.cwd);
  const skills = useLoad(() => api.skills(cwd), [cwd]);
  const rows = skills.data ?? [];
  const settings = useSettings(rows, skills.reload);
  const [query, setQuery] = useState('');
  const [filter, setFilter] = useState('all');
  const [opened, setOpened] = useState(null);
  const [blank, setBlank] = useState(null);

  const counts = useMemo(() => {
    const c = { all: rows.length };
    for (const r of rows) c[kindOf(r)] = (c[kindOf(r)] ?? 0) + 1;
    return c;
  }, [rows]);
  const shown = useMemo(() => {
    const q = query.trim().toLowerCase();
    const order = (r) => KINDS.indexOf(kindOf(r));
    // Usable skills first, then hidden ones, then broken ones.
    const rank = (r) => (r.invalid ? 2 : r.active === false ? 1 : 0);
    return rows
      .filter((r) => filter === 'all' || kindOf(r) === filter)
      .filter((r) => !q || [r.title, r.name, r.description, r.plugin].some((v) => v?.toLowerCase().includes(q)))
      .sort((a, b) => order(a) - order(b) || rank(a) - rank(b) || (a.title || a.name).localeCompare(b.title || b.name));
  }, [rows, filter, query]);
  // The open row follows reloads (switches change it).
  const current = opened && (rows.find((r) => keyOf(r) === opened) ?? null);
  const filters = ['all', 'project', 'personal', 'plugin', ...(counts.system ? ['system'] : [])];

  let body;
  if (skills.loading && !skills.data) body = html`<${Skeleton} />`;
  else if (skills.error && !skills.data) {
    body = html`<div class="page-empty xe-empty"><p>${t.skLoadFailed}</p>
      <button type="button" class="btn btn-small" onClick=${skills.reload}><${IconRefresh} size=${14} />${t.retry}</button></div>`;
  } else if (!rows.length) {
    body = html`<div class="page-empty xe-empty"><p class="xe-empty-title">${t.skEmpty}</p><p>${t.skEmptyHint}</p>
      <button type="button" class="btn btn-small" onClick=${() => setBlank('personal')}><${IconPlus} size=${14} />${t.skBlank}</button></div>`;
  } else if (!shown.length) {
    body = query.trim()
      ? html`<div class="page-empty xe-empty"><p>${t.skNoMatch(quote(query.trim()))}</p>
          <button type="button" class="link-btn" onClick=${() => setQuery('')}>${t.skClearSearch}</button></div>`
      : html`<div class="page-empty xe-empty"><p>${t.skNoneOf[filter]}</p>
          ${filter === 'project' && cwd ? html`<button type="button" class="link-btn" onClick=${() => setBlank('project')}>${t.skBlankHere}</button>` : null}</div>`;
  } else {
    const sections = filter === 'all' && !query.trim();
    let last = null;
    body = html`<div class="sk-list">${shown.map((s) => {
      const kind = kindOf(s);
      const label = sections && kind !== last ? html`<div class="sk-section" key=${`h-${kind}`}>${t.skKinds[kind]}</div>` : null;
      last = kind;
      return html`${label}<${SkillRow} key=${keyOf(s)} s=${s} settings=${settings}
        onOpen=${(x) => setOpened(keyOf(x))} onUse=${insertInChat} />`;
    })}</div>`;
  }

  return html`<div class="sk-page">
    <div class="xe-toolbar">
      <label class="xe-search">
        <${IconSearch} size=${14} />
        <input type="search" value=${query} placeholder=${t.skSearch} aria-label=${t.skSearch} spellcheck="false"
          onInput=${(e) => setQuery(e.currentTarget.value)} onKeyDown=${(e) => { if (e.key === 'Escape' && query) { e.stopPropagation(); setQuery(''); } }} />
      </label>
      <${NewSkillButton} rows=${rows} onBlank=${() => setBlank('personal')} />
    </div>
    <div class="xe-filters" role="group" aria-label=${t.skFilterLabel}>
      ${filters.map((f) => html`<button type="button" key=${f} class=${cx('xe-filter', filter === f && 'is-on')} aria-pressed=${filter === f}
        onClick=${() => setFilter(f)}>${f === 'all' ? t.skAll : t.skKinds[f]}${skills.data ? html`<span class="xe-count">${counts[f] ?? 0}</span>` : null}</button>`)}
    </div>
    ${cwd ? html`<div class="plug-meta page-note">${t.skInProject(workspaceName(cwd))}</div>` : null}
    ${body}
    ${current ? html`<${SkillDrawer} key=${keyOf(current)} s=${current} cwd=${cwd} settings=${settings} onClose=${() => setOpened(null)} />` : null}
    ${blank ? html`<${BlankSkillDialog} cwd=${cwd} scope=${blank} onClose=${() => setBlank(null)}
      onCreated=${(made) => { setBlank(null); skills.reload(); setOpened(keyOf(made)); }} />` : null}
  </div>`;
}
