// Work details (dsh `POLICIES`, `processActivity`, `processTitle`): how much
// of the agent's work the transcript shows, and the one status line that
// stands for a run of reasoning and tool steps between two answers.
import { t, de } from './i18n.js';

export const VIEW_MODES = ['compact', 'standard', 'detailed', 'verbose'];

/**
 * dsh's four views. `stepGrouping`: `collapsed` = one status line per group,
 * `history` = every step while the turn runs, folded afterwards, `none` = flat.
 */
export const POLICIES = {
  compact: { mode: 'compact', foldCompletedTurns: true, stepGrouping: 'collapsed', liveProcessDetail: false, settledReasoningPreview: false },
  standard: { mode: 'standard', foldCompletedTurns: true, stepGrouping: 'collapsed', liveProcessDetail: true, settledReasoningPreview: true },
  detailed: { mode: 'detailed', foldCompletedTurns: true, stepGrouping: 'history', liveProcessDetail: true, settledReasoningPreview: true },
  verbose: { mode: 'verbose', foldCompletedTurns: false, stepGrouping: 'none', liveProcessDetail: false, settledReasoningPreview: true },
};

export const policyOf = (mode) => POLICIES[mode] ?? POLICIES.standard;

const TEAM = new Set(['spawn_teammate', 'send_message', 'list_agents', 'wait_agent', 'interrupt_agent']);

/** dsh `activity`, plus the Mac tools, MCP servers (`mcp:<server>`) and skills. */
export function activityOf(name = '') {
  if (name === 'read') return 'read';
  if (name === 'read_image') return 'readImage';
  if (name === 'grep' || name === 'glob' || name.endsWith('_inspect')) return 'search';
  if (name === 'write') return 'write';
  if (name === 'edit' || name === 'apply_patch') return 'edit';
  if (['bash', 'pwsh', 'exec_command', 'write_stdin'].includes(name) || name.startsWith('terminal_') || name.startsWith('job_')) return 'commands';
  if (name === 'run_code') return 'code';
  if (name === 'web_search') return 'webSearch';
  if (name === 'web_fetch') return 'webFetch';
  if (name === 'subagent' || name.startsWith('subagent_') || name.startsWith('team_task_') || TEAM.has(name)) return 'subagents';
  if (['todo_write', 'create_goal', 'update_goal', 'get_goal'].includes(name)) return 'plan';
  if (['ask_user', 'ask_user_question', 'request_user_input'].includes(name)) return 'questions';
  if (name === 'browser') return 'browser';
  if (name === 'computer' || name === 'app') return 'mac';
  if (name === 'skill') return 'skill';
  const mcp = /^mcp__(.+?)__/.exec(name);
  if (mcp) return `mcp:${mcp[1]}`;
  return 'tools';
}

/** The localized phrase of an activity from one of t.workLive / workPrepare / workDone. */
export function activityLabel(table, kind) {
  if (kind.startsWith('mcp:')) return table.mcp(kind.slice(4));
  return table[kind] ?? table.tools;
}

/** dsh `processTitle`: the top three kinds of a finished group, without counts. */
export function processTitle(counts) {
  const labels = counts.slice(0, 3).map(({ kind }) => activityLabel(t.workDone, kind));
  if (!labels.length) return t.workDone.thinking;
  // English continues in lower case ("Read files and ran commands"); German nouns keep theirs.
  const next = (s) => (de ? s : s.charAt(0).toLowerCase() + s.slice(1));
  if (labels.length === 1) return labels[0];
  if (labels.length === 2) return t.workJoin(labels[0], next(labels[1]));
  const title = [labels[0], ...labels.slice(1).map(next)].join(', ');
  return counts.length > 3 ? t.workMore(title) : title;
}

/** Distinct tool calls per kind, most frequent first (ties by first appearance). */
export function countKinds(items) {
  const counts = new Map();
  for (const x of items) {
    if (x.kind !== 'tool') continue;
    const kind = activityOf(x.name);
    counts.set(kind, (counts.get(kind) ?? 0) + 1);
  }
  return [...counts].map(([kind, count]) => ({ kind, count })).sort((a, b) => b.count - a.count);
}

const DETAIL_KEYS = ['title', 'description', 'objective', 'task', 'task_name', 'name', 'question', 'questions', 'prompt', 'message',
  'command', 'cmd', 'queries', 'query', 'pattern', 'url', 'uri', 'file_path', 'path', 'target', 'action', 'status'];
const DETAIL_MAX = 160;

function normalize(value) {
  const s = (typeof value === 'string' ? value : Array.isArray(value) && value.every((v) => typeof v === 'string') ? value.join(', ') : '')
    .replace(/\s+/g, ' ').trim();
  const chars = Array.from(s);
  return chars.length <= DETAIL_MAX ? s : `${chars.slice(0, DETAIL_MAX - 1).join('').trimEnd()}…`;
}

/** dsh `liveToolDetail`: what a running call works on (paths relative to the chat's folder). */
export function liveToolDetail(item, cwd) {
  const args = item.args ?? {};
  for (const key of DETAIL_KEYS) {
    if (!(key in args)) continue;
    let value = args[key];
    if (key === 'questions' && Array.isArray(value)) value = value.find((q) => q?.question)?.question;
    if ((key === 'file_path' || key === 'path') && typeof value === 'string' && cwd && value.startsWith(`${cwd}/`)) value = value.slice(cwd.length + 1);
    const detail = normalize(value);
    if (detail) return detail;
  }
  // Only an unknown tool names itself; the others have their activity label.
  return activityOf(item.name) === 'tools' ? normalize(item.name) : '';
}

export const stripBold = (s) => s.replaceAll('**', '');

export function firstLine(text) {
  const i = text.indexOf('\n');
  return i < 0 ? text : text.slice(0, i);
}

/** dsh: the streaming preview advances when a paragraph's first line is complete. */
export function latestCompletedParagraphFirstLine(text) {
  let summary = '';
  let start = 0;
  const separator = /\r?\n(?:[\t ]*\r?\n)+/g;
  for (;;) {
    const next = separator.exec(text);
    const end = next === null ? text.length : next.index + next[0].indexOf('\n');
    const newline = text.indexOf('\n', start);
    if (newline !== -1 && newline <= end) {
      const candidate = text.slice(start, newline).trim();
      if (candidate) summary = candidate;
    }
    if (next === null) return summary;
    start = next.index + next[0].length;
  }
}

/** dsh `liveReasoningDetail`: the latest non-empty paragraph of streaming reasoning. */
export function reasoningDetail(text) {
  const paragraphs = text.split(/\r?\n[\t ]*\r?\n/);
  for (let i = paragraphs.length - 1; i >= 0; i--) {
    const detail = normalize(stripBold(paragraphs[i] ?? ''));
    if (detail) return detail;
  }
  return '';
}

/**
 * Splits a turn's items into segments: runs of reasoning and tool items
 * become one group (`g:<n>`, numbered within the turn so a live group keeps
 * its key once its step lands); everything else stands alone. A group is
 * closed once something follows it or the turn ended.
 */
export function groupItems(items, ended) {
  const segments = [];
  let pending = [];
  let groups = 0;
  const flush = (closed) => {
    if (!pending.length) return;
    segments.push({ kind: 'group', key: `g:${groups++}`, items: pending, closed, counts: countKinds(pending) });
    pending = [];
  };
  for (const x of items) {
    if (x.kind === 'reasoning' || x.kind === 'tool') pending.push(x);
    else {
      flush(true);
      segments.push({ kind: 'item', key: x.key, item: x });
    }
  }
  flush(ended);
  return { segments, groups };
}

/**
 * What a finished turn folds into: the final answer stays, the rest hides
 * behind "Completed in … · N tool calls · N messages" (dsh `turnProcess`).
 */
export function turnWork(items) {
  let answer = -1;
  items.forEach((x, i) => { if (x.kind === 'text') answer = i; });
  let toolCalls = 0;
  let messages = 0;
  let users = 0;
  let hidden = false;
  items.forEach((x, i) => {
    if (x.kind === 'tool') toolCalls++;
    if (x.kind === 'text' && i < answer) messages++;
    if (x.kind === 'user') users++;
    if (i !== answer && (x.kind === 'reasoning' || x.kind === 'tool' || x.kind === 'text' || x.kind === 'divider')) hidden = true;
  });
  return { answerKey: answer >= 0 ? items[answer].key : null, toolCalls, messages, steered: users > 1, hidden };
}

/** Items that stay visible in a folded turn besides its answer. */
export function staysVisible(item) {
  if (item.kind === 'user') return true;
  if (item.kind !== 'notice') return false;
  return item.code !== 'stopped' && item.code !== 'interrupted';
}
