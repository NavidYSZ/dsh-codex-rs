// Codex-like transcript rows for the dsh tools. One quiet 24 px line each
// (icon, verb, dot, detail); a click opens the body. Edits open by default.
import { useMemo, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t, clip } from '../i18n.js';
import { highlight } from '../highlight.js';
import { diffLines, countChanges } from '../diff.js';
import { CODEX_TOOLS, splitBashOutput } from '../reduce.js';
import { splitShellOutput } from '../persistent-shell.js';
import {
  IconTerminal, IconFile, IconPencil, IconSearch, IconList, IconGlobe, IconImage, IconTool, IconJob, IconChevronRight,
} from '../icons.js';
import { Shimmer, useImage } from './common.js';
import { CodexRow } from './codex.js';
import { openFile, openAutomations } from '../state.js';
import { describeSchedule } from './automations.js';

const resultText = (r) => (r?.content ?? []).filter((c) => c.type === 'text').map((c) => c.text).join('\n');
const errorReason = (r) => r?.error?.reason || resultText(r).replace(/^Error:\s*/, '') || t.failed;

/** Paths inside the workspace read better relative. */
export function relPath(path, cwd) {
  if (!path) return '';
  if (cwd && path.startsWith(cwd + '/')) return path.slice(cwd.length + 1);
  return path.replace(/^\.\//, '');
}

const extOf = (p) => (p.match(/\.([a-z0-9+]+)$/i)?.[1] ?? '').toLowerCase();

/** `path` makes the summary a link that opens the file in the side panel. */
function Row({ icon, title, summary, mono, running, error, badge, open, onToggle, children, state, path }) {
  const expandable = !!onToggle;
  const click = (e) => {
    if (path && e.target.closest('.row-path')) { e.stopPropagation(); openFile(path); return; }
    onToggle?.();
  };
  return html`<div class=${cx('tool', open && 'is-open')} data-state=${state ?? (running ? 'running' : error ? 'error' : 'done')}>
    <button type="button" class=${cx('row', (expandable || path) && 'is-expandable')} onClick=${click} aria-expanded=${expandable ? !!open : undefined}
      tabindex=${expandable || path ? 0 : -1}>
      <span class="lead">${icon}</span>
      <${Shimmer} active=${running}><span class="row-title">${title}</span></${Shimmer}>
      ${summary ? html`<span class="sep" /><span class=${cx('row-summary', mono && 'mono', error && 'is-error', path && !error && 'row-path')} title=${summary}>${summary}</span>` : null}
      ${badge}
      ${expandable ? html`<span class="chev"><${IconChevronRight} size=${12} /></span>` : null}
    </button>
    ${open && children ? html`<div class="tool-body">${children}</div>` : null}
  </div>`;
}

function useOpen(initial = false) {
  const [open, setOpen] = useState(initial);
  return [open, () => setOpen((v) => !v)];
}

function ApprovalBadge({ item }) {
  const a = item.approval;
  if (!a) return null;
  if (!a.outcome) return html`<span class="badge badge-warn">${t.waitingApproval}</span>`;
  if (a.outcome === 'rejected' || a.outcome === 'cancelled' || a.outcome === 'unavailable') return html`<span class="badge">${t.denied}</span>`;
  return null;
}

/** Long outputs show their first lines; the rest is one click away. */
function Clamp({ lines, max = 14, render }) {
  const [all, setAll] = useState(false);
  const shown = all || lines.length <= max + 3 ? lines : lines.slice(0, max);
  return html`${render(shown)}${lines.length > max + 3
    ? html`<button type="button" class="more" onClick=${() => setAll((v) => !v)}>${all ? t.showLess : t.showMore(lines.length - max)}</button>`
    : null}`;
}

// ---- bash ----------------------------------------------------------------

const waiting = (item) => !!item.approval && !item.approval.outcome;

function BashRow({ item }) {
  const [open, toggle] = useOpen(false);
  const { args, result } = item;
  const running = !result && !waiting(item);
  const text = resultText(result);
  const out = useMemo(() => splitShellOutput(text) ?? splitBashOutput(text), [text]);
  const failed = result?.isError;
  const exit = failed ? null : out.exit;
  const badge = html`<${ApprovalBadge} item=${item} />${exit ? html`<span class="badge badge-error">${t.exit(exit)}</span>` : null}${out.note && !failed
    ? html`<span class=${cx('badge', out.noteError && 'badge-error')}>${out.note}</span>` : null}`;
  return html`<${Row} icon=${html`<${IconTerminal} size=${14} />`} title=${running || waiting(item) ? t.running : t.ran}
      summary=${args.command ?? ''} mono running=${running} error=${failed} badge=${badge}
      open=${open} onToggle=${result ? toggle : undefined} state=${running ? 'running' : failed || exit || out.noteError ? 'error' : 'done'}>
    <div class="shell">
      <div class="shell-cmd"><span class="shell-prompt">$</span>${args.command ?? ''}</div>
      ${failed ? html`<pre class="shell-err">${errorReason(result)}</pre>` : null}
      ${!failed && out.stdout ? html`<${Clamp} lines=${out.stdout.split('\n')} render=${(l) => html`<pre class="shell-out">${l.join('\n')}</pre>`} />` : null}
      ${!failed && out.stderr ? html`<${Clamp} lines=${out.stderr.split('\n')} render=${(l) => html`<pre class="shell-out shell-stderr">${l.join('\n')}</pre>`} />` : null}
      ${!failed && result && !out.stdout && !out.stderr ? html`<div class="shell-none">${t.noOutput}</div>` : null}
      ${result && !failed ? html`<div class=${cx('shell-exit', (exit || out.noteError) && 'is-error')}>${[exit != null ? t.exit(exit) : null, out.note].filter(Boolean).join(' · ')}</div>` : null}
    </div>
  </${Row}>`;
}

// ---- read ----------------------------------------------------------------

function ReadRow({ item, cwd }) {
  const [open, toggle] = useOpen(false);
  const { args, result } = item;
  const meta = result?.meta;
  const running = !result;
  const failed = result?.isError;
  const path = relPath(meta?.path ?? args.file_path, cwd);
  const lines = Array.isArray(meta?.lines) ? meta.lines : null;
  const range = lines?.length ? t.lines(lines[0].number, lines[lines.length - 1].number) : '';
  const lang = meta?.lang ?? extOf(path);
  return html`<${Row} icon=${html`<${IconFile} size=${14} />`} title=${running ? t.reading : t.read}
      summary=${failed ? errorReason(result) : path} running=${running} error=${failed} path=${failed ? null : meta?.path ?? args.file_path}
      badge=${range && !failed ? html`<span class="row-meta">${range}</span>` : null}
      open=${open} onToggle=${lines ? toggle : undefined}>
    ${lines ? html`<div class="code-panel"><${Clamp} lines=${lines} max=${18} render=${(ls) => html`<table class="code-lines"><tbody>
      ${ls.map((l) => html`<tr><td class="ln">${l.number}</td><td class="lc" dangerouslySetInnerHTML=${{ __html: highlight(l.text, lang) || ' ' }} /></tr>`)}
    </tbody></table>`} /></div>` : null}
  </${Row}>`;
}

// ---- write / edit ----------------------------------------------------------

export function Diff({ hunks, lang }) {
  const rows = hunks.flatMap((h, i) => [...(i ? [{ op: 'gap' }] : []), ...h]);
  return html`<div class="diff"><${Clamp} lines=${rows} max=${24} render=${(rs) => html`<table class="diff-table"><tbody>
    ${rs.map((r) => (r.op === 'gap'
    ? html`<tr class="diff-gap"><td class="dm"></td><td class="dc">⋯</td></tr>`
    : html`<tr class=${r.op === '+' ? 'diff-add' : r.op === '-' ? 'diff-del' : 'diff-ctx'}>
        <td class="dm">${r.op === ' ' ? '' : r.op === '-' ? '−' : '+'}</td>
        <td class="dc" dangerouslySetInnerHTML=${{ __html: highlight(r.text, lang) || ' ' }} /></tr>`))}
  </tbody></table>`} /></div>`;
}

/** Diff hunks of an edit or write: the tool's own diffs, else the arguments. */
export function editHunks(item) {
  const { args, result, name } = item;
  const meta = result?.meta;
  if (meta?.diffs?.length) return meta.diffs.map((d) => diffLines(d.oldText, d.newText));
  if (name === 'write') return [diffLines(null, args.content ?? '')];
  if (name === 'edit' && (args.old_string || args.new_string)) return [diffLines(args.old_string || null, args.new_string ?? '')];
  return [];
}

/** Absolute or relative path an edit/write touched. */
export const editPath = (item) => item.result?.meta?.path ?? item.args.file_path ?? item.result?.meta?.diffs?.[0]?.path ?? '';

function EditRow({ item, cwd }) {
  const { args, result, name } = item;
  const running = !result;
  const failed = result?.isError;
  const meta = result?.meta;
  const path = relPath(editPath(item), cwd);
  const create = name === 'write' && (meta?.operation === 'create' || (!meta?.diffs?.length && running));
  const hunks = useMemo(() => editHunks(item), [result, item.argsRaw]);
  const { add, del } = countChanges(hunks.flat());
  const [open, toggle] = useOpen(!failed);
  const title = create ? (running ? t.creating : t.created) : running ? t.editing : t.edited;
  const counts = !failed && (add || del)
    ? html`<span class="row-meta counts">${add ? html`<span class="add">+${add}</span>` : null}${del ? html`<span class="del">−${del}</span>` : null}</span>`
    : null;
  return html`<${Row} icon=${html`<${IconPencil} size=${14} />`} title=${title}
      summary=${failed ? `${path} · ${errorReason(result)}` : path} running=${running} error=${failed}
      path=${failed || running ? null : editPath(item)}
      badge=${html`<${ApprovalBadge} item=${item} />${counts}`}
      open=${open} onToggle=${hunks.length && !failed ? toggle : undefined}>
    <${Diff} hunks=${hunks} lang=${extOf(path)} />
  </${Row}>`;
}

// ---- glob / grep -----------------------------------------------------------

function SearchRow({ item, cwd }) {
  const [open, toggle] = useOpen(false);
  const { args, result, name } = item;
  const running = !result;
  const failed = result?.isError;
  const meta = result?.meta;
  let count = '';
  let body = null;
  if (meta?.shape === 'paths') {
    count = meta.total ? t.files(meta.total) : t.noMatches;
    body = meta.paths?.length ? html`<div class="list-panel"><${Clamp} lines=${meta.paths} render=${(ps) => ps.map((p) => html`<div class="list-line mono">${relPath(p, cwd)}</div>`)} /></div>` : null;
  } else if (meta?.shape === 'matches') {
    count = meta.total ? t.matches(meta.total, meta.files?.length ?? 0) : t.noMatches;
    body = meta.files?.length ? html`<div class="list-panel">${meta.files.map((f) => html`<div class="grep-file">
      <div class="list-line mono grep-path">${relPath(f.path, cwd)}</div>
      ${(f.matches ?? []).slice(0, 20).map((m) => html`<div class="list-line mono grep-hit"><span class="ln">${m.lineNumber}</span>${m.line}</div>`)}
    </div>`)}</div>` : null;
  } else if (result && !failed) {
    const text = resultText(result);
    body = text ? html`<div class="list-panel"><pre class="plain">${text}</pre></div>` : null;
  }
  const what = name === 'glob' ? args.pattern : args.pattern;
  const where = args.path && args.path !== '.' ? ` in ${relPath(args.path, cwd)}` : '';
  return html`<${Row} icon=${html`<${IconSearch} size=${14} />`} title=${running ? t.searching : t.searched}
      summary=${failed ? errorReason(result) : `${what ?? ''}${where}${args.include ? ` (${args.include})` : ''}`} mono=${!failed}
      running=${running} error=${failed} badge=${count && !failed ? html`<span class="row-meta">${count}</span>` : null}
      open=${open} onToggle=${body ? toggle : undefined}>${body}</${Row}>`;
}

// ---- todo ------------------------------------------------------------------

function TodoCard({ item }) {
  const todos = item.todos ?? item.args.todos ?? [];
  const [open, toggle] = useOpen(!!item.latestTodo);
  const done = todos.filter((x) => x.status === 'completed').length;
  return html`<${Row} icon=${html`<${IconList} size=${14} />`} title=${t.updatedPlan} summary=${t.planDone(done, todos.length)}
      running=${!item.result} open=${open} onToggle=${todos.length ? toggle : undefined}>
    <ul class="todo">
      ${todos.map((x) => html`<li class=${cx('todo-item', `is-${x.status}`)}>
        <span class="todo-box">${x.status === 'completed'
    ? html`<svg width="14" height="14" viewBox="0 0 16 16"><circle cx="8" cy="8" r="7" fill="currentColor"/><path d="m4.9 8.2 2.1 2.1 4.1-4.3" fill="none" stroke="var(--bg)" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/></svg>`
    : x.status === 'in_progress'
      ? html`<svg width="14" height="14" viewBox="0 0 16 16"><circle cx="8" cy="8" r="6.4" fill="none" stroke="currentColor" stroke-width="1.3"/><circle cx="8" cy="8" r="3" fill="currentColor"/></svg>`
      : html`<svg width="14" height="14" viewBox="0 0 16 16"><circle cx="8" cy="8" r="6.4" fill="none" stroke="currentColor" stroke-width="1.3"/></svg>`}</span>
        <span class="todo-text">${x.content}</span>
      </li>`)}
    </ul>
  </${Row}>`;
}

// ---- web -------------------------------------------------------------------

function WebRow({ item }) {
  const [open, toggle] = useOpen(false);
  const { args, result, name } = item;
  const running = !result;
  const failed = result?.isError;
  const text = resultText(result);
  const fetch = name === 'web_fetch';
  const what = fetch
    ? String(args.url ?? '').replace(/^https?:\/\//, '').replace(/\/$/, '')
    : (Array.isArray(args.queries) ? args.queries : [args.query]).filter(Boolean).join(' · ');
  const title = fetch ? (running ? t.fetching : t.fetched) : running ? t.searchingWeb : t.searchedWeb;
  return html`<${Row} icon=${html`<${IconGlobe} size=${14} />`} title=${title}
      summary=${failed ? `${clip(what, 60)} · ${errorReason(result)}` : what} running=${running} error=${failed}
      open=${open} onToggle=${text && !failed ? toggle : undefined}>
    <div class="list-panel"><${Clamp} lines=${text.split('\n')} render=${(l) => html`<pre class="plain">${l.join('\n')}</pre>`} /></div>
  </${Row}>`;
}

function ImageRow({ item, cwd }) {
  const [open, toggle] = useOpen(true);
  const { args, result } = item;
  const image = result?.content?.find?.((c) => c.type === 'image')?.attachment;
  const url = useImage(image);
  return html`<${Row} icon=${html`<${IconImage} size=${14} />`} title=${t.viewedImage} summary=${relPath(args.file_path, cwd)}
      running=${!result} error=${result?.isError} open=${open} onToggle=${url ? toggle : undefined}>
    ${url ? html`<img class="tool-image" src=${url} alt="" />` : null}
  </${Row}>`;
}

// ---- anything else -----------------------------------------------------------

function GenericRow({ item }) {
  const [open, toggle] = useOpen(false);
  const { args, result, name } = item;
  const running = !result;
  const failed = result?.isError;
  const text = resultText(result);
  const detail = args.description ?? args.job_id ?? args.name ?? args.agent_id ?? '';
  const pretty = useMemo(() => {
    try { return JSON.stringify(args, null, 2); } catch { return item.argsRaw ?? ''; }
  }, [item.argsRaw]);
  const Icon = name.startsWith('job_') ? IconJob : IconTool;
  return html`<${Row} icon=${html`<${Icon} size=${14} />`} title=${name}
      summary=${failed ? errorReason(result) : String(detail)} running=${running} error=${failed}
      badge=${html`<${ApprovalBadge} item=${item} />`} open=${open} onToggle=${toggle}>
    <div class="list-panel">
      <pre class="plain dim">${pretty}</pre>
      ${text ? html`<${Clamp} lines=${text.split('\n')} render=${(l) => html`<pre class="plain">${l.join('\n')}</pre>`} />` : null}
    </div>
  </${Row}>`;
}

// ---- automations, goals, teams -------------------------------------------------

const jsonOf = (r) => { try { return JSON.parse(resultText(r)); } catch { return null; } };

/** The dsh automation tools: one readable line instead of the raw call. */
function AutomationRow({ item }) {
  const [open, toggle] = useOpen(false);
  const { args, result, name } = item;
  const running = !result;
  const out = jsonOf(result);
  // Validation problems come back as `{code, message}` results, not as errors.
  const problem = result?.isError ? errorReason(result) : out?.code && out?.message ? out.message : null;
  const view = out?.goal ?? out;
  let detail = '';
  if (name.startsWith('schedule_') && view?.kind) detail = `${view.title} · ${describeSchedule(view)}`;
  else if (name.endsWith('_goal')) detail = view?.objective ?? args.objective ?? '';
  else detail = args.name ?? args.target ?? args.subject ?? args.task_id ?? '';
  const Icon = name.startsWith('schedule_') ? IconJob : IconTool;
  const text = resultText(result);
  return html`<${Row} icon=${html`<${Icon} size=${14} />`} title=${t.toolLabels[name] ?? name}
      summary=${problem || detail} running=${running} error=${!!problem}
      badge=${name === 'schedule_create' && !problem && result ? html`<span class="badge badge-link" role="link" onClick=${(e) => { e.stopPropagation(); openAutomations(); }}>${t.automations}</span>` : null}
      open=${open} onToggle=${text ? toggle : undefined}>
    <div class="list-panel"><pre class="plain">${text}</pre></div>
  </${Row}>`;
}

const AUTOMATION = /^(schedule_|team_task_)|^(create_goal|update_goal|get_goal|spawn_teammate|list_agents|wait_agent|interrupt_agent)$/;

export function ToolItem({ item, cwd }) {
  const n = item.name;
  if (AUTOMATION.test(n)) return html`<${AutomationRow} item=${item} />`;
  if (CODEX_TOOLS.has(n)) return html`<${CodexRow} item=${item} />`;
  if (n === 'bash') return html`<${BashRow} item=${item} />`;
  if (n === 'read') return html`<${ReadRow} item=${item} cwd=${cwd} />`;
  if (n === 'edit' || n === 'write') return html`<${EditRow} item=${item} cwd=${cwd} />`;
  if (n === 'glob' || n === 'grep') return html`<${SearchRow} item=${item} cwd=${cwd} />`;
  if (n === 'todo_write') return html`<${TodoCard} item=${item} />`;
  if (n === 'web_fetch' || n === 'web_search') return html`<${WebRow} item=${item} />`;
  if (n === 'read_image') return html`<${ImageRow} item=${item} cwd=${cwd} />`;
  return html`<${GenericRow} item=${item} />`;
}

