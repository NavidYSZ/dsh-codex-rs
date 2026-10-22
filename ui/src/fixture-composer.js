// The composer routes in fixture mode (?fixture=…): `/` and `@` menus over
// a made-up catalog, the chat's MCP servers, /compact, the queue and file
// uploads, shaped like the answers of docs/rust/SPEC.md §6. A fixture may
// bring its own `composer: {skills, servers, plugins, files, active}`.
import { rank } from './suggest.js';

const CATALOG = {
  skills: [
    { name: 'visual-brand-foundation', description: 'Develop a coherent visual brand identity and an applied brand or CI sheet.', source: 'user-agents' },
    { name: 'skill-creator', description: 'Create new skills, modify and improve existing skills, and measure skill performance.', source: 'plugin', plugin: 'anthropic-skills@claude' },
    { name: 'release-notes', description: 'Write release notes from the merged pull requests since the last tag.', source: 'project-agents' },
    { name: 'pdf', description: 'Read, merge, split and fill PDF files.', source: 'plugin', plugin: 'anthropic-skills@claude' },
    { name: 'docx', description: 'Create and edit Word documents with tracked changes.', source: 'plugin', plugin: 'anthropic-skills@claude' },
    { name: 'test-plan', description: 'Turn a feature description into a reviewable test plan.', source: 'user-dsh-codex' },
  ],
  servers: [
    { name: 'linear', title: 'Linear', description: 'Issues, projects and cycles.', status: 'connected', tools: 12, connector: 'linear', brandColor: '#5E6AD2' },
    { name: 'github', title: 'GitHub', description: 'Repositories, issues and pull requests.', status: 'needs-login', tools: 0, connector: 'github', brandColor: '#24292F' },
    { name: 'allintitle-gsc', title: 'allintitle-gsc', description: null, status: 'connected', tools: 11 },
    { name: 'mailindex', title: 'mailindex', description: null, status: 'error', tools: 0 },
  ],
  plugins: [
    { name: 'figma', id: 'figma@openai-curated', title: 'Figma', description: 'Design files, components and comments.', servers: ['figma_desktop', 'figma_remote'], skills: 3, brandColor: '#A259FF' },
  ],
  files: [
    'README.md', 'pyproject.toml', 'tally.py', 'tests/test_tally.py', 'tests/fixtures/words.txt',
    'docs/usage.md', 'docs/design notes/top-flag.md', 'src/tally/__init__.py', 'src/tally/cli.py', 'src/tally/count.py',
  ],
};
const COMMANDS = [
  ['compact', 'Compact', 'Compact older conversation history', 'chat'],
  ['goal', 'Goal', 'Set or view the goal for a long-running task', 'chat'],
  ['model', 'Model', 'Switch the model and reasoning effort'],
  ['permission', 'Permission', 'Switch the access mode of this chat'],
  ['status', 'Status', 'Show context, cache and cost of this chat', 'chat'],
  ['view', 'View', "Choose how much of the agent's work the chat shows"],
  ['export', 'Export', "Download this chat's session log (JSONL)", 'chat'],
];

const titleOf = (name) => name.split(/[-_]/).filter(Boolean).map((w) => w[0].toUpperCase() + w.slice(1)).join(' ');
const best = (q, ...texts) => texts.reduce((m, x) => Math.max(m, rank(q, x) ?? -1), -1);
const sortItems = (items) => items.sort((a, b) => b[0] - a[0] || String(a[1].name).localeCompare(String(b[1].name))).map((x) => x[1]);

function mention(path, dir) {
  const p = dir ? `${path}/` : path;
  if (!/\s/.test(p)) return `@${p}`;
  return dir ? `@"${p}` : `@"${p}"`;
}

/** Every file and folder of the catalog's tree. */
function tree(files) {
  const out = new Map();
  for (const f of files) {
    const parts = f.split('/');
    for (let i = 1; i < parts.length; i++) out.set(parts.slice(0, i).join('/'), true);
    out.set(f, false);
  }
  return [...out].map(([path, dir]) => ({ path, dir }));
}

function fileItems(files, query) {
  const entries = tree(files);
  const q = query.replace(/^"/, '');
  const item = (e) => {
    const at = e.path.lastIndexOf('/');
    return { type: e.dir ? 'folder' : 'file', name: e.path.slice(at + 1), path: e.path, dir: at < 0 ? '' : e.path.slice(0, at), insert: mention(e.path, e.dir) };
  };
  if (q.includes('/')) {
    const dir = q.slice(0, q.lastIndexOf('/'));
    const name = q.slice(q.lastIndexOf('/') + 1);
    return sortItems(entries.filter((e) => e.path.startsWith(`${dir}/`) && !e.path.slice(dir.length + 1).includes('/'))
      .map((e) => [rank(name, e.path.slice(dir.length + 1)), e]).filter(([s]) => s != null)
      .map(([s, e]) => [s + (e.dir ? 1 : 0), item(e)])).slice(0, 20);
  }
  return sortItems(entries.filter((e) => q || !e.path.includes('/'))
    .map((e) => [Math.max(rank(q, e.path.split('/').pop()) ?? -1, (rank(q, e.path) ?? -1) - 500), e])
    .filter(([s]) => s >= 0).map(([s, e]) => [s + (e.dir ? 1 : 0) - e.path.split('/').length, item(e)])).slice(0, 20);
}

function catalog(fx) {
  return { ...CATALOG, ...(fx.fx?.composer ?? {}), ...(fx.shared?.composer ?? {}) };
}

export const fixtureComposer = {
  async suggest(fx, { kind, q = '', session, model }) {
    await fx.ready;
    const c = catalog(fx);
    const active = fx.mcpActive ?? (fx.mcpActive = [...(c.active ?? [])]);
    const sections = [];
    if (kind === 'slash') {
      const commands = sortItems(COMMANDS.filter(([, , , needs]) => !needs || session)
        .map(([name, title, description]) => [best(q, name, title), { type: 'command', name, title, description, insert: `/${name}` }])
        .filter(([s]) => s >= 0));
      if (commands.length) sections.push({ id: 'commands', items: commands });
      sections.push({ id: 'skills', items: sortItems(c.skills.map((s) => {
        const title = titleOf(s.name);
        return [best(q, s.name, title), { type: 'skill', ...s, title, model: false, insert: `/${s.name} ` }];
      }).filter(([s]) => s >= 0)) });
    } else {
      const apps = c.servers.map((s) => [best(q, s.name, s.title) * 1.0001, {
        type: 'server', ...s, needsLogin: s.status === 'needs-login', active: active.includes(s.name), always: false, insert: `@${s.name} `,
      }]);
      for (const p of c.plugins) apps.push([best(q, p.name, p.title), { type: 'plugin', ...p, active: p.servers.every((x) => active.includes(x)), insert: `@${p.name} ` }]);
      sections.push({ id: 'apps', items: sortItems(apps.filter(([s]) => s >= 0)) });
      sections.push({ id: 'files', items: fileItems(fx.fx?.files?.map((f) => f.path) ?? c.files, q) });
    }
    const pro = (model ?? fx.infoData.settings.model) === 'deepseek-v4-pro' || String(model ?? '').includes('/');
    return { kind, query: q, sections, cacheMiss: pro };
  },

  async sessionMcp(fx) {
    await fx.ready;
    const c = catalog(fx);
    return { active: fx.mcpActive ?? (fx.mcpActive = [...(c.active ?? [])]) };
  },

  async setSessionMcp(fx, _id, server, on) {
    const { active } = await this.sessionMcp(fx);
    fx.mcpActive = on ? [...new Set([...active, server])] : active.filter((s) => s !== server);
    return { active: fx.mcpActive };
  },

  /** A prompt's `@server` and `@plugin` mentions switch them on, like the server. */
  onPrompt(fx, text) {
    const c = catalog(fx);
    fx.mcpActive ??= [...(c.active ?? [])];
    for (const m of String(text).matchAll(/(?:^|[^\p{L}\p{N}_])@([\w-]+)/gu)) {
      const server = c.servers.find((s) => s.name.toLowerCase() === m[1].toLowerCase());
      const plugin = c.plugins.find((p) => p.name.toLowerCase() === m[1].toLowerCase());
      for (const name of server ? [server.name] : plugin?.servers ?? []) {
        if (!fx.mcpActive.includes(name)) fx.mcpActive.push(name);
      }
    }
  },

  async compact(fx) {
    await fx.ready;
    const id = `compaction-${Date.now().toString(36)}`;
    fx.push({ type: 'compaction/start', data: { compactionId: id, turn: null } });
    fx.push({ type: 'compaction/end', data: { compactionId: id, turn: null } });
    return { compacted: true, items: 14, tokens: 52300 };
  },

  /** Edits, removes or steers a queued prompt through an inbox splice, like the agent. */
  async updateQueue(fx, _id, item, body) {
    await fx.ready;
    const lists = { 'next-turn': [] };
    for (const e of fx.history) {
      if (e.type !== 'agent/inbox/spliced' || e.data?.target !== 'next-turn') continue;
      lists['next-turn'].splice(e.data.start ?? 0, e.data.removedCount ?? 0, ...(e.data.inserted ?? []));
    }
    const at = lists['next-turn'].findIndex((m) => m.id === item);
    if (at < 0) throw Object.assign(new Error('queued item is no longer pending'), { status: 400 });
    const message = lists['next-turn'][at];
    const edited = body.action === 'edit' ? [{ ...message, content: [{ type: 'text', text: body.text }] }] : [];
    fx.push({ type: 'agent/inbox/spliced', data: { target: 'next-turn', start: at, removedCount: 1, inserted: edited, ...(body.action === 'remove' ? { outcome: 'canceled' } : {}) } });
    return null;
  },

  async uploadFile(_fx, file) {
    const dir = Math.random().toString(16).slice(2, 18);
    return { path: `/Users/alex/.dsh-codex/attachments/files/${dir}/${file.name}`, name: file.name, bytes: file.size, sha256: dir };
  },

  async exportLog(fx) {
    await fx.ready;
    const lines = [fx.sessionData?.header, ...fx.history].filter(Boolean).map((x) => JSON.stringify(x));
    return new Blob([`${lines.join('\n')}\n`], { type: 'application/x-ndjson' });
  },
};
