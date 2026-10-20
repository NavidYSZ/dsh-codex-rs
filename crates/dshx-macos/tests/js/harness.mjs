// Runs the original plugin (index.js) against a fake helper so the Rust port
// can be compared with it call by call.
//
//   HOME=<dir> node harness.mjs <repo root> <scenario.json>
//
// The fake helper listens on $HOME/Library/Application Support/dsh-codex/cua.sock.
// The scenario is { config, steps: [{ tool, args, images? }] }; the output is
// { tools, results: [{ ok, value, meta, content } | { ok: false, error, cancelled }], saved }.
import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

// index.js registers "app" only on macOS; decide that before it is loaded.
Object.defineProperty(process, 'platform', { value: 'darwin' });

const [repo, scenarioPath] = process.argv.slice(2);
const scenario = JSON.parse(fs.readFileSync(scenarioPath, 'utf8'));
const plugin = await import(pathToFileURL(path.join(repo, 'index.js')).href);

let images = true;
const saved = [];
// Same rule as the Rust test store: anything over 3 bytes comes back "downscaled".
const attachments = {
  async saveImage(img) {
    const bytes = img.data.length;
    saved.push({ mediaType: img.mediaType, name: img.name, bytes });
    const small = bytes > 3;
    return {
      attachmentId: `att-${saved.length}`, mediaType: img.mediaType, bytes,
      width: small ? 640 : 1280, height: small ? 400 : 800, name: img.name,
    };
  },
};
const llm = { async resolveModelInfo() { return { inputModalities: images ? ['text', 'image'] : ['text'] }; } };
const tools = new Map();
const ctx = {
  get: (key) => ({ attachments, llm })[key],
  inject: (_keys, fn) => fn(ctx),
  tools: { register: (def) => { tools.set(def.name, def); } },
  effect: () => {},
};
plugin.apply(ctx, scenario.config);

const results = [];
for (const step of scenario.steps) {
  images = step.images !== false;
  const def = tools.get(step.tool);
  const exec = {
    agent: { session: { requestHeader: () => ({ config: { provider: 'deepseek', model: 'test' } }) }, options: {} },
    signal: new AbortController().signal,
  };
  try {
    const value = await def.execute(step.args, exec);
    results.push({ ok: true, value, meta: def.output.presentationMeta(step.args, value), content: def.output.render(step.args, value) });
  } catch (err) {
    results.push({ ok: false, error: err.message, cancelled: !!err.cancelled });
  }
}
process.stdout.write(JSON.stringify({ tools: [...tools.keys()], results, saved }));
