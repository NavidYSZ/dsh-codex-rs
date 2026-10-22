// Session events (dsh format v4 envelopes) → what the conversation shows.
//
// Only the events the UI needs are folded; everything else (request/header,
// inbox splices, delivery receipts, unknown plugin events) is ignored, so a
// log written by the official dsh renders too.
import { groupItems, turnWork } from './worklog.js';

export const CODEX_TOOLS = new Set(['computer', 'app', 'browser']);

/** The picker's model reference: bare DeepSeek ids, `provider/model` otherwise. */
export const modelRef = (provider, model) => (provider && provider !== 'deepseek-official' ? `${provider}/${model}` : model);

export function parseArgs(raw) {
  if (raw && typeof raw === 'object') return raw;
  if (typeof raw !== 'string' || !raw) return {};
  try { return JSON.parse(raw) ?? {}; } catch { return {}; }
}

/** Per content index: how long the model spent in that block (from the timed stream record). */
function blockTimes(stream) {
  const out = {};
  if (!Array.isArray(stream)) return out;
  for (const rec of stream) {
    if (rec?.type !== 'chunk') continue;
    const c = rec.chunk;
    if (c?.type === 'block-start') out[c.index] = { start: rec.time };
    else if (c?.type === 'block-end' && out[c.index]) out[c.index].end = rec.time;
  }
  const ms = {};
  for (const [i, v] of Object.entries(out)) if (v.end >= v.start) ms[i] = v.end - v.start;
  return ms;
}

function textOf(content) {
  return (content ?? []).filter((b) => b.type === 'text').map((b) => b.text).join('\n');
}

/** The line for a turn that a goal round, a reminder or a teammate started. */
function sourceNotice(kind, d, seq) {
  const text = textOf(d.content);
  const base = { kind: 'notice', key: `n:${seq}`, code: kind, tone: 'muted' };
  if (kind === 'goal') {
    const max = /Round: \d+\/(\d+)/.exec(text)?.[1];
    return { ...base, round: d.source.round, max };
  }
  if (kind === 'schedule') {
    // dsh frames reminders as JSON strings: one prompt or a batch.
    let prompt = '';
    try { prompt = JSON.parse(/reminder_prompt_json: (".*")/.exec(text)?.[1] ?? '""'); } catch { /* keep empty */ }
    if (!prompt) {
      try { prompt = JSON.parse(/reminders_json: (\[.*\])/s.exec(text)?.[1] ?? '[]').map((r) => r.reminder_prompt).join(' · '); } catch { /* keep empty */ }
    }
    return { ...base, prompt };
  }
  return { ...base, sender: d.source.senderName ?? '', text: text.replace(/^Team message \S+ from [^:]*:\s*/, '') };
}

/**
 * Folds the log into turns. Each turn holds ordered items:
 * user | reasoning | text | tool | notice | divider.
 */
export function buildTranscript(events) {
  const out = {
    turns: [],
    title: '',
    todos: null,
    model: null,
    effort: undefined,
    sandbox: null,
    contextWindow: 0,
    pendingApproval: null,
  };
  const calls = new Map();
  const approvals = new Map();
  const shots = {};
  let turn = null;
  let lastTodoCall = null;

  const openTurn = (n, time) => {
    turn = { turn: n, items: [], status: 'running', reason: null, start: time, end: null, retry: null };
    out.turns.push(turn);
    return turn;
  };
  const current = (n, time) => (turn && (n == null || turn.turn === n) ? turn : openTurn(n ?? out.turns.length + 1, time));

  const toolItem = (id, name, argsRaw, time) => {
    const item = {
      kind: 'tool', key: `c:${id}`, id, name, argsRaw, args: parseArgs(argsRaw),
      result: null, approval: null, todos: null, time,
      // Codex rows glide the cursor from where the previous step of the same tool left it.
      prevShot: CODEX_TOOLS.has(name) ? shots[name] ?? null : null,
    };
    calls.set(id, item);
    if (name === 'todo_write') lastTodoCall = item;
    return item;
  };

  for (const e of events) {
    const d = e.data ?? {};
    switch (e.type) {
      case 'turn/start':
        openTurn(d.turn, e.time);
        break;

      case 'user/message': {
        // Goal rounds, reminders and Team mail open turns of their own: a quiet line.
        const kind = d.source?.kind;
        if (kind === 'goal' || kind === 'schedule' || kind === 'team-message') {
          const t = current(null, e.time);
          t.items.push(sourceNotice(kind, d, e.seq));
          break;
        }
        // An MCP server the user switched on or off for this chat: a quiet line.
        if (kind === 'mcp-activation' || kind === 'mcp-deactivation') {
          current(null, e.time).items.push({ kind: 'notice', key: `m:${e.seq}`, code: kind, tone: 'muted', server: d.source.server ?? '' });
          break;
        }
        // Runtime context, AGENTS.md, skills etc. are model-only user messages.
        if (kind !== 'user') break;
        const t = current(null, e.time);
        t.items.push({
          kind: 'user', key: `u:${e.seq}`, text: textOf(d.content), time: e.time,
          images: (d.content ?? []).filter((b) => b.type === 'image' && b.attachment).map((b) => b.attachment),
        });
        break;
      }

      case 'assistant/message': {
        const t = current(d.turn, e.time);
        const ms = blockTimes(d.stream);
        const msg = d.message ?? {};
        (msg.content ?? []).forEach((b, i) => {
          if (b.type === 'reasoning') {
            if (b.text?.trim()) t.items.push({ kind: 'reasoning', key: `r:${e.seq}:${i}`, text: b.text, ms: ms[i] ?? null });
          } else if (b.type === 'text') {
            if (b.text?.trim()) t.items.push({ kind: 'text', key: `t:${e.seq}:${i}`, text: b.text });
          } else if (b.type === 'tool-call') {
            t.items.push(calls.get(b.id) ?? toolItem(b.id, b.name, b.arguments, e.time));
          }
        });
        if (d.interrupted) t.items.push({ kind: 'notice', key: `i:${e.seq}`, tone: 'muted', code: 'stopped' });
        if (msg.source?.model) out.model = modelRef(msg.source.provider, msg.source.model);
        t.retry = null;
        break;
      }

      case 'tool/call': {
        if (calls.has(d.callId)) break;
        current(d.turn, e.time).items.push(toolItem(d.callId, d.name, d.arguments, e.time));
        break;
      }

      case 'tool/result': {
        const m = d.message ?? {};
        const id = m.toolCallId ?? m.source?.callId;
        let item = calls.get(id);
        if (!item) {
          item = toolItem(id, d.meta?.tool ?? 'tool', '{}', e.time);
          current(d.turn, e.time).items.push(item);
        }
        item.result = { content: m.content ?? [], isError: !!m.isError, error: d.error ?? null, meta: d.meta ?? null, time: e.time };
        if (CODEX_TOOLS.has(item.name)) {
          const image = item.result.content.find((c) => c.type === 'image')?.attachment;
          const meta = item.result.meta ?? {};
          if (image && meta.screen) {
            shots[item.name] = {
              image, screen: meta.screen, place: meta.place ?? null,
              cursor: meta.cursor ?? (typeof item.args.x === 'number' ? { x: item.args.x, y: item.args.y } : null),
            };
          }
        }
        break;
      }

      case 'todo/write':
        out.todos = d.todos ?? [];
        if (lastTodoCall && !lastTodoCall.todos) lastTodoCall.todos = out.todos;
        break;

      case 'approval/asked': {
        const a = { id: d.id, toolName: d.toolName, callId: d.callId ?? null, reason: d.reason ?? '', outcome: null, time: e.time };
        approvals.set(d.id, a);
        const item = d.callId ? calls.get(d.callId) : null;
        if (item) item.approval = a;
        out.pendingApproval = a;
        break;
      }

      case 'approval/decided': {
        const a = approvals.get(d.id);
        if (a) a.outcome = d.outcome;
        if (out.pendingApproval?.id === d.id) out.pendingApproval = null;
        break;
      }

      case 'llm/retry':
        current(d.turn, e.time).retry = { retry: d.retry, max: d.maxRetries ?? null, delayMs: d.delayMs ?? 0, failure: d.failure ?? null, time: e.time };
        break;

      case 'compaction/end': {
        if (d.error) break;
        // A manual /compact runs between turns (`turn: null`): its line closes the last turn.
        const t = !turn && d.turn == null ? out.turns[out.turns.length - 1] : current(null, e.time);
        t?.items.push({ kind: 'divider', key: `k:${e.seq}`, code: 'compacted' });
        break;
      }

      case 'turn/end': {
        const t = current(d.turn, e.time);
        const kind = d.reason?.kind ?? 'completed';
        t.status = kind;
        t.reason = d.reason ?? null;
        t.end = e.time;
        t.retry = null;
        if (kind === 'error') t.items.push({ kind: 'notice', key: `x:${e.seq}`, tone: 'error', code: 'error', failure: d.reason.error ?? null });
        else if (kind === 'aborted') t.items.push({ kind: 'notice', key: `x:${e.seq}`, tone: 'muted', code: 'stopped' });
        else if (kind === 'max-tokens') t.items.push({ kind: 'notice', key: `x:${e.seq}`, tone: 'warn', code: 'maxTokens' });
        else if (kind === 'interrupted') t.items.push({ kind: 'notice', key: `x:${e.seq}`, tone: 'warn', code: 'interrupted' });
        // An approval still open when the turn ended will never be answered.
        if (out.pendingApproval) out.pendingApproval = null;
        turn = null;
        break;
      }

      case 'session/title':
        if (d.title) out.title = d.title;
        break;
      case 'model/selection':
        out.model = d.model ? modelRef(d.provider, d.model) : out.model;
        out.effort = d.reasoningEffort;
        break;
      case 'request/header': {
        const c = d.header?.config;
        if (c?.model) out.model = modelRef(c.provider, c.model);
        if (c && 'reasoningEffort' in c) out.effort = c.reasoningEffort;
        break;
      }
      case 'request/context':
        if (d.contextWindow) out.contextWindow = d.contextWindow;
        if (d.model) out.model = modelRef(d.provider, d.model);
        break;
      case 'sandbox/mode':
        out.sandbox = d.mode ?? out.sandbox;
        break;
      default:
        break;
    }
  }
  if (lastTodoCall) lastTodoCall.latestTodo = true;
  // Work groups and the fold of finished turns, once per event, not per render.
  for (const tn of out.turns) {
    Object.assign(tn, groupItems(tn.items, tn.end != null));
    tn.work = turnWork(tn.items);
  }
  return out;
}

/** The bash tool appends "[exit code: N]" when the command failed; no marker means 0. */
export function splitBashOutput(text) {
  let body = text ?? '';
  let exit = 0;
  const m = body.match(/\n?\[exit code: (-?\d+)\]\s*$/);
  if (m) {
    exit = Number(m[1]);
    body = body.slice(0, m.index);
  }
  // stdout comes first; "[stderr]" opens the stderr part.
  const at = body.startsWith('[stderr]\n') ? 0 : body.indexOf('\n[stderr]\n');
  if (at < 0) return { stdout: body.replace(/\n$/, ''), stderr: '', exit };
  const stdout = at === 0 ? '' : body.slice(0, at);
  const stderr = body.slice(at === 0 ? 9 : at + 10);
  return { stdout: stdout.replace(/\n$/, ''), stderr: stderr.replace(/\n$/, ''), exit };
}
