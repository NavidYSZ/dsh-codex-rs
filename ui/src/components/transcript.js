// The conversation: user bubbles, Markdown answers, reasoning, tool rows,
// turn notices and the live (streaming) tail of the running step.
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t, duration } from '../i18n.js';
import { renderMarkdown, onMarkdownClick } from '../markdown.js';
import { IconAlert, IconArrowDown, IconChevronRight, IconFile } from '../icons.js';
import { Shimmer, Thumb } from './common.js';
import { ToolItem } from './tools.js';
import { openSettings, openPanel, openFile } from '../state.js';
import { collectChanges, Counts } from './panel.js';

export function Markdown({ text, class: klass }) {
  const htmlText = useMemo(() => renderMarkdown(text), [text]);
  return html`<div class=${cx('md', klass)} onClick=${onMarkdownClick} dangerouslySetInnerHTML=${{ __html: htmlText }} />`;
}

function UserBubble({ text, images, pending }) {
  return html`<div class=${cx('user', pending && 'is-pending')}>
    ${images?.length ? html`<div class="user-images">${images.map((img) => html`<${Thumb} key=${img.attachmentId} image=${img} />`)}</div>` : null}
    ${text ? html`<div class="bubble">${text}</div>` : null}
  </div>`;
}

function Reasoning({ text, ms, live }) {
  const [open, setOpen] = useState(false);
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

function Notice({ item }) {
  if (item.code === 'error') {
    const f = item.failure ?? {};
    const auth = f.code === 'AUTH';
    return html`<div class="notice notice-error" role="alert">
      <span class="notice-icon"><${IconAlert} size=${16} /></span>
      <div class="notice-body">
        <div class="notice-title">${t.turnFailed}</div>
        <div class="notice-text">${f.message || ''}${f.code ? html` <span class="notice-code">${f.code}${f.status ? ` · ${f.status}` : ''}</span>` : null}</div>
        ${auth ? html`<button type="button" class="btn btn-small" onClick=${openSettings}>${t.openSettings}</button>` : null}
      </div>
    </div>`;
  }
  if (item.code === 'goal' || item.code === 'schedule' || item.code === 'team-message') {
    const [label, detail] = item.code === 'goal' ? [t.goal, t.goalRounds(item.round ?? '?', item.max ?? '∞')]
      : item.code === 'schedule' ? [t.automations, item.prompt] : [t.teamMessageFrom(item.sender), item.text];
    return html`<div class="source-line"><span class="source-label">${label}</span>${detail ? html`<span class="source-detail">${detail}</span>` : null}</div>`;
  }
  const text = item.code === 'maxTokens' ? t.maxTokens : item.code === 'interrupted' ? t.interrupted : t.stopped;
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

/** Work items between the prompt and the final answer can fold under "Worked for …". */
function Turn({ turn, live, deltas, cwd }) {
  const [folded, setFolded] = useState(false);
  const items = turn.items;
  const firstWork = items.findIndex((x) => x.kind !== 'user');
  let lastTool = -1;
  items.forEach((x, i) => { if (x.kind === 'tool') lastTool = i; });
  const done = !live && turn.end != null && turn.status === 'completed';
  const showWorked = done && lastTool >= 0 && turn.start != null;

  const render = (x) => {
    switch (x.kind) {
      case 'user': return html`<${UserBubble} key=${x.key} text=${x.text} images=${x.images} />`;
      case 'reasoning': return html`<${Reasoning} key=${x.key} text=${x.text} ms=${x.ms} />`;
      case 'text': return html`<${Markdown} key=${x.key} text=${x.text} />`;
      case 'tool': return html`<${ToolItem} key=${x.key} item=${x} cwd=${cwd} />`;
      case 'notice': return html`<${Notice} key=${x.key} item=${x} />`;
      case 'divider': return html`<div key=${x.key} class="divider"><span>${t.compacted}</span></div>`;
      default: return null;
    }
  };

  const out = [];
  items.forEach((x, i) => {
    if (folded && showWorked && i >= firstWork && i <= lastTool) return;
    out.push(render(x));
    if (showWorked && i === lastTool) {
      out.push(html`<button key="worked" type="button" class=${cx('worked', folded && 'is-folded')} onClick=${() => setFolded((v) => !v)}>
        <span class="worked-line" /><span class="worked-label">${t.workedFor(duration(turn.end - turn.start))}<${IconChevronRight} size=${11} /></span><span class="worked-line" />
      </button>`);
    }
  });

  // The running step streams here until its assistant/message event lands.
  let streaming = false;
  if (live && deltas && deltas.turn === turn.turn) {
    const blocks = Object.entries(deltas.blocks).filter(([, b]) => b.text).sort((a, b) => a[0] - b[0]);
    blocks.forEach(([index, b], k) => {
      streaming = true;
      const next = blocks[k + 1]?.[1];
      // Reasoning stays "Thinking" only until the next block starts streaming.
      out.push(b.type === 'reasoning'
        ? html`<${Reasoning} key=${`live-r-${deltas.key}-${index}`} text=${b.text} live=${!next} ms=${next ? next.start - b.start : null} />`
        : html`<${Markdown} key=${`live-t-${deltas.key}-${index}`} text=${b.text} class=${next ? '' : 'is-streaming'} />`);
    });
  }
  if (done) out.push(html`<${ChangesCard} key="changes" turn=${turn} cwd=${cwd} />`);
  if (live && turn.retry) out.push(html`<${Retry} key="retry" retry=${turn.retry} />`);
  const busyTool = items.some((x) => x.kind === 'tool' && !x.result);
  if (live && !streaming && !busyTool && !turn.retry) {
    out.push(html`<div key="thinking" class="row thinking"><${Shimmer} active><span class="row-title">${t.thinking}</span></${Shimmer}></div>`);
  }
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

  // A prompt that is not in the log yet shows right away (its own turn if the agent is idle).
  const lastTurn = tx.turns[lastIndex];
  const liveTurn = running && lastTurn && lastTurn.end == null ? lastIndex : -1;
  return html`<div class="scroll-wrap"><div class="scroll" ref=${scroller} onScroll=${onScroll}>
    <div class="column transcript">
      ${tx.turns.map((turn, i) => html`<${Turn} key=${turn.turn} turn=${turn} live=${i === liveTurn} deltas=${deltas} cwd=${cwd} />`)}
      ${pending ? html`<section class="turn"><${UserBubble} text=${pending.text} images=${pending.images} pending />
        ${liveTurn < 0 ? html`<div class="row thinking"><${Shimmer} active><span class="row-title">${t.thinking}</span></${Shimmer}></div>` : null}</section>` : null}
      ${running && liveTurn < 0 && !pending ? html`<div class="row thinking"><${Shimmer} active><span class="row-title">${t.thinking}</span></${Shimmer}></div>` : null}
    </div>
  </div>
  ${away ? html`<button type="button" class="to-bottom" aria-label=${t.scrollDown} onClick=${toBottom}><${IconArrowDown} size=${16} /></button>` : null}
  </div>`;
}
