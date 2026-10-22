// node ui/dev/pills_check.mjs: pills of a prompt skip inline code and code
// fences, like the server's @mention parser (ondemand.rs `mentions`).
import assert from 'node:assert/strict';
import { registerHooks } from 'node:module';

// The UI's import map, for node.
const vendor = new URL('../vendor/', import.meta.url);
const map = { preact: 'preact.mjs', 'preact/hooks': 'hooks.mjs' };
registerHooks({
  resolve: (spec, ctx, next) => (map[spec] ? { url: new URL(map[spec], vendor).href, shortCircuit: true } : next(spec, ctx)),
});
const { pills } = await import('../src/suggest.js');

const names = { skills: ['qa'], commands: [], servers: ['linear', 'sentry', 'github'], plugins: [] };
const text = 'run `npm i @linear/sdk` or `@github`\n```\n@sentry\n```\n``a `@qa` b`` /qa then @sentry';
assert.deepEqual(pills(text, names).map((p) => `${p.kind}:${p.name}`), ['skill:qa', 'server:sentry']);
assert.deepEqual(pills('@linear and /qa', names).map((p) => p.name), ['linear', 'qa']);
console.log('pills: code is skipped');
