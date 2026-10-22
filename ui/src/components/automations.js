// Automations page (dsh "Automation tasks"): every reminder and recurring task
// the agent scheduled, with its chat, next run, prompt and delivery history.
// New ones come from a chat: the agent creates them with schedule_create.
import { useEffect, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t, lang, age } from '../i18n.js';
import { useStore } from '../store.js';
import { store, api, loadSchedules, openSession, newChat, setDraft, fail, ask } from '../state.js';
import { IconJob, IconChevronRight, IconPlus, IconTrash } from '../icons.js';
import { PageShell, Tabs, Pill } from './page.js';

const browserZone = (() => { try { return Intl.DateTimeFormat().resolvedOptions().timeZone; } catch { return ''; } })();

function span(sec) {
  const units = lang === 'de'
    ? [[86400, 'Tag', 'Tage'], [3600, 'Std.', 'Std.'], [60, 'Min.', 'Min.']]
    : [[86400, 'day', 'days'], [3600, 'h', 'h'], [60, 'min', 'min']];
  for (const [n, one, many] of units) {
    if (sec >= n && sec % n === 0) return `${sec / n} ${sec / n === 1 ? one : many}`;
  }
  return `${sec} s`;
}

const hhmm = (time) => String(time ?? '').slice(0, 5);
const zoneNote = (zone) => (zone && zone !== browserZone ? ` (${zone})` : '');
const when = (iso) => {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? '' : d.toLocaleString(lang, { weekday: 'short', day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit' });
};

/** "Daily at 09:00", "Every 15 min.", "Mon, Wed at 08:30 (Asia/Tokyo)" … */
export function describeSchedule(r) {
  switch (r.kind) {
    case 'after': return t.once(when(r.scheduledAt));
    case 'at': return t.once(when(r.scheduledAt));
    case 'every': return t.every(span(r.everySeconds ?? 0));
    case 'daily': return t.daily(hhmm(r.time)) + zoneNote(r.timeZone);
    case 'weekly': return t.weekly((r.weekdays ?? []).map((d) => t.weekdays[d - 1]).join(', '), hhmm(r.time)) + zoneNote(r.timeZone);
    case 'cron': return t.cronLabel(r.expression) + zoneNote(r.timeZone);
    default: return r.kind ?? '';
  }
}

function History({ id }) {
  const [rows, setRows] = useState(null);
  useEffect(() => {
    api.scheduleHistory(id).then((r) => setRows(r?.records ?? r ?? []), () => setRows([]));
  }, [id]);
  if (!rows) return html`<div class="plug-meta">${t.loading}</div>`;
  if (!rows.length) return html`<div class="plug-meta">${t.noHistory}</div>`;
  return html`<ul class="auto-history">${rows.map((h) => html`<li key=${h.messageId ?? h.deliveredAt}>
    <span>${when(h.deliveredAt ?? h.scheduledAt)}</span>${h.prompt ? html`<span class="plug-meta">${h.prompt}</span>` : null}</li>`)}</ul>`;
}

function Row({ r, chat }) {
  const [open, setOpen] = useState(false);
  const [title, setTitle] = useState(r.title ?? '');
  const [prompt, setPrompt] = useState(r.prompt ?? '');
  const active = r.status !== 'inactive';
  const changed = title.trim() !== (r.title ?? '') || prompt.trim() !== (r.prompt ?? '');
  const save = async (e) => {
    e.preventDefault();
    try {
      await api.updateSchedule(r.id, { ...(title.trim() !== r.title ? { title: title.trim() } : {}), ...(prompt.trim() !== r.prompt ? { prompt: prompt.trim() } : {}) });
      loadSchedules();
    } catch (err) { fail(err); }
  };
  const remove = async () => {
    if (!(await ask({ title: t.confirmDeleteSchedule, action: t.deleteSchedule }))) return;
    try { await api.deleteSchedule(r.id); loadSchedules(); } catch (err) { fail(err); }
  };
  const overdue = active && r.state === 'overdue';
  return html`<div class=${cx('auto-row', !active && 'is-off', open && 'is-open')}>
    <button type="button" class="auto-head" onClick=${() => setOpen((v) => !v)} aria-expanded=${open}>
      <span class="pf-chev"><${IconChevronRight} size=${12} /></span>
      <span class="auto-icon"><${IconJob} size=${15} /></span>
      <span class="plug-text">
        <span class="plug-name">${r.title || t.newAutomation}</span>
        <span class="plug-desc">${describeSchedule(r)}</span>
      </span>
      ${active ? html`<span class="auto-next">${overdue ? html`<${Pill} tone="warn">${t.overdue}</${Pill}>` : html`<span class="plug-meta">${t.nextRun}</span> ${when(r.scheduledAt)}`}</span>`
    : html`<${Pill} tone="off">${t.inactive}</${Pill}>`}
    </button>
    ${open ? html`<div class="auto-body">
      <div class="auto-chat">
        <span class="plug-meta">${t.inChat}</span>
        <button type="button" class="link-btn" onClick=${() => openSession(r.sessionId)}>${chat?.title || r.sessionId}</button>
        ${r.lastDelivery ? html`<span class="plug-meta">· ${t.lastRun} ${age(Date.parse(r.lastDelivery.deliveredAt))}</span>` : null}
      </div>
      <form class="auto-form" onSubmit=${save}>
        <label class="field-label">${t.title}<input class="input" value=${title} maxLength="120" onInput=${(e) => setTitle(e.currentTarget.value)} /></label>
        <label class="field-label">${t.prompt}<textarea class="input auto-prompt" rows="3" value=${prompt} onInput=${(e) => setPrompt(e.currentTarget.value)} /></label>
        <div class="auto-actions">
          <button type="button" class="btn btn-small" onClick=${remove}><${IconTrash} size=${13} />${t.remove}</button>
          <span class="spacer" />
          <button type="submit" class="btn btn-small btn-primary" disabled=${!changed || !title.trim() || !prompt.trim()}>${t.save}</button>
        </div>
      </form>
      <div class="field-label">${t.history}</div>
      <${History} id=${r.id} />
    </div>` : null}
  </div>`;
}

export function AutomationsPage() {
  const schedules = useStore(store, (s) => s.schedules);
  const sessions = useStore(store, (s) => s.sessions);
  const [filter, setFilter] = useState('all');
  useEffect(() => { loadSchedules(); }, []);
  const list = schedules.filter((r) => (filter === 'all' ? true : filter === 'active' ? r.status !== 'inactive' : r.status === 'inactive'));
  const create = () => { newChat(); setDraft(t.newAutomationDraft); };
  return html`<${PageShell} title=${t.automations} lead=${t.automationsLead}
    actions=${html`<button type="button" class="btn btn-small" onClick=${create}><${IconPlus} size=${14} />${t.newAutomation}</button>`}>
    <${Tabs} value=${filter} labels=${{ all: t.filterAll, active: t.filterActive, inactive: t.filterInactive }} onChange=${setFilter} />
    ${list.length ? html`<div class="auto-list">${list.map((r) => html`<${Row} key=${r.id} r=${r} chat=${sessions.find((x) => x.id === r.sessionId)} />`)}</div>`
    : html`<div class="page-empty">${t.noAutomations}</div>`}
  </${PageShell}>`;
}
