// The conversation: user bubbles, Markdown answers, work groups (reasoning
// and tool rows behind one status line), turn notices and the live
// (streaming) tail of the running step.
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t, duration } from '../i18n.js';
import { renderMarkdown, onMarkdownClick } from '../markdown.js';
import { IconArrowDown, IconChevronRight, IconFile } from '../icons.js';
import { Shimmer, Thumb } from './common.js';
import { openPanel, openFile } from '../state.js';
import { collectChanges, Counts } from './panel.js';
import { staysVisible } from '../worklog.js';
import { WorkGroup, WorkItem, ReasoningLine, ThinkingRow, TurnLine, useWorkPolicy } from './worklog.js';
import { useChatActions, UserActions, AnswerActions, FailureNotice, MaxTokensNotice, CompactionNotice } from './messages.js';
import { useTurnWindow, LoadOlder, TurnRail } from './turnnav.js';
import { FindBar, firstMatch } from './find.js';
import { PillText } from './composer-input.js';

export function Markdown({ text, class: klass }) {
  const htmlText = useMemo(() => renderMarkdown(text), [text]);
  return html`<div class=${cx('md', klass)} onClick=${onMarkdownClick} dangerouslySetInnerHTML=${{ __html: htmlText }} />`;
}

function UserBubble({ text, images, pending }) {
  return html`<div class=${cx('user', pending && 'is-pending')}>
    ${images?.length ? html`<div class="user-images">${images.map((img) => html`<${Thumb} key=${img.attachmentId} image=${img} />`)}</div>` : null}
    ${text ? html`<div class="bubble"><${PillText} text=${text} /></div>` : null}
  </div>`;
}

/** The verbose view's reasoning: the whole text, open while and after it streams; a click folds it. */
function Reasoning({ text, ms, live }) {
  const [open, setOpen] = useState(true);
  const show = live || open;
  const label = live ? t.thinking : ms != null && ms >= 500 ? t.thoughtFor(duration(ms)) : t.thought;
  return html`<div class=${cx('reasoning', show && 'is-open')}>
    <button type="button" class="row is-expandable" onClick=${() => setOpen((v) => !v)} aria-expanded=${show}>
      <${Shimmer} active=${live}><span class="row-title">${label}</span></${Shimmer}>
      <span class="chev"><${IconChevronRight} size=${12} /></span>
    </button>
    ${show ? html`<div class="reasoning-body"><${Markdown} text=${text} /></div>` : null}
  </div>`;
}

/** Goal, reminder and Team lines, "Stopped" and "Interrupted"; failures live in messages.js. */
function Notice({ item }) {
  if (item.code === 'mcp-activation' || item.code === 'mcp-deactivation') {
    const on = item.code === 'mcp-activation';
    return html`<div class="source-line"><span class="source-label">${t.ciMcpLabel}</span>
      <span class="source-detail">${on ? t.ciActivated(item.server) : t.ciDeactivated(item.server)}</span></div>`;
  }
  if (item.code === 'job') {
    return html`<div class="source-line"><span class="source-label">${t.jobLabel}</span><span class="source-detail" title=${item.text}>${item.text}</span></div>`;
  }
  if (item.code === 'goal' || item.code === 'schedule' || item.code === 'team-message') {
    const [label, detail] = item.code === 'goal' ? [t.goal, t.goalRounds(item.round ?? '?', item.max ?? '∞')]
      : item.code === 'schedule' ? [t.automations, item.prompt] : [t.teamMessageFrom(item.sender), item.text];
    return html`<div class="source-line"><span class="source-label">${label}</span>${detail ? html`<span class="source-detail">${detail}</span>` : null}</div>`;
  }
  const text = item.code === 'interrupted' ? t.interrupted : t.stopped;
  return html`<div class=${cx('notice-line', `tone-${item.tone}`)}>${text}</div>`;
}

function Retry({ retry }) {
  const [, tick] = useState(0);
  useEffect(() => {
    const id = setInterval(() => tick((n) => n + 1), 500);
    return () => clearInterval(id);
  }, []);
  const left = Math.max(0, retry.time + retry.delayMs - Date.now());
  const why = retry.failure?.message ?? '';
  return html`<div class="notice-line tone-warn">${t.retrying(retry.retry, retry.max ?? '∞', duration(Math.max(left, 1000)), why)}</div>`;
}

/** Codex's "N files changed" card under a finished turn. */
function ChangesCard({ turn, cwd }) {
  const changes = useMemo(() => collectChanges([turn], cwd), [turn, cwd]);
  if (!changes.length) return null;
  const total = changes.reduce((n, f) => ({ add: n.add + f.add, del: n.del + f.del }), { add: 0, del: 0 });
  const shown = changes.slice(0, 6);
  return html`<div class="changes-card">
    <div class="changes-head">
      <span class="changes-title">${t.filesChanged(changes.length)}</span><${Counts} ...${total} />
      <span class="spacer" />
      <button type="button" class="link-btn" onClick=${() => openPanel('changes')}>${t.review}</button>
    </div>
    ${shown.map((f) => html`<button type="button" key=${f.path} class="changes-file" onClick=${() => openFile(f.path)}>
      <${IconFile} size=${13} /><span class="changes-path" title=${f.rel}>${f.rel}</span><${Counts} add=${f.add} del=${f.del} />
    </button>`)}
    ${changes.length > shown.length ? html`<button type="button" class="changes-file changes-more" onClick=${() => openPanel('changes')}>+${changes.length - shown.length}</button>` : null}
  </div>`;
}

const SOURCES = new Set(['goal', 'schedule', 'team-message', 'job', 'mcp-activation', 'mcp-deactivation']);

/**
 * The running turn's segments plus what streams: reasoning and starting tool
 * calls join the open group (or open one), text closes it.
 */
function liveSegments(turn, deltas) {
  const segs = [...turn.segments];
  let groups = turn.groups;
  const last = segs[segs.length - 1];
  let open = null;
  if (last?.kind === 'group') {
    open = { ...last, closed: false, live: [], prep: null };
    segs[segs.length - 1] = open;
  }
  const blocks = deltas && deltas.turn === turn.turn
    ? Object.entries(deltas.blocks).map(([i, b]) => ({ ...b, index: Number(i) })).sort((a, b) => a.index - b.index)
    : [];
  let streaming = false;
  blocks.forEach((b, k) => {
    const next = blocks[k + 1];
    if (b.type === 'text') {
      if (!b.text) return;
      if (open) open.closed = true;
      open = null;
      streaming = true;
      segs.push({ kind: 'live-text', key: `live-t-${deltas.key}-${b.index}`, text: b.text, streaming: !next });
      return;
    }
    if (b.type === 'reasoning' && !b.text) return;
    if (!open) {
      open = { kind: 'group', key: `g:${groups++}`, items: [], counts: [], closed: false, live: [], prep: null };
      segs.push(open);
    }
    // Reasoning stays "Thinking" only until the next block starts streaming.
    if (b.type === 'reasoning') open.live.push({ key: `live-r-${deltas.key}-${b.index}`, text: b.text, live: !next, ms: next ? next.start - b.start : null });
    else if (b.type === 'tool-call') open.prep = { name: b.text };
  });
  return { segs, open, streaming };
}

/**
 * One turn in the current work-details view (dsh `POLICIES`): work groups
 * behind status lines, and once it is done, folded under one turn line.
 */
function Turn({ turn, live, deltas, cwd, policy, acts }) {
  const [unfolded, setUnfolded] = useState(false);
  const work = turn.work;
  const ended = !live && turn.end != null;
  const waiting = turn.items.some((x) => x.kind === 'tool' && x.approval && !x.approval.outcome);
  // Stopped, failed and interrupted turns, steered turns and pending approvals stay open.
  const alwaysOpen = !ended || ['aborted', 'error', 'interrupted'].includes(turn.status) || work.steered || waiting;
  const foldable = ended && policy.foldCompletedTurns && !alwaysOpen && work.hidden;
  const folded = foldable && !unfolded;
  const flat = policy.stepGrouping === 'none' || (policy.stepGrouping === 'history' && !ended);
  const verbose = policy.stepGrouping === 'none';

  const { segs, open, streaming } = live ? liveSegments(turn, deltas) : { segs: turn.segments, open: null, streaming: false };

  const render = (x) => {
    switch (x.kind) {
      case 'user': return html`<div key=${x.key} class="msg msg-user">
        <${UserBubble} text=${x.text} images=${x.images} /><${UserActions} item=${x} turn=${turn} acts=${acts} />
      </div>`;
      case 'text':
        // The turn's answer: copy and branch (dsh puts both under the closing message).
        if (ended && x.key === work.answerKey) {
          return html`<div key=${x.key} class="msg msg-answer">
            <${Markdown} text=${x.text} /><${AnswerActions} item=${x} turn=${turn} acts=${acts} shown=${acts.isLast(turn)} />
          </div>`;
        }
        return html`<${Markdown} key=${x.key} text=${x.text} />`;
      case 'notice':
        if (x.code === 'error') return html`<${FailureNotice} key=${x.key} item=${x} turn=${turn} acts=${acts} cwd=${cwd} />`;
        if (x.code === 'maxTokens') return html`<${MaxTokensNotice} key=${x.key} turn=${turn} acts=${acts} />`;
        return html`<${Notice} key=${x.key} item=${x} />`;
      case 'divider': return html`<${CompactionNotice} key=${x.key} item=${x} />`;
      default: return null;
    }
  };
  // The verbose view keeps today's open reasoning; the others show one line.
  const reasoning = (b) => (verbose
    ? html`<${Reasoning} key=${b.key} text=${b.text} ms=${b.ms} live=${!!b.live} />`
    : html`<${ReasoningLine} key=${b.key} text=${b.text} ms=${b.ms} live=${!!b.live} preview=${policy.settledReasoningPreview} />`);

  const out = [];
  let lineShown = false;
  const line = () => {
    if (lineShown || !ended) return;
    lineShown = true;
    out.push(html`<${TurnLine} key="turn-line" turn=${turn} folded=${folded} foldable=${foldable} onToggle=${() => setUnfolded((v) => !v)} />`);
  };
  for (const seg of segs) {
    if (seg.kind === 'live-text') {
      out.push(html`<${Markdown} key=${seg.key} text=${seg.text} class=${seg.streaming ? 'is-streaming' : ''} />`);
      continue;
    }
    if (seg.kind === 'item') {
      const x = seg.item;
      // The turn line sits under the prompt (and a goal, reminder or team line).
      if (x.kind !== 'user' && !(x.kind === 'notice' && SOURCES.has(x.code))) line();
      if (folded && x.key !== work.answerKey && !staysVisible(x)) continue;
      out.push(render(x));
      continue;
    }
    line();
    if (folded) continue;
    if (flat) {
      for (const x of seg.items) out.push(x.kind === 'reasoning' ? reasoning(x) : html`<${WorkItem} key=${x.key} item=${x} cwd=${cwd} />`);
      for (const b of seg.live ?? []) out.push(reasoning(b));
    } else {
      out.push(html`<${WorkGroup} key=${seg.key} group=${seg} closed=${seg.closed || !live} policy=${policy} cwd=${cwd} />`);
    }
  }
  line();

  if (ended && turn.status === 'completed') out.push(html`<${ChangesCard} key="changes" turn=${turn} cwd=${cwd} />`);
  if (live && turn.retry) out.push(html`<${Retry} key="retry" retry=${turn.retry} />`);
  // Before anything streams, a status line (flat views: "Thinking") shows the agent is on it.
  const shown = open && (!flat || open.live.length || open.items.some((x) => x.kind === 'tool' && !x.result));
  // A running compaction says what the agent does already.
  const compacting = turn.items.some((x) => x.kind === 'divider' && x.running);
  if (live && !streaming && !shown && !turn.retry && !compacting) out.push(html`<${ThinkingRow} key="thinking" policy=${policy} />`);
  return html`<section class="turn" data-turn=${turn.turn}>${out}</section>`;
}

/** Scrolls with new content while the reader is at the bottom; otherwise offers a button. */
export function Transcript({ tx, deltas, status, pending, cwd }) {
  const scroller = useRef(null);
  const pinned = useRef(true);
  const [away, setAway] = useState(false);
  const running = status === 'running' || status === 'waiting-approval';
  const lastIndex = tx.turns.length - 1;

  const lastTop = useRef(0);
  // Only the reader scrolling up unpins; growing content never does.
  const onScroll = () => {
    const el = scroller.current;
    if (!el) return;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
    if (atBottom) pinned.current = true;
    else if (el.scrollTop < lastTop.current - 2) pinned.current = false;
    lastTop.current = el.scrollTop;
    setAway(!pinned.current);
  };
  useLayoutEffect(() => {
    const el = scroller.current;
    if (el && pinned.current) el.scrollTop = el.scrollHeight;
  });
  // Images and code blocks grow after render; stay at the bottom while pinned.
  useEffect(() => {
    const el = scroller.current;
    const inner = el?.firstElementChild;
    if (!inner || typeof ResizeObserver === 'undefined') return undefined;
    const ro = new ResizeObserver(() => { if (pinned.current) el.scrollTop = el.scrollHeight; });
    ro.observe(inner);
    return () => ro.disconnect();
  }, []);
  const toBottom = () => {
    const el = scroller.current;
    if (!el) return;
    pinned.current = true;
    el.scrollTo({ top: el.scrollHeight, behavior: 'smooth' });
  };
  // Opening a row keeps it where it is instead of following the bottom.
  const onClickCapture = (e) => {
    if (e.target.closest?.('button[aria-expanded="false"]')) pinned.current = false;
  };

  // A prompt that is not in the log yet shows right away (its own turn if the agent is idle).
  const lastTurn = tx.turns[lastIndex];
  const liveTurn = running && lastTurn && lastTurn.end == null ? lastIndex : -1;
  const policy = useWorkPolicy();
  const acts = useChatActions(tx);
  // Long chats render their latest turns; older ones load on request.
  const win = useTurnWindow(tx.turns, scroller, pinned);
  const onFind = (query) => {
    const i = firstMatch(tx.turns, query);
    if (i >= 0) win.reveal(i);
  };
  return html`<div class="scroll-wrap"><div class="scroll" ref=${scroller} onScroll=${onScroll} onClickCapture=${onClickCapture}>
    <div class="column transcript">
      ${win.from > 0 ? html`<${LoadOlder} count=${win.from} onClick=${win.older} />` : null}
      ${tx.turns.slice(win.from).map((turn, k) => html`<${Turn} key=${turn.turn} turn=${turn} live=${win.from + k === liveTurn}
        deltas=${deltas} cwd=${cwd} policy=${policy} acts=${acts} />`)}
      ${pending ? html`<section class="turn"><${UserBubble} text=${pending.text} images=${pending.images} pending />
        ${liveTurn < 0 ? html`<${ThinkingRow} policy=${policy} />` : null}</section>` : null}
      ${running && liveTurn < 0 && !pending ? html`<${ThinkingRow} policy=${policy} />` : null}
    </div>
  </div>
  ${away ? html`<button type="button" class="to-bottom" aria-label=${t.scrollDown} onClick=${toBottom}><${IconArrowDown} size=${16} /></button>` : null}
  <${TurnRail} turns=${tx.turns} from=${win.from} scroller=${scroller} onJump=${win.jump} />
  <${FindBar} scroller=${scroller} pinned=${pinned} onQuery=${onFind} />
  </div>`;
}
