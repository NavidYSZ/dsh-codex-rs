// Runs the web UI's reduce.js on ui/fixtures and writes what it makes of them into AetherTests/Fixtures:
// the Swift reducer (Core/Reduce) must produce the same items in the same order.  node ios/Tools/gen-expected.mjs
import { readFileSync, writeFileSync } from 'node:fs';
globalThis.location = { search: '', hash: '', pathname: '/', protocol: 'http:', host: 'x' };
globalThis.document = { documentElement: {} , addEventListener(){}, visibilityState: 'visible' };
globalThis.window = globalThis;
globalThis.history = { replaceState(){}, pushState(){} };
globalThis.localStorage = { getItem(){ return null; }, setItem(){}, removeItem(){} };
globalThis.sessionStorage = globalThis.localStorage;
globalThis.addEventListener = () => {};
globalThis.matchMedia = () => ({ matches: false, addEventListener(){} });
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, '..', '..');
const outDir = join(here, '..', 'AetherTests', 'Fixtures');
const { buildTranscript } = await import(root + '/ui/src/reduce.js');
const short = (s) => (typeof s === 'string' ? s : '');
function dump(tx) {
  return {
    title: tx.title, model: tx.model, effort: tx.effort === undefined ? null : tx.effort, sandbox: tx.sandbox,
    contextWindow: tx.contextWindow, todos: tx.todos, todosTurn: tx.todosTurn,
    pendingApproval: tx.pendingApproval ? { id: tx.pendingApproval.id, toolName: tx.pendingApproval.toolName, callId: tx.pendingApproval.callId, reason: tx.pendingApproval.reason } : null,
    turns: tx.turns.map((t) => ({
      turn: t.turn, status: t.status, start: t.start, end: t.end, retry: t.retry ? { retry: t.retry.retry, max: t.retry.max, delayMs: t.retry.delayMs } : null,
      todos: t.todos ?? null,
      work: t.work,
      segments: t.segments.map((s) => (s.kind === 'group' ? { kind: 'group', key: s.key, closed: s.closed, items: s.items.map((x) => x.key), counts: s.counts } : { kind: 'item', key: s.key })),
      items: t.items.map((x) => {
        const o = { kind: x.kind, key: x.key };
        if (x.kind === 'user') { o.text = x.text; o.images = x.images.length; o.time = x.time; }
        if (x.kind === 'text' || x.kind === 'reasoning') { o.text = x.text; if (x.kind === 'reasoning') o.ms = x.ms; }
        if (x.kind === 'tool') {
          o.id = x.id; o.name = x.name; o.argsRaw = short(x.argsRaw) || (typeof x.argsRaw === 'object' ? JSON.stringify(x.argsRaw) : ''); o.args = x.args; o.time = x.time;
          o.hasResult = !!x.result; o.isError = x.result ? x.result.isError : null; o.resultText = x.result ? x.result.content.filter((c) => c.type === 'text').map((c) => c.text).join('\n') : null;
          o.meta = x.result?.meta ?? null; o.resultTime = x.result?.time ?? null;
          o.approval = x.approval ? { id: x.approval.id, outcome: x.approval.outcome, reason: x.approval.reason } : null;
          o.todos = x.todos ?? null; o.latestTodo = !!x.latestTodo; o.prevShot = !!x.prevShot;
        }
        if (x.kind === 'notice') { o.code = x.code; o.tone = x.tone; if (x.failure) o.failure = x.failure; if (x.text != null) o.text = x.text; }
        if (x.kind === 'divider') { o.code = x.code; o.running = !!x.running; o.summary = x.summary; o.items = x.items; o.tokens = x.tokens; o.error = x.error; }
        return o;
      }),
    })),
  };
}
const load = (n) => JSON.parse(readFileSync(`${root}/ui/fixtures/${n}.json`, 'utf8'));
const coding = load('coding');
writeFileSync(outDir + '/coding.expected.json', JSON.stringify(dump(buildTranscript(coding.session.events)), null, 1));
const approval = load('approval');
const base = approval.session.events;
writeFileSync(outDir + '/approval.expected.json', JSON.stringify(dump(buildTranscript(base)), null, 1));
for (const decision of ['allowed-once', 'rejected']) {
  let seq = base[base.length - 1].seq; let time = base[base.length - 1].time;
  const tail = approval.onApproval[decision].map((e) => ({ ...e, seq: ++seq, time: (time += 1500) }));
  writeFileSync(`${outDir}/approval-${decision}.expected.json`, JSON.stringify(dump(buildTranscript([...base, ...tail])), null, 1));
  writeFileSync(`${outDir}/approval-${decision}.events.json`, JSON.stringify([...base, ...tail], null, 1));
}
// Other fixtures too, for the broad test.
for (const n of ['compacting', 'error', 'failures', 'messages', 'browser', 'computer', 'minimal', 'markdown']) {
  const fx = load(n);
  if (!fx.session) continue;
  writeFileSync(`${outDir}/${n}.expected.json`, JSON.stringify(dump(buildTranscript(fx.session.events)), null, 1));
}
console.log('ok');
