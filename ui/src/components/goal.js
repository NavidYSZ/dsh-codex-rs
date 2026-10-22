// Goal bar above the composer (dsh GoalBar): the chat's goal with its phase
// and rounds, pause/resume/edit/remove, and the form that sets a new one.
import { useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t } from '../i18n.js';
import { useStore } from '../store.js';
import { store, goalCommand, ask } from '../state.js';
import { IconX } from '../icons.js';

const IconTarget = ({ size = 14 }) => html`<svg class="icon" width=${size} height=${size} viewBox="0 0 16 16" aria-hidden="true">
  <circle cx="8" cy="8" r="6.25" fill="none" stroke="currentColor" stroke-width="1.3"/>
  <circle cx="8" cy="8" r="3.4" fill="none" stroke="currentColor" stroke-width="1.3"/>
  <circle cx="8" cy="8" r="1" fill="currentColor"/></svg>`;

export const openGoalForm = () => store.set({ goalForm: true });
const closeGoalForm = () => store.set({ goalForm: false });

function GoalForm({ goal }) {
  const [objective, setObjective] = useState(goal?.objective ?? '');
  const [rounds, setRounds] = useState('');
  const editing = !!goal;
  const submit = async (e) => {
    e.preventDefault();
    const text = objective.trim();
    if (!text) return;
    const max = Number(rounds);
    const body = editing
      ? { action: 'edit', objective: text }
      : { action: 'create', objective: text, ...(Number.isInteger(max) && max > 0 ? { maxGoalRounds: max } : {}) };
    if (await goalCommand(body)) closeGoalForm();
  };
  return html`<form class="goalbar goal-form" onSubmit=${submit}>
    <div class="goal-top">
      <span class="goal-icon"><${IconTarget} /></span>
      <span class="goal-label">${editing ? t.goalEdit : t.setGoal}</span>
      <span class="spacer" />
      <button type="button" class="icon-btn" aria-label=${t.cancel} onClick=${closeGoalForm}><${IconX} size=${14} /></button>
    </div>
    <textarea class="goal-input" rows="2" value=${objective} placeholder=${t.goalPlaceholder} aria-label=${t.goal}
      ref=${(el) => { if (el && !el.dataset.focused) { el.dataset.focused = '1'; el.focus(); } }}
      onInput=${(e) => setObjective(e.currentTarget.value)}
      onKeyDown=${(e) => { if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) submit(e); if (e.key === 'Escape') closeGoalForm(); }} />
    <div class="goal-actions">
      ${editing ? null : html`<label class="goal-rounds">${t.goalMaxRounds}
        <input class="input" type="number" min="1" step="1" value=${rounds} placeholder="20" onInput=${(e) => setRounds(e.currentTarget.value)} /></label>`}
      <span class="spacer" />
      <button type="submit" class="btn btn-small btn-primary" disabled=${!objective.trim()}>${editing ? t.goalSave : t.goalStart}</button>
    </div>
  </form>`;
}

export function GoalBar() {
  const goal = useStore(store, (s) => s.goal);
  const form = useStore(store, (s) => s.goalForm);
  const [open, setOpen] = useState(false);
  if (form) return html`<${GoalForm} goal=${goal && goal.phase !== 'complete' ? goal : null} />`;
  if (!goal || goal.phase === 'complete') return null;
  const phase = goal.phase === 'active' && goal.activation === 'disarmed' ? 'inactive' : goal.phase;
  const canResume = phase !== 'active';
  return html`<div class=${cx('goalbar', `is-${phase}`)}>
    <div class="goal-top">
      <span class="goal-icon"><${IconTarget} /></span>
      <span class="goal-label">${t.goalPhase[phase] ?? phase}</span>
      <span class="goal-rounds-n">${t.goalRounds(goal.roundsStarted ?? 0, goal.maxGoalRounds ?? '∞')}</span>
      <span class="spacer" />
      ${canResume
    ? html`<button type="button" class="link-btn" onClick=${() => goalCommand({ action: 'resume' })}>${t.goalResume}</button>`
    : html`<button type="button" class="link-btn" onClick=${() => goalCommand({ action: 'pause' })}>${t.goalPause}</button>`}
      <button type="button" class="link-btn" onClick=${() => store.set({ goalForm: true })}>${t.goalEdit}</button>
      <button type="button" class="icon-btn" aria-label=${t.goalClear} title=${t.goalClear}
        onClick=${async () => { if (await ask({ title: t.confirmClearGoal, action: t.goalClear })) goalCommand({ action: 'clear' }); }}><${IconX} size=${13} /></button>
    </div>
    <button type="button" class=${cx('goal-objective', open && 'is-open')} onClick=${() => setOpen((v) => !v)}>${goal.objective}</button>
    ${goal.blockedReason?.message ? html`<div class="goal-blocked">${goal.blockedReason.message}</div>` : null}
  </div>`;
}
