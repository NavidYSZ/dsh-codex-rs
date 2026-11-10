// Orchestration bar above the composer: the chat's run with phase, rounds
// and worker budget, criteria checklist, task board, and the form that
// starts a new run (objective plus one acceptance criterion per line).
import { useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t } from '../i18n.js';
import { useStore } from '../store.js';
import { store, orchestrationCommand, ask } from '../state.js';
import { IconX } from '../icons.js';

export const IconFlow = ({ size = 14 }) => html`<svg class="icon" width=${size} height=${size} viewBox="0 0 16 16" aria-hidden="true">
  <circle cx="3.5" cy="8" r="2" fill="none" stroke="currentColor" stroke-width="1.3"/>
  <circle cx="12.5" cy="3.5" r="2" fill="none" stroke="currentColor" stroke-width="1.3"/>
  <circle cx="12.5" cy="12.5" r="2" fill="none" stroke="currentColor" stroke-width="1.3"/>
  <path d="M5.4 7.1 10.6 4.4M5.4 8.9l5.2 2.7" fill="none" stroke="currentColor" stroke-width="1.3"/></svg>`;

export const openOrchForm = () => store.set({ orchForm: true });
const closeOrchForm = () => store.set({ orchForm: false });

/** One criterion per line; ids are slugified (collision-safe). */
function criteriaInput(text) {
  const used = new Set();
  return text.split('\n').map((l) => l.trim()).filter(Boolean).slice(0, 16).map((criterion, i) => {
    let id = criterion.toLowerCase().normalize('NFKD')
      .replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 48) || `c${i + 1}`;
    while (used.has(id)) id = `${id}-${i + 1}`;
    used.add(id);
    return { id, criterion };
  });
}

function OrchForm() {
  const [objective, setObjective] = useState('');
  const [criteria, setCriteria] = useState('');
  const [rounds, setRounds] = useState('');
  const submit = async (e) => {
    e.preventDefault();
    const obj = objective.trim();
    const list = criteriaInput(criteria);
    if (!obj || !list.length) return;
    const max = Number(rounds);
    const body = {
      action: 'create', objective: obj, criteria: list,
      ...(Number.isInteger(max) && max > 0 ? { limits: { maxRounds: max } } : {}),
    };
    if (await orchestrationCommand(body)) closeOrchForm();
  };
  return html`<form class="goalbar goal-form orch-form" onSubmit=${submit}>
    <div class="goal-top">
      <span class="goal-icon"><${IconFlow} /></span>
      <span class="goal-label">${t.setOrch}</span>
      <span class="spacer" />
      <button type="button" class="icon-btn" aria-label=${t.cancel} onClick=${closeOrchForm}><${IconX} size=${14} /></button>
    </div>
    <textarea class="goal-input" rows="2" value=${objective} placeholder=${t.orchPlaceholder} aria-label=${t.orch}
      ref=${(el) => { if (el && !el.dataset.focused) { el.dataset.focused = '1'; el.focus(); } }}
      onInput=${(e) => setObjective(e.currentTarget.value)} />
    <textarea class="goal-input" rows="3" value=${criteria} placeholder=${t.orchCriteriaPlaceholder} aria-label=${t.orchCriteria}
      onInput=${(e) => setCriteria(e.currentTarget.value)}
      onKeyDown=${(e) => { if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) submit(e); if (e.key === 'Escape') closeOrchForm(); }} />
    <div class="goal-actions">
      <label class="goal-rounds">${t.goalMaxRounds}
        <input class="input" type="number" min="1" max="10000" step="1" value=${rounds} placeholder="8" onInput=${(e) => setRounds(e.currentTarget.value)} /></label>
      <span class="spacer" />
      <button type="submit" class="btn btn-small btn-primary" disabled=${!objective.trim() || !criteriaInput(criteria).length}>${t.orchStart}</button>
    </div>
  </form>`;
}

const TASK_DOT = { pending: '○', running: '◐', completed: '●', failed: '✕' };

function TaskChip({ task }) {
  return html`<span class=${cx('orch-task', `is-${task.status}`)}
    title=${(task.summary ?? task.stopReason ?? task.description)}>${TASK_DOT[task.status] ?? '○'} ${task.id}</span>`;
}

function Criteria({ run, checks }) {
  const met = new Map((checks ?? []).map((c) => [c.criterionId, c]));
  return html`<ul class="orch-criteria">
    ${run.criteria.map((c) => html`<li class=${met.has(c.id) ? 'is-met' : ''}>${met.has(c.id) ? '☑' : '☐'} ${c.criterion}</li>`)}
  </ul>`;
}

export function OrchBar() {
  const orch = useStore(store, (s) => s.orchestration);
  const form = useStore(store, (s) => s.orchForm);
  const [open, setOpen] = useState(true);
  if (form) return html`<${OrchForm} />`;
  if (!orch || orch.phase === 'complete' || orch.phase === 'cancelled') return null;
  const phase = orch.phase === 'active' && orch.activation === 'disarmed' ? 'inactive' : orch.phase;
  const canResume = phase !== 'active';
  return html`<div class=${cx('goalbar', 'orchbar', `is-${phase}`)}>
    <div class="goal-top">
      <span class="goal-icon"><${IconFlow} /></span>
      <span class="goal-label">${t.orchPhase[phase] ?? phase}</span>
      <span class="goal-rounds-n">${t.orchRounds(orch.roundsStarted ?? 0, orch.limits?.maxRounds ?? '∞')}
        · ${t.orchWorkers(orch.workersDispatched ?? 0, orch.limits?.maxWorkers ?? '∞')}</span>
      <span class="spacer" />
      ${canResume
    ? html`<button type="button" class="link-btn" onClick=${() => orchestrationCommand({ action: 'resume' })}>${t.goalResume}</button>`
    : html`<button type="button" class="link-btn" onClick=${() => orchestrationCommand({ action: 'pause' })}>${t.goalPause}</button>`}
      <button type="button" class="link-btn" onClick=${async () => { if (await ask({ title: t.confirmCancelOrch, action: t.orchCancel })) orchestrationCommand({ action: 'cancel' }); }}>${t.orchCancel}</button>
      <button type="button" class="icon-btn" aria-label=${t.orchClear} title=${t.orchClear}
        onClick=${async () => { if (await ask({ title: t.confirmClearOrch, action: t.orchClear })) orchestrationCommand({ action: 'clear' }); }}><${IconX} size=${13} /></button>
    </div>
    <button type="button" class=${cx('goal-objective', open && 'is-open')} onClick=${() => setOpen((v) => !v)}>${orch.objective}</button>
    ${open ? html`<div class="orch-body">
      ${orch.blockedReason?.message ? html`<div class="goal-blocked">${orch.blockedReason.message}</div>` : null}
      <${Criteria} run=${orch} checks=${orch.checks} />
      <div class="orch-tasks">${(orch.tasks ?? []).map((task) => html`<${TaskChip} key=${task.id} task=${task} />`)}</div>
    </div>` : null}
  </div>`;
}
