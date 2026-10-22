// Skills, MCP servers and connectors in fixture mode (?fixture=…), from
// ui/fixtures/extensions.json and shaped like the server's answers
// (docs/rust/SPEC.md §6). Changes live in memory until the page reloads.
// ?ext=empty shows the empty states, ?ext=many 200 skills and a server with
// 50 tools, ?ext=error the error states.
import { ApiError } from './api.js';

const variant = new URLSearchParams(location.search).get('ext') ?? '';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const clone = (v) => JSON.parse(JSON.stringify(v));

let data = null;
async function load() {
  if (!data) {
    data = (async () => {
      const r = await fetch('fixtures/extensions.json');
      if (!r.ok) throw new ApiError(r.status, 'extensions fixture not found');
      const d = await r.json();
      if (variant === 'empty') { d.skills = []; d.servers = []; }
      if (variant === 'many') grow(d);
      const now = Date.now();
      for (const s of d.servers) {
        const login = s.auth?.login;
        if (login?.expiresIn) { login.expiresAt = now + login.expiresIn; delete login.expiresIn; }
      }
      for (const lines of Object.values(d.logs)) for (const l of lines) { l.time = now - l.ago; delete l.ago; }
      return d;
    })();
  }
  const d = await data;
  await sleep(120);
  if (variant === 'error') throw new ApiError(500, 'The server could not read its configuration (fixture error).');
  return d;
}

const WORDS = ['alpha', 'brief', 'cache', 'deploy', 'email', 'figma', 'graph', 'helm', 'issue', 'jira', 'kpi', 'lint', 'metrics', 'notes'];
const SOURCES = ['project-agents', 'user-agents', 'plugin', 'user-dsh-codex'];

/** 200 skills and a server with 50 tools. */
function grow(d) {
  for (let i = d.skills.length; i < 200; i++) {
    const a = WORDS[i % WORDS.length];
    const b = WORDS[(i * 7) % WORDS.length];
    const name = `${a}-${b}-${i}`;
    const source = SOURCES[i % SOURCES.length];
    d.skills.push({
      name, title: `${a[0].toUpperCase()}${a.slice(1)} ${b} ${i}`, source, plugin: source === 'plugin' ? 'bulk@curated' : null,
      description: `Generated skill number ${i} for the many-skills fixture: ${a} meets ${b}, with a description long enough to wrap onto a second line and be clipped there.`,
      path: `/Users/alex/.agents/skills/${name}/SKILL.md`, active: true, modelInvocable: false, userInvocable: true, enabled: i % 9 !== 0, model: false,
    });
  }
  const tools = Array.from({ length: 50 }, (_, i) => ({
    name: `mcp__big_suite__op_${i}`, rawName: `op_${i}`, description: `Operation ${i}: does one thing well and explains it in a sentence.`,
    inputSchema: { type: 'object', properties: { id: { type: 'string' }, limit: { type: 'number' } }, required: ['id'] },
  }));
  d.servers.push({ ...clone(d.servers[0]), name: 'big-suite', command: 'big-suite-mcp', args: [], env: [], secrets: { env: [], headers: [], bearerToken: false },
    disabledTools: ['op_3', 'op_17'], tools });
}

const stripSchemas = (s) => ({ ...s, tools: (s.tools ?? []).map(({ inputSchema, ...t }) => t) });
const serverOf = (d, name) => {
  const s = d.servers.find((x) => x.name === name);
  if (!s) throw new ApiError(404, `not found: MCP server ${name}`);
  return s;
};

function skeleton(name, description) {
  const title = name.split('-').map((w) => w[0].toUpperCase() + w.slice(1)).join(' ');
  return `---\nname: ${name}\ndescription: ${JSON.stringify(description || 'Describe what this skill does and when to use it.')}\n---\n\n# ${title}\n\n## When to use\n\n- \n\n## Steps\n\n1. \n`;
}

/** Canned tools of a server added in fixture mode. */
const NEW_TOOLS = [
  ['search', 'Search the service.'], ['get_item', 'Fetch one item by id.'], ['create_item', 'Create an item.'],
  ['update_item', 'Change an item.'], ['delete_item', 'Delete an item.'],
];

function formFields(body) {
  const c = body.config ?? {};
  const secrets = body.secrets ?? {};
  const remote = !!c.url;
  return {
    transport: remote ? 'streamable-http' : 'stdio',
    url: c.url ?? null, command: remote ? null : c.command ?? null, args: remote ? [] : c.args ?? [],
    cwd: remote ? null : c.cwd ?? null, envVars: remote ? [] : c.env_vars ?? [],
    bearerTokenEnvVar: remote ? c.bearer_token_env_var ?? null : null,
    env: remote ? [] : Object.keys(secrets.env ?? {}).sort(),
    headers: remote ? Object.keys(secrets.headers ?? {}).sort() : [],
    oauthEnabled: c.oauth?.enabled ?? true,
  };
}

export const fixtureExt = {
  async skills() {
    const d = await load();
    return clone(d.skills);
  },

  async skill(name, path) {
    const d = await load();
    const row = d.skills.find((s) => s.name === name && (!path || s.path === path));
    if (!row) throw new ApiError(404, `not found: skill ${name}`);
    const doc = d.skillDocs[name] ?? {
      content: `---\nname: ${name}\ndescription: ${row.description}\n---\n\n# ${row.title}\n\n${row.description}\n\n## Steps\n\n1. Read the request.\n2. Do the work.\n3. Report what changed.\n`,
      files: [{ path: 'SKILL.md', size: 412 }],
    };
    const body = doc.content.replace(/^---\n[\s\S]*?\n---\n+/, '');
    const dir = row.path.replace(/\/[^/]+$/, '');
    return clone({ ...row, ...doc, body, dir, frontmatter: { name, description: row.description }, filesTruncated: false, contentTruncated: false });
  },

  async createSkill({ name, scope, description }) {
    const d = await load();
    if (!/^[a-z0-9]+(-[a-z0-9]+)*$/.test(name ?? '')) throw new ApiError(400, 'use lowercase letters, digits and single hyphens for the name, e.g. release-notes');
    if (d.skills.some((s) => s.name === name && !s.invalid)) throw new ApiError(400, `a skill named ${name} already exists`);
    const project = scope === 'project';
    const path = project ? `/Users/alex/code/storefront/.agents/skills/${name}/SKILL.md` : `/Users/alex/.agents/skills/${name}/SKILL.md`;
    const content = skeleton(name, description);
    d.skills.unshift({
      name, title: name.split('-').map((w) => w[0].toUpperCase() + w.slice(1)).join(' '), description: description || 'Describe what this skill does and when to use it.',
      source: project ? 'project-agents' : 'user-agents', plugin: null, path, active: true, modelInvocable: false, userInvocable: true, enabled: true, model: false,
    });
    d.skillDocs[name] = { content, files: [{ path: 'SKILL.md', size: content.length }] };
    return this.skill(name, path);
  },

  async skillSettings(name, patch) {
    const d = await load();
    const rows = d.skills.filter((s) => s.name === name);
    if (!rows.length) throw new ApiError(404, `not found: skill ${name}`);
    for (const row of rows) {
      if ('enabled' in patch) row.enabled = !!patch.enabled;
      if ('model' in patch) row.model = !!patch.model;
      row.modelInvocable = row.enabled && row.model;
    }
    return clone(rows[0]);
  },

  async servers() {
    const d = await load();
    return clone(d.servers.map(stripSchemas));
  },

  async server(name) {
    const d = await load();
    return clone(serverOf(d, name));
  },

  async addServer(body) {
    const d = await load();
    const connector = body.connector ? d.catalog.find((c) => c.id === body.connector) : null;
    const name = body.name || connector?.id;
    if (!/^[A-Za-z0-9_-]{1,32}$/.test(name ?? '')) throw new ApiError(400, `mcp server "${name}": the server name must match [A-Za-z0-9_-]{1,32}`);
    if (d.servers.some((s) => s.name === name)) throw new ApiError(400, `an MCP server named ${name} already exists`);
    const fields = connector ? formFields({ config: { url: connector.url } }) : formFields(body);
    const oauth = fields.transport !== 'stdio' && fields.oauthEnabled && !(body.secrets?.bearerToken);
    const row = {
      name, origin: 'config', plugin: null, originalName: null, connector: connector?.id ?? null, enabled: true, ...fields,
      secrets: { env: fields.env, headers: fields.headers, bearerToken: !!body.secrets?.bearerToken },
      always: false, disabledTools: [], auth: { oauth, login: null }, status: oauth ? 'needs-login' : 'connecting',
      error: null, retrying: false, warnings: [], tools: [],
    };
    d.servers.push(row);
    d.logs[name] = [{ time: Date.now(), kind: 'info', text: fields.url ? `connecting to ${fields.url}` : `starting ${[fields.command, ...fields.args].join(' ')}` }];
    if (!oauth) this.connectLater(row);
    return clone(stripSchemas(row));
  },

  connectLater(row) {
    setTimeout(() => {
      if (/fail/.test(row.command ?? row.url ?? '')) {
        Object.assign(row, { status: 'error', error: 'connection failed: server process exited (exit status: 1)' });
        data.then((d) => d.logs[row.name].push({ time: Date.now(), kind: 'error', text: row.error }));
        return;
      }
      row.status = 'connected';
      row.tools = NEW_TOOLS.map(([n, desc]) => ({ name: `mcp__${row.name}__${n}`, rawName: n, description: desc,
        inputSchema: { type: 'object', properties: { query: { type: 'string' } } } }));
      data.then((d) => d.logs[row.name].push({ time: Date.now(), kind: 'info', text: `connected, ${row.tools.length} tools` }));
    }, 1200);
  },

  async updateServer(name, body) {
    const d = await load();
    const row = serverOf(d, name);
    if (row.origin === 'plugin') throw new ApiError(400, `${name} belongs to the plugin ${row.plugin}; change it in the plugin`);
    const fields = formFields(body);
    const keep = (list, patch) => (patch ? Object.keys(patch).sort() : list);
    Object.assign(row, fields, {
      env: keep(row.env, body.secrets?.env), headers: keep(row.headers, body.secrets?.headers),
      status: 'connecting', error: null,
    });
    row.secrets = { env: row.env, headers: row.headers, bearerToken: body.secrets?.bearerToken === '' ? false : !!(body.secrets?.bearerToken || row.secrets?.bearerToken) };
    row.auth = { ...row.auth, oauth: row.transport !== 'stdio' && row.oauthEnabled && !row.secrets.bearerToken };
    d.logs[name] = [...(d.logs[name] ?? []), { time: Date.now(), kind: 'info', text: 'settings changed, restarting' }];
    this.connectLater(row);
    return clone(stripSchemas(row));
  },

  async testServer(body) {
    await load();
    await sleep(900);
    const c = body.config ?? {};
    const target = c.url ?? [c.command, ...(c.args ?? [])].join(' ');
    if (!c.url && !c.command) return { ok: false, tools: [], error: 'set command (stdio) or url (streamable-http)', needsLogin: false, stderr: [], elapsedMs: 3 };
    if (/fail/.test(target)) {
      return { ok: false, tools: [], error: 'Connection closed', needsLogin: false, elapsedMs: 840,
        stderr: ['npm ERR! code E404', `npm ERR! 404 Not Found - GET https://registry.npmjs.org/${c.args?.at(-1) ?? 'pkg'} - Not found`] };
    }
    if (c.url && (c.oauth?.enabled ?? true) && !body.secrets?.bearerToken) {
      return { ok: false, tools: [], error: 'authorization required', needsLogin: true, stderr: [], elapsedMs: 310 };
    }
    return { ok: true, tools: NEW_TOOLS.map(([name, description]) => ({ name, description })), needsLogin: false, stderr: [], elapsedMs: 870 };
  },

  async log(name) {
    const d = await load();
    serverOf(d, name);
    return { name, lines: clone(d.logs[name] ?? []) };
  },

  async action(name, action) {
    const d = await load();
    const row = serverOf(d, name);
    if (action === 'enable' || action === 'disable') {
      if (row.origin === 'plugin') throw new ApiError(400, `${name} belongs to the plugin ${row.plugin}; enable or disable the plugin instead`);
      row.enabled = action === 'enable';
      row.status = row.enabled ? 'connecting' : 'disabled';
      if (row.enabled) this.connectLater(row);
    } else if (action === 'restart') {
      row.status = 'connecting';
      d.logs[name] = [...(d.logs[name] ?? []), { time: Date.now(), kind: 'info', text: 'restarted' }];
      this.connectLater(row);
    } else if (action === 'login') {
      // No sign-in page in fixture mode: the login just succeeds.
      row.auth.login = { kind: 'oauth', issuer: row.url ? new URL(row.url).origin : null, scope: 'read', expiresAt: Date.now() + 3600_000, refreshable: true };
      row.status = 'connecting';
      this.connectLater(row);
      return { server: name, authorizationUrl: null };
    } else if (action === 'logout') {
      row.auth.login = null;
      row.status = 'needs-login';
      row.tools = [];
    }
    return clone(stripSchemas(row));
  },

  async removeServer(name) {
    const d = await load();
    const row = serverOf(d, name);
    if (row.origin === 'plugin') throw new ApiError(400, `${name} belongs to the plugin ${row.plugin}; disable the plugin instead`);
    d.servers = d.servers.filter((s) => s !== row);
    return null;
  },

  async token(name, token) {
    const d = await load();
    const row = serverOf(d, name);
    row.auth.login = token ? { kind: 'token', refreshable: false } : null;
    row.status = token ? 'connecting' : 'needs-login';
    if (token) this.connectLater(row);
    return clone(stripSchemas(row));
  },

  async serverSettings(name, patch) {
    const d = await load();
    const row = serverOf(d, name);
    if ('always' in patch) row.always = !!patch.always;
    if ('disabledTools' in patch) row.disabledTools = [...patch.disabledTools];
    return clone(stripSchemas(row));
  },

  async connectors() {
    const d = await load();
    return clone(d.catalog.map((c) => {
      const servers = d.servers.filter((s) => s.url === c.url).map((s) => s.name);
      const status = servers.length ? d.servers.find((s) => s.name === servers[0]).status : null;
      return { ...c, added: servers.length > 0, servers, status };
    }));
  },
};
