// "Paste JSON" of the Add server dialog: MCP server definitions as other
// apps write them, turned into the server form's fields. Understands
// `{"mcpServers": {…}}` (Claude Desktop, Cursor, Windsurf), `{"servers": {…}}`
// and `{"mcp": {"servers": {…}}}` (VS Code), Codex's `[mcp_servers.x]` TOML,
// a bare server object, a `"name": {…}` fragment from a README, JSON with
// comments or trailing commas, a command line and a URL.

/** Server names as dsh-codex accepts them: [A-Za-z0-9_-]{1,32}. */
export const SERVER_NAME = /^[A-Za-z0-9_-]{1,32}$/;

export function serverName(raw) {
  const name = String(raw ?? '').trim().replace(/^@/, '').replace(/[^A-Za-z0-9_-]+/g, '-').replace(/^-+|-+$/g, '');
  return name.slice(0, 32) || 'server';
}

/** Splits a command line like a shell would for simple quoting. */
export function splitCommand(line) {
  return String(line ?? '').match(/"(?:[^"\\]|\\.)*"|'[^']*'|\S+/g)?.map((p) => {
    if (p.startsWith('"')) return p.slice(1, -1).replace(/\\(.)/g, '$1');
    if (p.startsWith("'")) return p.slice(1, -1);
    return p;
  }) ?? [];
}

/** Drops // and /* *\/ comments and trailing commas outside strings. */
function relaxJson(text) {
  let out = '';
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (c === '"') {
      let j = i + 1;
      while (j < text.length && text[j] !== '"') j += text[j] === '\\' ? 2 : 1;
      out += text.slice(i, j + 1);
      i = j;
    } else if (c === '/' && text[i + 1] === '/') {
      while (i < text.length && text[i] !== '\n') i++;
      out += '\n';
    } else if (c === '/' && text[i + 1] === '*') {
      const end = text.indexOf('*/', i + 2);
      i = end < 0 ? text.length : end + 1;
    } else {
      out += c;
    }
  }
  return out.replace(/,(\s*[}\]])/g, '$1');
}

function parseJson(text) {
  const attempts = [text, relaxJson(text)];
  // A `"name": {…}` fragment, or several of them.
  if (/^\s*"[^"]+"\s*:/.test(text)) attempts.push(`{${relaxJson(text)}}`);
  for (const a of attempts) {
    try { return JSON.parse(a); } catch { /* next */ }
  }
  return undefined;
}

// ---- Codex TOML (the subset its docs show) ------------------------------------

function tomlValue(raw) {
  const v = raw.trim();
  if (v.startsWith('"') || v.startsWith("'")) return v.startsWith('"') ? JSON.parse(v) : v.slice(1, -1);
  if (v.startsWith('[')) return splitTop(v.slice(1, -1)).map(tomlValue);
  if (v.startsWith('{')) {
    const obj = {};
    for (const part of splitTop(v.slice(1, -1))) {
      const eq = part.indexOf('=');
      if (eq > 0) obj[unquote(part.slice(0, eq))] = tomlValue(part.slice(eq + 1));
    }
    return obj;
  }
  if (v === 'true' || v === 'false') return v === 'true';
  return Number.isFinite(Number(v)) ? Number(v) : v;
}

const unquote = (k) => k.trim().replace(/^["']|["']$/g, '');

/** Splits a list body at top-level commas. */
function splitTop(body) {
  const parts = [];
  let depth = 0;
  let quote = null;
  let cur = '';
  for (const ch of body) {
    if (quote) { if (ch === quote) quote = null; cur += ch; continue; }
    if (ch === '"' || ch === "'") quote = ch;
    if (ch === '[' || ch === '{') depth++;
    if (ch === ']' || ch === '}') depth--;
    if (ch === ',' && depth === 0) { if (cur.trim()) parts.push(cur.trim()); cur = ''; continue; }
    cur += ch;
  }
  if (cur.trim()) parts.push(cur.trim());
  return parts;
}

function parseToml(text) {
  if (!/^\s*\[mcp_servers\./m.test(text)) return undefined;
  const servers = {};
  let target = null;
  for (const line of text.split('\n')) {
    const l = line.replace(/\s+#.*$/, '').trim();
    if (!l || l.startsWith('#')) continue;
    const head = /^\[mcp_servers\.("[^"]+"|[^.\]]+)(?:\.([A-Za-z_]+))?\]$/.exec(l);
    if (head) {
      const server = (servers[unquote(head[1])] ??= {});
      target = head[2] ? (server[head[2]] ??= {}) : server;
      continue;
    }
    if (l.startsWith('[')) { target = null; continue; }
    const eq = l.indexOf('=');
    if (target && eq > 0) {
      try { target[unquote(l.slice(0, eq))] = tomlValue(l.slice(eq + 1)); } catch { /* skip the line */ }
    }
  }
  return { mcpServers: servers };
}

// ---- normalising ------------------------------------------------------------------

const isServer = (v) => v && typeof v === 'object' && !Array.isArray(v)
  && (typeof v.command === 'string' || typeof v.url === 'string' || typeof v.serverUrl === 'string' || typeof v.type === 'string');

function serverMap(data) {
  if (!data || typeof data !== 'object') return null;
  for (const v of [data.mcpServers, data.servers, data.mcp?.servers, data.mcp_servers, data.context_servers]) {
    if (v && typeof v === 'object') return v;
  }
  if (isServer(data)) return { '': data };
  const entries = Object.entries(data).filter(([, v]) => isServer(v));
  return entries.length ? Object.fromEntries(entries) : null;
}

const strings = (obj) => Object.fromEntries(Object.entries(obj && typeof obj === 'object' ? obj : {})
  .filter(([, v]) => v != null && typeof v !== 'object').map(([k, v]) => [k, String(v)]));

/** `${FOO}`, `${env:FOO}` or `$FOO` naming the same variable: pass it through instead. */
const passthrough = (key, value) => {
  const m = /^\$(?:\{(?:env:)?([A-Za-z_][A-Za-z0-9_]*)\}|([A-Za-z_][A-Za-z0-9_]*))$/.exec(value.trim());
  return m && (m[1] ?? m[2]) === key;
};

/** One server entry → the form's fields plus `warnings`. */
export function toForm(name, raw) {
  const warnings = [];
  const type = String(raw.type ?? raw.transport ?? '').toLowerCase();
  const url = raw.url ?? raw.serverUrl ?? raw.httpUrl;
  const remote = typeof url === 'string' && type !== 'stdio';
  if (type === 'sse' || /\/sse\/?$/.test(url ?? '')) warnings.push('sse');
  const form = { name: serverName(name || raw.name || (remote ? hostName(url) : commandName(raw.command))), type: remote ? 'http' : 'stdio' };
  if (remote) {
    form.url = url;
    const headers = strings(raw.headers ?? raw.http_headers ?? raw.httpHeaders);
    const auth = Object.keys(headers).find((k) => k.toLowerCase() === 'authorization');
    const bearer = auth && /^Bearer\s+(.+)$/i.exec(headers[auth]);
    if (bearer && !/^\$\{/.test(bearer[1])) {
      form.auth = 'bearer';
      form.bearerToken = bearer[1].trim();
      delete headers[auth];
    } else if (raw.bearer_token_env_var ?? raw.bearerTokenEnvVar) {
      form.auth = 'bearer';
      form.bearerEnv = raw.bearer_token_env_var ?? raw.bearerTokenEnvVar;
    } else if (Object.keys(headers).length) {
      form.auth = 'headers';
    } else {
      form.auth = 'oauth';
    }
    form.headers = Object.entries(headers).map(([key, value]) => ({ key, value }));
  } else {
    let command = typeof raw.command === 'string' ? raw.command : '';
    let args = Array.isArray(raw.args) ? raw.args.map(String) : [];
    // "command": "npx -y pkg" in one string.
    if (!args.length && /\s/.test(command.trim())) [command, ...args] = splitCommand(command);
    form.command = command;
    form.args = args;
    const env = strings(raw.env);
    form.envVars = [...(Array.isArray(raw.env_vars) ? raw.env_vars.map(String) : [])];
    form.env = [];
    for (const [key, value] of Object.entries(env)) {
      if (passthrough(key, value)) { if (!form.envVars.includes(key)) form.envVars.push(key); }
      else form.env.push({ key, value });
    }
    if (typeof raw.cwd === 'string') form.cwd = raw.cwd;
    if (!command) warnings.push('command');
  }
  if (Object.values(raw.env ?? {}).some((v) => /\$\{input:/.test(String(v)))) warnings.push('input');
  return { form, warnings };
}

function hostName(url) {
  try { return new URL(url).hostname.split('.').filter((p) => !['www', 'mcp', 'api', 'com', 'io', 'dev', 'app'].includes(p))[0] || 'remote'; } catch { return 'remote'; }
}

function commandName(command) {
  const last = splitCommand(command).filter((p) => !p.startsWith('-')).pop() ?? '';
  return last.split('/').pop().replace(/^server-|-mcp$|^mcp-server-|-server$/g, '').replace(/@.*$/, '') || 'server';
}

/**
 * Pasted text → `{servers: [{form, warnings}], error?}`. `error` is
 * `empty` or `unreadable`.
 */
export function parseServers(text) {
  let body = String(text ?? '').trim();
  if (!body) return { servers: [], error: 'empty' };
  const fence = /```[a-z]*\n([\s\S]*?)```/i.exec(body);
  if (fence) body = fence[1].trim();
  if (/^https?:\/\/\S+$/i.test(body)) return { servers: [toForm('', { url: body })] };
  const data = parseJson(body) ?? parseToml(body);
  if (data === undefined) {
    // A command line ("npx -y @scope/server-x").
    if (!/[{}[\]]/.test(body) && body.split('\n').length === 1) return { servers: [toForm('', { command: body })] };
    return { servers: [], error: 'unreadable' };
  }
  const map = serverMap(data);
  if (!map) return { servers: [], error: 'unreadable' };
  const servers = Object.entries(map).filter(([, v]) => v && typeof v === 'object').map(([name, raw]) => toForm(name, raw));
  return servers.length ? { servers } : { servers: [], error: 'unreadable' };
}
