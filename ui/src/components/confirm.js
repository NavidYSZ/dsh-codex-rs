// "Enable Full access?" before the agent may act without asking (dsh
// permission-presets `confirm.*`): the risk, "I understand the risks" and
// "Don't ask again", which ui-state.json keeps for every browser.
import { useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t } from '../i18n.js';
import { useStore } from '../store.js';
import { store, answerAsk } from '../state.js';
import { Dialog } from './models.js';
import { setUiPref } from './worklog.js';
import { Field, Segmented } from './settings.js';

let answer = null;

/**
 * Resolves `true` once the user enabled Full access, at once when they chose
 * not to be asked again. `scope`: `chat` (the composer) or `defaults` (Settings).
 */
export function confirmFullAccess(scope = 'chat') {
  if (store.get().info?.ui?.confirmFullAccess === false) return Promise.resolve(true);
  answer?.(false);
  return new Promise((resolve) => {
    answer = resolve;
    store.set({ fullAccessAsk: scope });
  });
}

function finish(ok, dontAsk) {
  const resolve = answer;
  answer = null;
  store.set({ fullAccessAsk: null });
  if (ok && dontAsk) setUiPref({ confirmFullAccess: false });
  resolve?.(ok);
}

function Body({ scope }) {
  const [ack, setAck] = useState(false);
  const [dontAsk, setDontAsk] = useState(false);
  const footer = html`<button type="button" class="btn" onClick=${() => finish(false)}>${t.cancel}</button>
    <button type="submit" form="fa-confirm" class="btn btn-warn" disabled=${!ack}>${t.fullAccessEnable}</button>`;
  return html`<${Dialog} title=${t.fullAccessTitle} class="md-small fa-dialog" onClose=${() => finish(false)} footer=${footer}
      description=${scope === 'defaults' ? t.fullAccessTextDefaults : t.fullAccessText}>
    <form id="fa-confirm" class="fa-form" onSubmit=${(e) => { e.preventDefault(); if (ack) finish(true, dontAsk); }}
      onKeyDown=${(e) => { if (e.key === 'Enter' && ack) { e.preventDefault(); finish(true, dontAsk); } }}>
      <label class="fa-check"><input type="checkbox" checked=${ack} onChange=${(e) => setAck(e.currentTarget.checked)} />
        <span>${t.fullAccessAck}</span></label>
      <label class="fa-check is-quiet"><input type="checkbox" checked=${dontAsk} onChange=${(e) => setDontAsk(e.currentTarget.checked)} />
        <span>${t.fullAccessDontAsk}</span></label>
    </form>
  </${Dialog}>`;
}

/** Settings: the way back after "Don't ask again". */
export function FullAccessSetting() {
  const on = useStore(store, (s) => s.info?.ui?.confirmFullAccess !== false);
  return html`<${Field} label=${t.fullAccessAsk} hint=${t.fullAccessAskHint}>
    <${Segmented} label=${t.fullAccessAsk} value=${on ? 'on' : 'off'} onChange=${(v) => setUiPref({ confirmFullAccess: v === 'on' })}
      options=${[['on', t.enabled], ['off', t.disabledLabel]]} />
  </${Field}>`;
}

/** Mounted once by the app shell. */
export function FullAccessConfirm() {
  const scope = useStore(store, (s) => s.fullAccessAsk);
  return scope ? html`<${Body} scope=${scope} />` : null;
}

/** The question of `ask()` (state.js); Cancel has the focus, Esc cancels. */
export function AskDialog() {
  const q = useStore(store, (s) => s.ask);
  if (!q) return null;
  return html`<${Dialog} title=${q.title} description=${q.text} onClose=${() => answerAsk(false)} class="md-small"
    footer=${html`<button type="button" class="btn" onClick=${() => answerAsk(false)}>${t.cancel}</button>
      <button type="button" class=${cx('btn', q.danger ? 'btn-danger' : 'btn-primary')} onClick=${() => answerAsk(true)}>${q.action}</button>`} />`;
}
