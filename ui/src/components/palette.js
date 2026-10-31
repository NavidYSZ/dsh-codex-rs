// ⌘K: find a chat by title or project, or run a common action.
import { useMemo, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t, age } from '../i18n.js';
import { useStore } from '../store.js';
import {
  store, closePalette, newChat, openSettings, setTheme, setSidebarMode, openSession, openPlugins, openAutomations, setComposer,
} from '../state.js';
import { IconCompose, IconGear, IconSun, IconBell, IconChat, IconFolder, IconSearch, IconPlug, IconChart, IconJob, IconList } from '../icons.js';
import { workspaceName } from './composer.js';
import { setUiPref } from './worklog.js';
import { VIEW_MODES } from '../worklog.js';

const SETTINGS_PANES = ['general', 'appearance', 'models', 'defaults', 'context', 'browser', 'usage', 'archived', 'about'];

const matches = (hay, terms) => terms.every((w) => hay.includes(w));

function useItems(q) {
  const sessions = useStore(store, (s) => s.sessions);
  const workspaces = useStore(store, (s) => s.workspaces);
  const info = useStore(store, (s) => s.info);
  return useMemo(() => {
    const terms = q.toLowerCase().split(/\s+/).filter(Boolean);
    const dark = document.documentElement.dataset.theme === 'dark';
    const actions = [
      { key: 'new', icon: IconCompose, label: t.newChat, kbd: '⇧⌘O', run: () => newChat() },
      { key: 'new-none', icon: IconChat, label: t.newChatNoProject, run: () => newChat('') },
      ...workspaces.map((w) => ({ key: `new:${w.path}`, icon: IconFolder, label: t.newChatIn(w.name || workspaceName(w.path)), hint: w.path, run: () => newChat(w.path), quiet: true })),
      // A new chat in another mode (searched for: "Minimal").
      ...(info?.presets ?? []).map((p) => ({
        key: `mode:${p.id}`, icon: IconCompose, label: t.modeNewChat(t.modes[p.id]?.name ?? p.name), quiet: true,
        run: () => { newChat(); setComposer({ preset: p.id }); },
      })),
      { key: 'activity', icon: IconBell, label: t.activity, run: () => setSidebarMode('activity') },
      ...(info?.extensions ? [
        { key: 'plugins', icon: IconPlug, label: t.plugins, run: () => openPlugins('plugins') },
        { key: 'skills', icon: IconPlug, label: t.tabsPlugins.skills, run: () => openPlugins('skills') },
        { key: 'mcp', icon: IconPlug, label: t.tabsPlugins.mcp, run: () => openPlugins('mcp') },
        { key: 'apps', icon: IconPlug, label: t.tabsPlugins.connectors, run: () => openPlugins('connectors'), quiet: true },
      ] : []),
      ...(info?.automation?.schedules ? [{ key: 'automations', icon: IconJob, label: t.automations, run: openAutomations }] : []),
      { key: 'usage', icon: IconChart, label: t.usage, run: () => openSettings('usage') },
      { key: 'settings', icon: IconGear, label: t.settings, run: () => openSettings() },
      ...SETTINGS_PANES.map((k) => ({
        key: `settings:${k}`, icon: IconGear, label: `${t.settings} · ${t.settingsNav[k]}`, quiet: true, run: () => openSettings(k),
      })),
      // The work-details view of every chat (searched for: "Ansicht", "Kompakt").
      ...VIEW_MODES.map((m) => ({
        key: `view:${m}`, icon: IconList, label: `${t.workDetails}: ${t.workModes[m]}`, quiet: true, run: () => setUiPref({ workDetails: m }),
      })),
      { key: 'theme', icon: IconSun, label: t.toggleTheme, run: () => setTheme(dark ? 'light' : 'dark') },
    ].filter((a) => (terms.length ? matches(a.label.toLowerCase(), terms) : !a.quiet));
    const chats = [...sessions]
      .sort((a, b) => (b.updatedAt ?? 0) - (a.updatedAt ?? 0))
      .filter((s) => {
        const project = s.scratch ? '' : workspaceName(s.cwd);
        return matches(`${s.title ?? ''} ${project}`.toLowerCase(), terms);
      })
      .slice(0, terms.length ? 40 : 8)
      .map((s) => ({
        key: s.id, icon: IconChat, label: s.title || t.newChat, hint: s.scratch ? '' : workspaceName(s.cwd),
        meta: age(s.updatedAt ?? s.createdAt ?? Date.now()), run: () => openSession(s.id), archived: s.archived,
      }));
    return { actions, chats };
  }, [q, sessions, workspaces, info]);
}

function Body() {
  const [q, setQ] = useState('');
  const [sel, setSel] = useState(0);
  const { actions, chats } = useItems(q);
  // Searching puts chats first; an empty query offers the actions first.
  const groups = q.trim() ? [[t.chats, chats], [t.actions, actions]] : [[t.actions, actions], [t.chats, chats]];
  const flat = groups.flatMap(([, list]) => list);
  const pick = (item) => { closePalette(); item?.run(); };
  const onKey = (e) => {
    if (e.key === 'ArrowDown') { e.preventDefault(); setSel((n) => Math.min(flat.length - 1, n + 1)); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); setSel((n) => Math.max(0, n - 1)); }
    else if (e.key === 'Enter') { e.preventDefault(); pick(flat[sel]); }
    else if (e.key === 'Escape') { e.preventDefault(); closePalette(); }
  };
  let i = -1;
  return html`<div class="palette-scrim" onMouseDown=${(e) => { if (e.target === e.currentTarget) closePalette(); }}>
    <div class="palette" role="dialog" aria-label=${t.search}>
      <div class="palette-input">
        <${IconSearch} size=${16} />
        <input value=${q} placeholder=${t.paletteHint} aria-label=${t.paletteHint} spellcheck=${false} autocomplete="off"
          ref=${(el) => { if (el && !el.dataset.focused) { el.dataset.focused = '1'; el.focus(); } }}
          onInput=${(e) => { setQ(e.currentTarget.value); setSel(0); }} onKeyDown=${onKey} />
        <kbd class="kbd">esc</kbd>
      </div>
      <div class="palette-list" role="listbox">
        ${flat.length ? groups.map(([title, list]) => (list.length ? html`<div class="palette-group" key=${title}>
          <div class="menu-head">${title}</div>
          ${list.map((item) => {
            i += 1;
            const n = i;
            const Icon = item.icon;
            return html`<button type="button" key=${item.key} role="option" aria-selected=${n === sel}
              class=${cx('palette-item', n === sel && 'is-sel', item.archived && 'is-archived')}
              onMouseMove=${() => setSel(n)} onClick=${() => pick(item)}>
              <span class="menu-icon"><${Icon} size=${14} /></span>
              <span class="palette-label">${item.label}${item.hint ? html`<span class="palette-hint">${item.hint}</span>` : null}</span>
              ${item.kbd ? html`<kbd class="kbd">${item.kbd}</kbd>` : item.meta ? html`<span class="palette-meta">${item.meta}</span>` : null}
            </button>`;
          })}
        </div>` : null)) : html`<div class="palette-empty">${t.noResults}</div>`}
      </div>
    </div>
  </div>`;
}

export function Palette() {
  const open = useStore(store, (s) => s.palette);
  return open ? html`<${Body} />` : null;
}
