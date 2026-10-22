// The chat's current plan above the composer (dsh TodoPanel in the input
// dock): the latest `todo_write` list as one line with the item in progress
// and "1 of 3 done", unfolding into the checklist. Like dsh's projection it
// stays while items are left; a finished list goes when its turn ends.
import { useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t } from '../i18n.js';
import { useStore } from '../store.js';
import { store } from '../state.js';
import { IconList, IconChevronDown } from '../icons.js';
import { TodoList } from './tools.js';

/** The list to show for `tx`, or null. */
export function currentPlan(tx, running) {
  const todos = tx?.todos;
  if (!todos?.length) return null;
  const done = todos.filter((x) => x.status === 'completed').length;
  if (done < todos.length) return { todos, done };
  const last = tx.turns[tx.turns.length - 1];
  return running && last && tx.todosTurn === last.turn ? { todos, done } : null;
}

export function PlanBar({ tx }) {
  const running = useStore(store, (s) => s.status === 'running' || s.status === 'waiting-approval');
  const [open, setOpen] = useState(false);
  const plan = currentPlan(tx, running);
  if (!plan) return null;
  const { todos, done } = plan;
  const now = todos.find((x) => x.status === 'in_progress') ?? todos.find((x) => x.status === 'pending');
  return html`<section class=${cx('planbar', open && 'is-open')} aria-label=${t.plan}>
    <button type="button" class="plan-head" aria-expanded=${open} onClick=${() => setOpen((v) => !v)}>
      <span class="plan-icon"><${IconList} size=${14} /></span>
      <span class="plan-title">${t.plan}</span>
      <span class=${cx('plan-now', running && now?.status === 'in_progress' && 'is-live')}>${now ? now.content : t.planAllDone}</span>
      <span class="plan-progress">${t.planDone(done, todos.length)}</span>
      <span class="plan-chev"><${IconChevronDown} size=${14} /></span>
    </button>
    ${open ? html`<${TodoList} todos=${todos} class="plan-list" />` : null}
  </section>`;
}
