// The sidebar's view options (dsh workspace `viewOptions`): group by project
// or one list, order by last activity or creation, and whether archived
// chats show. Kept in ui-state.json, so every browser sees the same sidebar.
import { useRef, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t } from '../i18n.js';
import { useStore } from '../store.js';
import { store } from '../state.js';
import { Menu, MenuItem } from './common.js';
import { setUiPref } from './worklog.js';

const S = { fill: 'none', stroke: 'currentColor', 'stroke-width': 1.3, 'stroke-linecap': 'round', 'stroke-linejoin': 'round' };
const IconView = ({ size = 16 }) => html`<svg class="icon" width=${size} height=${size} viewBox="0 0 16 16" aria-hidden="true">
  <path d="M2.75 4.5h6.5M2.75 8h10.5M2.75 11.5h4" ...${S}/><circle cx="11.25" cy="4.5" r="1.5" ...${S}/><circle cx="9" cy="11.5" r="1.5" ...${S}/></svg>`;

const ARCHIVED = {
  hide: (s) => !s.archived,
  show: () => true,
  only: (s) => s.archived,
};

/** `{group, order, archived, keep(row), when(row)}` from Settings in ui-state. */
export function useSidebarView() {
  const ui = useStore(store, (s) => s.info?.ui);
  const group = ui?.sidebarGroup === 'flat' ? 'flat' : 'projects';
  const order = ui?.sidebarOrder === 'created' ? 'created' : 'activity';
  const archived = ARCHIVED[ui?.sidebarArchived] ? ui.sidebarArchived : 'hide';
  return {
    group,
    order,
    archived,
    keep: ARCHIVED[archived],
    when: order === 'created' ? (s) => s.createdAt ?? 0 : (s) => s.updatedAt ?? s.createdAt ?? 0,
  };
}

export function ViewOptions() {
  const view = useSidebarView();
  const [open, setOpen] = useState(false);
  const anchor = useRef(null);
  const changed = view.group !== 'projects' || view.order !== 'activity' || view.archived !== 'hide';
  const item = (key, value, label) => html`<${MenuItem} checked=${view[key] === value}
    onClick=${() => setUiPref({ [`sidebar${key[0].toUpperCase()}${key.slice(1)}`]: value })}>${label}</${MenuItem}>`;
  return html`<span class="chip-wrap">
    <button type="button" ref=${anchor} class=${cx('icon-btn', 'sb-view', changed && 'is-on')} aria-label=${t.viewOptions}
      title=${t.viewOptions} aria-haspopup="menu" aria-expanded=${open} onClick=${() => setOpen((v) => !v)}>
      <${IconView} size=${16} />
    </button>
    <${Menu} open=${open} onClose=${() => setOpen(false)} anchor=${anchor} fixed side="bottom" align="end" class="menu-actions sb-view-menu">
      <div class="menu-head">${t.groupBy}</div>
      ${item('group', 'projects', t.groupProjects)}
      ${item('group', 'flat', t.groupFlat)}
      <div class="menu-sep" /><div class="menu-head">${t.orderBy}</div>
      ${item('order', 'activity', t.orderActivity)}
      ${item('order', 'created', t.orderCreated)}
      <div class="menu-sep" /><div class="menu-head">${t.archivedFilter}</div>
      ${item('archived', 'hide', t.archivedHide)}
      ${item('archived', 'show', t.archivedShow)}
      ${item('archived', 'only', t.archivedOnly)}
    </${Menu}>
  </span>`;
}
