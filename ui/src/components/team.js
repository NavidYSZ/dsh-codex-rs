// Agent Team of a Lead chat, as a panel tab: roster (click opens a member's
// own chat) and the shared task board.
import { html, cx } from '../html.js';
import { t } from '../i18n.js';
import { useStore } from '../store.js';
import { store, openSession } from '../state.js';

const ORDER = ['in_progress', 'pending', 'completed'];

function TeamPanel() {
  const team = useStore(store, (s) => s.team);
  const leadId = useStore(store, (s) => s.activeId);
  if (!team) return null;
  const members = team.members ?? [];
  const idOf = (name) => members.find((m) => m.name === name)?.id;
  const nameOf = (id) => members.find((m) => m.id === id)?.name ?? id;
  const roster = team.roster?.length ? team.roster : members.map((m) => ({ target: m.name, role: m.role, status: m.phase }));
  const tasks = [...(team.tasks ?? [])].sort((a, b) => ORDER.indexOf(a.status) - ORDER.indexOf(b.status));
  return html`<div class="panel-scroll team">
    ${team.failure ? html`<div class="plug-warn">${team.failure.message ?? String(team.failure)}</div>` : null}
    <div class="field-label">${t.teamMembers}</div>
    <div class="team-roster">${roster.map((r) => {
      const id = r.role === 'lead' ? leadId : idOf(r.target);
      return html`<button type="button" key=${r.target} class="team-member" disabled=${!id || id === leadId}
        onClick=${() => id && openSession(id)}>
        <span class=${cx('live-dot', r.status === 'running' && 'is-active', r.status === 'failed' && 'is-error')} />
        <span class="plug-text">
          <span class="plug-name">${r.target}${r.model ? html`<span class="plug-meta">${r.model}</span>` : null}</span>
          ${r.description ? html`<span class="plug-desc">${r.description}</span>` : null}
        </span>
        <span class="plug-meta">${t.memberStatus[r.status] ?? r.status}</span>
      </button>`;
    })}</div>
    <div class="field-label">${t.teamTasks}</div>
    ${tasks.length ? html`<div class="team-tasks">${tasks.map((k) => html`<div key=${k.id} class=${cx('team-task', `is-${k.status}`)}>
      <div class="team-task-head"><span class="plug-name">${k.subject}</span><span class="plug-meta">${t.taskStatus[k.status] ?? k.status}</span></div>
      ${k.description ? html`<div class="plug-desc">${k.description}</div>` : null}
      <div class="plug-meta">${k.ownerId ? `${t.owner}: ${nameOf(k.ownerId)}` : ''}${k.blockedBy?.length ? ` · ${t.blockedBy}: ${k.blockedBy.join(', ')}` : ''}</div>
    </div>`)}</div>` : html`<div class="page-empty">${t.noTasks}</div>`}
  </div>`;
}

/** Panel tab while the chat leads a Team. */
export function useTeamTab() {
  const team = useStore(store, (s) => s.team);
  if (!team) return null;
  const busy = (team.roster ?? []).filter((r) => r.status === 'running').length;
  return {
    label: t.team,
    badge: busy ? html`<span class="ptab-count">${busy}</span>` : null,
    body: html`<${TeamPanel} />`,
  };
}
