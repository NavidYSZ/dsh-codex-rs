// What a message offers besides its text: copy, send again, edit and send
// again, branch into a new conversation (dsh `MessageIconActions`,
// `message.branch*`, Codex); failure notices with the action that helps
// (dsh `message.failure.*`), the output-limit hint with "Continue"
// (`message.maxTokens*`) and the compaction marker (`CompactionItem`).
import { useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t } from '../i18n.js';
import { useStore } from '../store.js';
import { store, api, send, openSession, refreshSessions, setDraft, newChat, fail } from '../state.js';
import { copyText } from '../markdown.js';
import { IconCopy, IconCheck, IconRefresh, IconPencil, IconAlert, IconChevronRight, IconExternal } from '../icons.js';
import { Shimmer } from './common.js';
import { Markdown } from './transcript.js';
import { useSelection } from './composer.js';
import { editProvider } from './models.js';
import { shortTokens } from './usage.js';
import { compactChat } from './composer-extras.js';

const S = { fill: 'none', stroke: 'currentColor', 'stroke-width': 1.3, 'stroke-linecap': 'round', 'stroke-linejoin': 'round' };
const IconBranch = ({ size = 14 }) => html`<svg class="icon" width=${size} height=${size} viewBox="0 0 16 16" aria-hidden="true">
  <circle cx="4.5" cy="3.5" r="1.6" ...${S}/><circle cx="4.5" cy="12.5" r="1.6" ...${S}/><circle cx="11.5" cy="5" r="1.6" ...${S}/>
  <path d="M4.5 5.1v5.8M11.5 6.6c0 2.7-2.3 3.6-5.6 4.6" ...${S}/></svg>`;
const IconContext = ({ size = 14 }) => html`<svg class="icon" width=${size} height=${size} viewBox="0 0 16 16" aria-hidden="true">
  <path d="M2.75 4.25h10.5M2.75 8h10.5M2.75 11.75h6" ...${S}/><path d="m11 10.25 1.75 1.5L11 13.25" ...${S}/></svg>`;

// ---- what the chat can do -------------------------------------------------------

/** Where a provider's balance is topped up (QUOTA failures). */
const TOP_UP = {
  'deepseek-official': 'https://platform.deepseek.com/top_up',
  anthropic: 'https://console.anthropic.com/settings/billing',
};

const SUFFIXES = [' (branch)', ' (Abzweig)'];
const baseTitle = (title) => SUFFIXES.reduce((s, x) => (s.endsWith(x) ? s.slice(0, -x.length) : s), title.trim());

/** A new chat with this one's log through `turn` (0 = none of it); `draft` waits in its composer. */
export async function forkChat(turn, { draft } = {}) {
  const s = store.get();
  const id = s.activeId;
  if (!id) return;
  const listed = s.sessions.find((x) => x.id === id);
  const title = t.branchTitle(baseTitle(listed?.title || t.newChat));
  try {
    const { id: child } = await api.forkSession(id, { turn, title });
    await openSession(child);
    refreshSessions();
    if (draft != null) setDraft(draft);
  } catch (e) {
    fail(e?.status === 400 || e?.status === 404 ? Object.assign(new Error(`${t.forkFailed} ${e.message}`), { status: e.status }) : e);
  }
}

const GERMAN = /[äöüß]|\b(und|der|die|das|nicht|ist|ich|du|bitte|mit|für|auf|eine?|wie|was|warum|noch|mach)\b/gi;
const ENGLISH = /\b(the|and|is|are|please|with|for|what|why|how|this|that|you|can|make)\b/gi;

/** "weiter" or "continue": the language of the prompt, else the UI's. */
export function continueWord(text = '') {
  const g = text.match(GERMAN)?.length ?? 0;
  const e = text.match(ENGLISH)?.length ?? 0;
  if (g > e) return 'weiter';
  if (e > g) return 'continue';
  return t.continueWord;
}

/**
 * Per chat: send again, edit and send again, branch, continue, and the
 * provider for failure links. `idle`: the agent waits, so a new prompt
 * starts a turn instead of joining one.
 */
export function useChatActions(tx) {
  const sel = useSelection(tx);
  const status = useStore(store, (s) => s.status);
  const pending = useStore(store, (s) => !!s.pending);
  const models = useStore(store, (s) => s.info?.models);
  const idle = (status === 'idle' || status === 'error') && !pending;
  return useMemo(() => {
    const opts = { model: sel.model, ...(sel.effort ? { effort: sel.effort } : {}), sandbox: sel.sandbox };
    const turns = tx.turns;
    const last = turns[turns.length - 1];
    const firstUser = (turn) => turn?.items.find((x) => x.kind === 'user') ?? null;
    return {
      idle,
      provider: models?.find((m) => m.id === tx.model)?.provider ?? null,
      /** The last prompt of the chat can be sent again while the agent waits. */
      lastUser: idle ? [...(last?.items ?? [])].reverse().find((x) => x.kind === 'user')?.key ?? null : null,
      isLast: (turn) => turn === last,
      resend: (item) => send(item.text, item.images ?? [], opts),
      /** Sends the turn's prompt again (failure notices). */
      retry: (turn) => {
        const item = firstUser(turn);
        return item && idle && turn === last ? () => send(item.text, item.images ?? [], opts) : null;
      },
      /** Edit and send again: a branch right before the turn, the prompt in its composer. */
      editResend: (turn, item) => {
        const i = turns.indexOf(turn);
        const before = i > 0 ? turns[i - 1].turn : 0;
        return forkChat(before, { draft: item.text });
      },
      firstUser: (turn) => firstUser(turn)?.key ?? null,
      branch: (turn) => forkChat(turn.turn),
      continueWith: (turn) => (idle && turn === last ? () => send(continueWord(firstUser(turn)?.text), [], opts) : null),
    };
  }, [tx, idle, sel.model, sel.effort, sel.sandbox, models]);
}

// ---- action rows ------------------------------------------------------------------

function Action({ label, hint, onClick, disabled, children }) {
  return html`<button type="button" class="msg-action" aria-label=${label} data-tip=${hint ?? label}
    aria-disabled=${disabled || undefined} onClick=${disabled ? undefined : onClick}>${children}</button>`;
}

function CopyAction({ text }) {
  const [done, setDone] = useState(false);
  const timer = useRef(0);
  useEffect(() => () => clearTimeout(timer.current), []);
  const copy = async () => {
    await copyText(text);
    setDone(true);
    clearTimeout(timer.current);
    timer.current = setTimeout(() => setDone(false), 1000);
  };
  return html`<${Action} label=${done ? t.copied : t.copy} onClick=${copy}>
    ${done ? html`<${IconCheck} size=${14} />` : html`<${IconCopy} size=${14} />`}
  </${Action}>`;
}

/** Copy, send again (last prompt) and edit and send again, right-aligned under the bubble. */
export function UserActions({ item, turn, acts }) {
  const edit = acts.firstUser(turn) === item.key;
  return html`<div class="msg-actions is-user">
    ${item.text ? html`<${CopyAction} text=${item.text} />` : null}
    ${acts.lastUser === item.key ? html`<${Action} label=${t.resend} onClick=${() => acts.resend(item)}><${IconRefresh} size=${14} /></${Action}>` : null}
    ${edit && item.text ? html`<${Action} label=${t.editResend} hint=${t.editResendHint} onClick=${() => acts.editResend(turn, item)}>
      <${IconPencil} size=${14} /></${Action}>` : null}
  </div>`;
}

/** Copy and "Branch into a new conversation" under a finished turn's answer. */
export function AnswerActions({ item, turn, acts, shown }) {
  const done = turn.status === 'completed';
  return html`<div class=${cx('msg-actions', shown && 'is-shown')}>
    <${CopyAction} text=${item.text} />
    <${Action} label=${t.branch} hint=${done ? t.branch : t.branchUnavailable} disabled=${!done} onClick=${() => acts.branch(turn)}>
      <${IconBranch} size=${14} />
    </${Action}>
  </div>`;
}

// ---- notices ------------------------------------------------------------------------

/** A failed turn: what happened in plain words, the provider's own message, and what helps. */
export function FailureNotice({ item, turn, acts, cwd }) {
  const f = item.failure ?? {};
  const code = f.code ?? '';
  const known = t.failure[code];
  const provider = item.provider ?? acts.provider;
  const full = code === 'CONTEXT_WINDOW_EXCEEDED';
  const retry = acts.retry(turn);
  const top = code === 'QUOTA' ? TOP_UP[provider] : null;
  const buttons = [];
  if (code === 'AUTH') buttons.push(html`<button type="button" key="key" class="btn btn-small btn-primary" onClick=${() => editProvider(provider)}>${t.checkKey}</button>`);
  if (top) {
    buttons.push(html`<a key="top" class="btn btn-small btn-primary" href=${top} target="_blank" rel="noopener noreferrer">
      ${t.topUp}<${IconExternal} size=${12} /></a>`);
  } else if (code === 'QUOTA') {
    buttons.push(html`<button type="button" key="prov" class="btn btn-small" onClick=${() => editProvider(provider)}>${t.providerSettings}</button>`);
  }
  // A full context: compact (dsh /compact), then send again, or move on in a new chat.
  if (full && retry) buttons.push(html`<button type="button" key="compact" class="btn btn-small btn-primary" onClick=${compactChat}>${t.ciCompactNow}</button>`);
  if (retry) buttons.push(html`<button type="button" key="retry" class="btn btn-small" onClick=${retry}><${IconRefresh} size=${13} />${t.resend}</button>`);
  if (full) buttons.push(html`<button type="button" key="new" class="btn btn-small" onClick=${() => newChat(cwd)}>${t.newChatAction}</button>`);
  const codeTag = code ? html`<span class="notice-code">${code}${f.status ? ` · ${f.status}` : ''}</span>` : null;
  return html`<div class="notice notice-error" role="alert" data-code=${code || undefined}>
    <span class="notice-icon"><${IconAlert} size=${16} /></span>
    <div class="notice-body">
      <div class="notice-title">${known ? known[0] : t.turnFailed}</div>
      ${known
    ? html`<div class="notice-text">${known[1]}</div>${f.message ? html`<div class="notice-detail">${f.message} ${codeTag}</div>` : codeTag}`
    : html`<div class="notice-text">${f.message || ''} ${codeTag}</div>`}
      ${buttons.length ? html`<div class="notice-actions">${buttons}</div>` : null}
    </div>
  </div>`;
}

/** dsh `message.maxTokens.hint` with a button that sends "continue" in the prompt's language. */
export function MaxTokensNotice({ turn, acts }) {
  const go = acts.continueWith(turn);
  return html`<div class="notice notice-warn" role="status">
    <span class="notice-icon"><${IconAlert} size=${16} /></span>
    <div class="notice-body">
      <div class="notice-title">${t.maxTokensTitle}</div>
      <div class="notice-text">${t.maxTokensHint}</div>
      ${go ? html`<div class="notice-actions"><button type="button" class="btn btn-small btn-primary" onClick=${go}>${t.continueAction}</button></div>` : null}
    </div>
  </div>`;
}

/** "Compacted N history items (~X tokens)"; a click shows the summary. */
export function CompactionNotice({ item }) {
  const [open, setOpen] = useState(false);
  if (item.running) {
    return html`<div class="cmp is-running" role="status"><span class="cmp-row">
      <span class="cmp-icon"><${IconContext} /></span><${Shimmer} active><span class="cmp-title">${t.compactionRunning}</span></${Shimmer}>
    </span></div>`;
  }
  const expandable = !!item.summary;
  let summary;
  if (item.error) summary = '';
  else if (item.items != null && item.tokens != null) summary = t.compactionDone(item.items, shortTokens(item.tokens));
  else summary = expandable ? t.compactionExpand : t.compactionUnavailable;
  return html`<div class=${cx('cmp', open && 'is-open', item.error && 'is-failed')}>
    <button type="button" class="cmp-row" disabled=${!expandable} aria-expanded=${expandable ? open : undefined}
      title=${item.error || undefined} onClick=${() => setOpen((v) => !v)}>
      <span class="cmp-icon"><${IconContext} /></span>
      <span class="cmp-title">${item.error ? t.compactionFailed : t.compaction}</span>
      ${summary ? html`<span class="cmp-sep" aria-hidden="true" /><span class="cmp-summary">${summary}</span>` : null}
      ${expandable ? html`<span class="chev"><${IconChevronRight} size=${12} /></span>` : null}
    </button>
    ${open ? html`<div class="cmp-body"><${Markdown} text=${item.summary} /></div>` : null}
  </div>`;
}
