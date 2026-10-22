// Composer seat: approval card, banners, the hero tray (workspace + access),
// the borderless composer card and the usage bar under it.
import { useEffect, useRef, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t } from '../i18n.js';
import { useStore } from '../store.js';
import {
  store, send, cancel, decide, setComposer, openSettings,
} from '../state.js';
import {
  IconPlus, IconArrowUp, IconStop, IconChevronDown, IconFolder, IconShield, IconAlert, IconTerminal, IconPencil, IconTool,
} from '../icons.js';
import { Menu, MenuItem, ActionItem } from './common.js';
import { GoalBar, openGoalForm } from './goal.js';
import { PlanBar } from './plan.js';
import { ModeChip, useChatMode, useStagedMode } from './modes.js';
import { ProjectMenu } from './folders.js';
import { ModelChip, openModelPicker } from './models.js';
import { IconImage } from '../icons.js';
import { parseArgs } from '../reduce.js';
import { UsageBar } from './usage.js';
import { confirmFullAccess } from './confirm.js';
import { ComposerInput, insertAtCaret } from './composer-input.js';
import { useAttachments, AttachmentRow } from './composer-attach.js';
import { ComposerExtras } from './composer-extras.js';

const ACCESS = ['read-only', 'workspace-write', 'danger-full-access'];

export function workspaceName(path) {
  if (!path) return '';
  return path.split('/').filter(Boolean).pop() || path;
}

/** Model, effort and access for the next prompt: composer pick > session > defaults. */
export function useSelection(tx) {
  const info = useStore(store, (s) => s.info);
  const pick = useStore(store, (s) => s.composer);
  const d = info?.settings ?? {};
  const model = pick.model ?? tx?.model ?? d.model ?? info?.models?.[0]?.id ?? '';
  const modelInfo = info?.models?.find((m) => m.id === model) ?? null;
  let effort = pick.effort ?? (tx && tx.effort !== undefined ? tx.effort : undefined) ?? d.effort ?? modelInfo?.defaultReasoningEffort ?? null;
  if (modelInfo?.reasoningEfforts?.length && effort && !modelInfo.reasoningEfforts.includes(effort)) effort = modelInfo.defaultReasoningEffort ?? null;
  const sandbox = pick.sandbox ?? tx?.sandbox ?? d.sandbox ?? 'workspace-write';
  // A model this server no longer offers (its provider was removed, say).
  const missing = !!model && !!info?.models?.length && !modelInfo;
  return { model, effort: missing ? null : effort, sandbox, modelInfo, models: info?.models ?? [], missing };
}

function AccessChip({ sel, side = 'top', align = 'start', plain }) {
  const [open, setOpen] = useState(false);
  const anchor = useRef(null);
  const full = sel.sandbox === 'danger-full-access';
  const [name] = t.access[sel.sandbox] ?? [sel.sandbox];
  return html`<span class="chip-wrap">
    <button type="button" ref=${anchor} class=${cx('chip', 'chip-access', full && 'is-full', plain && 'chip-plain')}
      aria-label=${name} aria-haspopup="menu" aria-expanded=${open} onClick=${() => setOpen((v) => !v)}>
      ${full ? html`<${IconAlert} size=${14} />` : html`<${IconShield} size=${14} />`}
      <span class="chip-label">${name}</span>
      ${full ? null : html`<span class="chip-chev"><${IconChevronDown} size=${12} /></span>`}
    </button>
    <${Menu} open=${open} onClose=${() => setOpen(false)} anchor=${anchor} side=${side} align=${align} class="menu-wide">
      ${ACCESS.map((mode) => html`<${MenuItem} key=${mode} checked=${mode === sel.sandbox} hint=${t.access[mode][1]}
        tone=${mode === 'danger-full-access' ? 'warn' : null}
        onClick=${async () => {
          setOpen(false);
          if (mode === 'danger-full-access' && sel.sandbox !== mode && !(await confirmFullAccess())) return;
          setComposer({ sandbox: mode });
        }}>${t.access[mode][0]}</${MenuItem}>`)}
    </${Menu}>
  </span>`;
}

// The project menu ("Choose folder…" and recent projects) lives in folders.js.
export const WorkspaceMenu = ProjectMenu;

/** Workspace and access, tucked behind the composer's top edge on the start page. */
function HeroTray({ sel }) {
  const cwd = useStore(store, (s) => s.cwd);
  const [open, setOpen] = useState(false);
  const anchor = useRef(null);
  return html`<div class="tray">
    <span class="chip-wrap">
      <button type="button" ref=${anchor} class="chip chip-plain" onClick=${() => setOpen((v) => !v)} aria-haspopup="menu" aria-expanded=${open}>
        <${IconFolder} size=${14} /><span class="chip-label">${workspaceName(cwd) || t.noProject}</span>
      </button>
      <${WorkspaceMenu} open=${open} onClose=${() => setOpen(false)} anchor=${anchor} />
    </span>
    <${ModeChip} />
    <${AccessChip} sel=${sel} plain />
  </div>`;
}

/** Unsent text per chat ('' = the start page), while the page lives. */
const drafts = new Map();

export function Composer({ hero, tx }) {
  const status = useStore(store, (s) => s.status);
  const info = useStore(store, (s) => s.info);
  const activeId = useStore(store, (s) => s.activeId);
  const sel = useSelection(tx);
  const [text, setText] = useState(() => drafts.get(store.get().activeId ?? '') ?? '');
  const [drag, setDrag] = useState(false);
  const area = useRef(null);
  const file = useRef(null);
  const composing = useRef(false);
  const att = useAttachments({ sel, insert: (s) => insertAtCaret(area.current, s) });

  const running = status === 'running' || status === 'waiting-approval';
  const noKey = info && !info.apiKeyConfigured;
  const canSend = !noKey && !sel.missing && !att.uploading && (text.trim() || att.items.length);

  // Each chat keeps its own unsent text.
  const textNow = useRef(text);
  textNow.current = text;
  const draftKey = useRef(activeId ?? '');
  useEffect(() => {
    const key = activeId ?? '';
    if (draftKey.current === key) return;
    if (textNow.current.trim()) drafts.set(draftKey.current, textNow.current);
    else drafts.delete(draftKey.current);
    draftKey.current = key;
    setText(drafts.get(key) ?? '');
  }, [activeId]);
  useEffect(() => () => { if (textNow.current.trim()) drafts.set(draftKey.current, textNow.current); }, []);
  useEffect(() => { area.current?.focus({ preventScroll: true }); }, [activeId]);
  // "New automation" and similar hand over a draft once.
  const draft = useStore(store, (s) => s.draft);
  useEffect(() => {
    if (draft == null) return;
    setText(draft);
    store.set({ draft: null });
    requestAnimationFrame(() => { const el = area.current; if (el) { el.focus(); el.setSelectionRange(el.value.length, el.value.length); } });
  }, [draft]);
  const [plus, setPlus] = useState(false);
  const plusAnchor = useRef(null);
  const chatMode = useChatMode();
  const goals = !!info?.automation?.goals && !!activeId && chatMode !== 'minimal';
  useEffect(() => {
    const el = area.current;
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = `${Math.min(el.scrollHeight, Math.round(window.innerHeight * 0.4))}px`;
  }, [text]);

  // While a turn runs, a message either steers it (taken at its next step) or
  // queues as the next turn; ⌘↩ picks the other one for this message.
  const followup = useStore(store, (s) => s.followup);
  const submit = async (other = false) => {
    if (running && !canSend) { cancel(); return; }
    if (!canSend) return;
    const typed = text.trim();
    const body = att.withFiles(typed);
    const refs = att.refs();
    setText('');
    att.clear();
    const steer = running && (followup === 'steer') !== other;
    const ok = await send(body, refs, { model: sel.model, ...(sel.effort ? { effort: sel.effort } : {}), sandbox: sel.sandbox, ...(steer ? { steer } : {}) });
    if (!ok) { setText(typed); }
  };

  const onKey = (e) => {
    if (e.key === 'Enter' && !e.shiftKey && !composing.current && !e.isComposing) {
      e.preventDefault();
      submit(e.metaKey || e.ctrlKey);
    }
  };

  // Minimal has neither skills nor apps: the hint names only what works.
  const stagedMode = useStagedMode();
  const minimal = (hero ? stagedMode : chatMode) === 'minimal';
  const placeholder = noKey ? t.placeholderNoKey : hero ? (minimal ? t.placeholderHeroMinimal : t.placeholderHero)
    : running ? t.placeholderRunning[followup] : minimal ? t.placeholderMinimal : t.placeholder;
  const showStop = running && !canSend;
  return html`<div class=${cx('composer', drag && 'is-drag')}
      onDragOver=${(e) => { if ([...(e.dataTransfer?.types ?? [])].includes('Files')) { e.preventDefault(); setDrag(true); } }}
      onDragLeave=${() => setDrag(false)}
      onDrop=${(e) => { e.preventDefault(); setDrag(false); att.drop(e.dataTransfer); }}>
    <${AttachmentRow} att=${att} />
    <${ComposerInput} areaRef=${area} value=${text} onValue=${setText} placeholder=${placeholder} onKeyDown=${onKey} model=${sel.model} hero=${hero}
      onCompositionStart=${() => { composing.current = true; }} onCompositionEnd=${() => { composing.current = false; }}
      onPaste=${(e) => { const fs = e.clipboardData?.files; if (fs?.length) { e.preventDefault(); att.add(fs); } }} />
    <div class="composer-foot">
      <span class="chip-wrap">
        <button type="button" ref=${plusAnchor} class="icon-btn add" aria-label=${t.attach} title=${t.attach} aria-haspopup="menu"
          onClick=${() => (goals ? setPlus((v) => !v) : file.current?.click())}>
          <${IconPlus} size=${18} />
        </button>
        <${Menu} open=${plus} onClose=${() => setPlus(false)} anchor=${plusAnchor} side="top" align="start" class="menu-actions">
          <${ActionItem} icon=${html`<${IconImage} size=${14} />`} onClick=${() => { setPlus(false); file.current?.click(); }}>${t.attachFile}</${ActionItem}>
          <${ActionItem} icon=${html`<svg class="icon" width="14" height="14" viewBox="0 0 16 16"><circle cx="8" cy="8" r="6.25" fill="none" stroke="currentColor" stroke-width="1.3"/><circle cx="8" cy="8" r="3.4" fill="none" stroke="currentColor" stroke-width="1.3"/><circle cx="8" cy="8" r="1" fill="currentColor"/></svg>`}
            onClick=${() => { setPlus(false); openGoalForm(); }}>${t.setGoal}</${ActionItem}>
        </${Menu}>
      </span>
      <input ref=${file} type="file" multiple hidden
        onChange=${(e) => { att.add(e.currentTarget.files); e.currentTarget.value = ''; }} />
      <${ModelChip} sel=${sel} />
      ${hero ? null : html`<${AccessChip} sel=${sel} />`}
      <span class="spacer" />
      <button type="button" class=${cx('send', showStop && 'is-stop')} disabled=${!showStop && !canSend}
        aria-label=${showStop ? t.stop : t.send} title=${showStop ? t.stop : t.send} onClick=${() => submit()}>
        ${showStop ? html`<${IconStop} size=${16} />` : html`<${IconArrowUp} size=${16} />`}
      </button>
    </div>
  </div>`;
}

// ---- approval --------------------------------------------------------------

const APPROVE_ICON = { bash: IconTerminal, edit: IconPencil, write: IconPencil };

/** Pending approval from the live message, or rebuilt from approval/asked after a reload. */
function useApproval(tx) {
  const live = useStore(store, (s) => s.approval);
  const status = useStore(store, (s) => s.status);
  if (live) return live;
  const a = tx?.pendingApproval;
  if (!a || status !== 'waiting-approval' || !a.callId) return null;
  let summary = a.toolName;
  for (const turn of tx.turns) {
    for (const x of turn.items) {
      if (x.kind === 'tool' && x.id === a.callId) {
        const args = parseArgs(x.argsRaw);
        summary = args.command ?? args.file_path ?? summary;
      }
    }
  }
  return { callId: a.callId, tool: a.toolName, summary, justification: a.reason };
}

function ApprovalCard({ approval }) {
  const Icon = APPROVE_ICON[approval.tool] ?? IconTool;
  const title = t.approveTitle[approval.tool] ?? t.approveTitle.other;
  return html`<div class="approval" role="alertdialog" aria-label=${title}>
    <div class="approval-head"><${IconShield} size=${16} /><span>${title}</span></div>
    <div class="approval-cmd"><span class="approval-tool"><${Icon} size=${14} /></span><code>${approval.summary}</code></div>
    ${approval.justification ? html`<div class="approval-why">${approval.justification}</div>` : null}
    <div class="approval-actions">
      <button type="button" class="btn" onClick=${() => decide(approval.callId, 'rejected')}>${t.deny}</button>
      <button type="button" class="btn btn-primary" onClick=${() => decide(approval.callId, 'allowed-once')}>${t.allowOnce}</button>
    </div>
  </div>`;
}

// ---- banners -----------------------------------------------------------------

/** Whether the chat's last turn ended with a failure card (it says what happened). */
const failureShown = (tx) => !!tx?.turns[tx.turns.length - 1]?.items.some((x) => x.kind === 'notice' && x.code === 'error');

function Banners({ tx, sel }) {
  const info = useStore(store, (s) => s.info);
  const conn = useStore(store, (s) => s.conn);
  const statusError = useStore(store, (s) => s.statusError);
  const toast = useStore(store, (s) => s.toast);
  const list = [];
  if (conn === 'unauthorized') list.push({ key: 'auth', tone: 'error', text: t.unauthorized });
  else if (conn === 'offline') list.push({ key: 'off', tone: 'error', text: t.offline });
  else if (conn === 'retry') list.push({ key: 'retry', tone: 'muted', text: t.reconnecting });
  if (info && !info.apiKeyConfigured) list.push({ key: 'key', tone: 'info', text: t.noKey, action: [t.openSettings, () => openSettings('models')] });
  if (sel?.missing) list.push({ key: 'model', tone: 'info', text: t.modelGone(sel.model), action: [t.pickModel, openModelPicker] });
  if (statusError && !failureShown(tx)) list.push({ key: 'status', tone: 'error', text: statusError });
  if (toast) list.push({ key: 'toast', tone: 'error', text: toast });
  if (!list.length) return null;
  return html`<div class="banners">${list.map((b) => html`<div key=${b.key} class=${cx('banner', `tone-${b.tone}`)} role="status">
    <span>${b.text}</span>${b.action ? html`<button type="button" class="link-btn" onClick=${b.action[1]}>${b.action[0]}</button>` : null}
  </div>`)}</div>`;
}

export function ComposerSeat({ hero, tx }) {
  const sel = useSelection(tx);
  const approval = useApproval(tx);
  return html`<div class=${cx('seat', hero && 'is-hero')}>
    <div class="column seat-column">
      <${Banners} tx=${tx} sel=${sel} />
      ${approval ? html`<${ApprovalCard} approval=${approval} />` : null}
      ${hero ? null : html`<${GoalBar} />`}
      ${hero ? null : html`<${PlanBar} tx=${tx} />`}
      <${ComposerExtras} />
      ${hero ? html`<${HeroTray} sel=${sel} />` : null}
      <${Composer} hero=${hero} tx=${tx} />
      ${hero ? null : html`<${UsageBar} />`}
    </div>
  </div>`;
}
