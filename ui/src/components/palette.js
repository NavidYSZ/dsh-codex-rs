// ⌘K: find a chat by title or project, or run a common action.
import { useMemo, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t, age } from '../i18n.js';
import { useStore } from '../store.js';
import { store, closePalette, newChat, openSettings, setTheme, setSidebarMode, openSession } from '../state.js';
import { IconCompose, IconGear, IconSun, IconBell, IconChat, IconFolder, IconSearch } from '../icons.js';
import { workspaceName } from './composer.js';

const matches = (hay, terms) => terms.every((w) => hay.includes(w));

function useItems(q) {
  const sessions = useStore(store, (s) => s.sessions);
  const workspaces = useStore(store, (s) => s.workspaces);
  return useMemo(() => {
    const terms = q.toLowerCase().split(/\s+/).filter(Boolean);
    const dark = document.documentElement.dataset.theme === 'dark';
    const actions = [
      { key: 'new', icon: IconCompose, label: t.newChat, kbd: '⇧⌘O', run: () => newChat() },
      { key: 'new-none', icon: IconChat, label: t.newChatNoProject, run: () => newChat('') },
      ...workspaces.map((w) => ({ key: `new:${w.path}`, icon: IconFolder, label: t.newChatIn(w.name || workspaceName(w.path)), hint: w.path, run: () => newChat(w.path), quiet: true })),
      { key: 'activity', icon: IconBell, label: t.activity, run: () => setSidebarMode('activity') },
      { key: 'settings', icon: IconGear, label: t.settings, run: openSettings },
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
  }, [q, sessions, workspaces]);
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
        <input value=${q} placeholder=${t.paletteHint} aria-label=${t.paletteHint} spellcheck="false" autocomplete="off"
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
