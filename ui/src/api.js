// UI ↔ server protocol (docs/rust/SPEC.md §6) and the offline fixture mode.
//
// Both implementations expose the same methods, so the rest of the UI never
// knows whether it talks to dshx-server or replays ui/fixtures/<name>.json.
import { t } from './i18n.js';
import { fixtureFs } from './fixture-fs.js';
import { fixtureExt } from './fixture-ext.js';
import { fixtureComposer } from './fixture-composer.js';

export class ApiError extends Error {
  constructor(status, message) {
    super(message || `HTTP ${status}`);
    this.status = status;
  }
}

/** Picks the implementation from the page URL: ?fixture=<name> or the live server. */
export function createApi() {
  const params = new URLSearchParams(location.search);
  const fixture = params.get('fixture');
  if (fixture) {
    const speed = params.has('speed') ? Number(params.get('speed')) : 1;
    return new FixtureApi(fixture, { speed: Number.isFinite(speed) ? speed : 1 });
  }
  return new HttpApi(takeToken(params));
}

/** The server opens /?token=…; keep it for this tab and drop it from the address bar. */
function takeToken(params) {
  let token = params.get('token');
  try {
    if (token) sessionStorage.setItem('dshx-token', token);
    else token = sessionStorage.getItem('dshx-token');
  } catch { /* storage blocked */ }
  if (params.has('token')) {
    params.delete('token');
    const q = params.toString();
    history.replaceState(null, '', location.pathname + (q ? `?${q}` : '') + location.hash);
  }
  return token || '';
}

const enc = encodeURIComponent;

// ---- live server ---------------------------------------------------------

class HttpApi {
  constructor(token) {
    this.token = token;
    this.fixture = null;
    this.images = new Map();
  }

  async req(method, path, body, raw) {
    const headers = {};
    if (this.token) headers.Authorization = `Bearer ${this.token}`;
    let payload;
    if (raw) {
      payload = raw;
      headers['Content-Type'] = raw.type || 'application/octet-stream';
    } else if (body !== undefined) {
      payload = JSON.stringify(body);
      headers['Content-Type'] = 'application/json';
    }
    let res;
    try {
      res = await fetch(path, { method, headers, body: payload });
    } catch {
      throw new ApiError(0, 'offline');
    }
    if (!res.ok) {
      let message = '';
      let code;
      try {
        const j = await res.json();
        message = typeof j.error === 'string' ? j.error : j.error?.message ?? j.message ?? '';
        code = j.code;
      } catch { /* not JSON */ }
      throw Object.assign(new ApiError(res.status, message || res.statusText), code ? { code } : {});
    }
    if (res.status === 202 || res.status === 204) return null;
    const type = res.headers.get('content-type') || '';
    return type.includes('json') ? res.json() : null;
  }

  info() { return this.req('GET', '/api/info'); }
  updateSettings(patch) { return this.req('PUT', '/api/settings', patch); }
  workspaces() { return this.req('GET', '/api/workspaces'); }
  addWorkspace(path) { return this.req('POST', '/api/workspaces', { path }); }
  // Project folder picker: listing, suggestions, new folder, the Mac's own dialog.
  fsDirs(path, hidden) { return this.req('GET', `/api/fs/dirs?path=${enc(path ?? '')}${hidden ? '&hidden=1' : ''}`); }
  fsSuggestions() { return this.req('GET', '/api/fs/suggestions'); }
  fsMkdir(path, name) { return this.req('POST', '/api/fs/mkdir', { path, name }); }
  fsPick(body) { return this.req('POST', '/api/fs/pick', body ?? {}); }
  sessions() { return this.req('GET', '/api/sessions'); }
  /** An empty `cwd` starts a chat without a project (the server gives it its own folder);
   * `preset` is the chat's mode (default: the server's). */
  createSession(cwd, preset) { return this.req('POST', '/api/sessions', { ...(cwd ? { cwd } : {}), ...(preset ? { preset } : {}) }); }
  session(id) { return this.req('GET', `/api/sessions/${enc(id)}`); }
  /** `{title?, pinned?, archived?, seen?, unread?}` → the session's list row. */
  updateSession(id, patch) { return this.req('PATCH', `/api/sessions/${enc(id)}`, patch); }
  prompt(id, body) { return this.req('POST', `/api/sessions/${enc(id)}/prompt`, body); }
  cancel(id) { return this.req('POST', `/api/sessions/${enc(id)}/cancel`); }
  /** `{turn, title?}` → `{id}`: a new chat with this one's log through `turn` (0 = none of it). */
  forkSession(id, body) { return this.req('POST', `/api/sessions/${enc(id)}/fork`, body); }
  /** `{base, root}`: `base` serves the chat's folder read-only for previews. */
  preview(id) { return this.req('POST', `/api/sessions/${enc(id)}/preview`); }
  files(id, dir) { return this.req('GET', `/api/sessions/${enc(id)}/files?dir=${enc(dir ?? '')}`); }

  // Plugins page: plugins, marketplaces, skills, MCP servers, connectors.
  plugins() { return this.req('GET', '/api/plugins'); }
  installPlugin(marketplace, name) { return this.req('POST', '/api/plugins', { marketplace, name, enable: true }); }
  pluginAction(id, action) { return this.req('POST', `/api/plugins/${enc(id)}/${action}`); }
  removePlugin(id) { return this.req('DELETE', `/api/plugins/${enc(id)}`); }
  /** Plugin logos load as <img>, which cannot send the bearer header. */
  assetUrl(url) { return url && this.token ? `${url}${url.includes('?') ? '&' : '?'}token=${enc(this.token)}` : url; }
  marketplaces() { return this.req('GET', '/api/marketplaces'); }
  addMarketplace(source, ref) { return this.req('POST', '/api/marketplaces', ref ? { source, ref } : { source }); }
  updateMarketplace(name) { return this.req('POST', `/api/marketplaces/${enc(name)}/update`); }
  removeMarketplace(name) { return this.req('DELETE', `/api/marketplaces/${enc(name)}`); }
  skills(cwd) { return this.req('GET', `/api/skills${cwd ? `?cwd=${enc(cwd)}` : ''}`); }
  mcpServers() { return this.req('GET', '/api/mcp/servers'); }
  addMcpServer(body) { return this.req('POST', '/api/mcp/servers', body); }
  mcpAction(name, action) { return this.req('POST', `/api/mcp/servers/${enc(name)}/${action}`); }
  removeMcpServer(name) { return this.req('DELETE', `/api/mcp/servers/${enc(name)}`); }
  mcpToken(name, token) { return this.req('PUT', `/api/mcp/servers/${enc(name)}/token`, { token }); }
  connectors() { return this.req('GET', '/api/connectors'); }
  // Skills and MCP pages: skill detail, blank skills, server forms, test, log.
  skill(name, cwd, path) { return this.req('GET', `/api/skills/${enc(name)}?${new URLSearchParams({ ...(cwd ? { cwd } : {}), ...(path ? { path } : {}) })}`); }
  createSkill(body) { return this.req('POST', '/api/skills', body); }
  /** `{enabled?, model?}` (the runtime side comes with package C). */
  skillSettings(name, patch) { return this.req('PUT', `/api/skills/${enc(name)}/settings`, patch); }
  mcpServer(name) { return this.req('GET', `/api/mcp/servers/${enc(name)}`); }
  updateMcpServer(name, body) { return this.req('PUT', `/api/mcp/servers/${enc(name)}`, body); }
  testMcpServer(body) { return this.req('POST', '/api/mcp/test', body); }
  mcpServerLog(name) { return this.req('GET', `/api/mcp/servers/${enc(name)}/log`); }
  /** `{always?, disabledTools?}` (the runtime side comes with package C). */
  mcpSettings(name, patch) { return this.req('PUT', `/api/mcp/servers/${enc(name)}/settings`, patch); }

  // Models page: providers, keys (write only) and provider model lists.
  providers() { return this.req('GET', '/api/providers'); }
  saveProvider(id, body) { return this.req('PUT', `/api/providers/${enc(id)}`, body); }
  deleteProvider(id) { return this.req('DELETE', `/api/providers/${enc(id)}`); }
  setProviderKey(id, key) { return this.req('PUT', `/api/providers/${enc(id)}/key`, { key }); }
  fetchProviderModels(id, probe) { return this.req('POST', `/api/providers/${enc(id)}/models/fetch`, probe ?? {}); }

  // Goals, schedules and Agent Teams.
  goal(id) { return this.req('GET', `/api/sessions/${enc(id)}/goal`); }
  goalCommand(id, body) { return this.req('POST', `/api/sessions/${enc(id)}/goal`, body); }
  schedules() { return this.req('GET', '/api/schedules'); }
  updateSchedule(id, body) { return this.req('PUT', `/api/schedules/${enc(id)}`, body); }
  deleteSchedule(id) { return this.req('DELETE', `/api/schedules/${enc(id)}`); }
  scheduleHistory(id, limit = 20) { return this.req('GET', `/api/schedules/${enc(id)}/history?limit=${limit}`); }
  team(id) { return this.req('GET', `/api/sessions/${enc(id)}/team`); }
  /** Tokens, USD costs and timings of a chat; `fromTurn` limits the turn list. */
  stats(id, fromTurn) { return this.req('GET', `/api/sessions/${enc(id)}/stats${fromTurn != null ? `?fromTurn=${fromTurn}` : ''}`); }
  /** `{since, tz, top}` → usage of all chats per day and model, and the top chats. */
  usage(query) { return this.req('GET', `/api/usage?${new URLSearchParams(query)}`); }
  deleteSession(id) { return this.req('DELETE', `/api/sessions/${enc(id)}`); }
  decide(callId, decision) { return this.req('POST', `/api/approvals/${enc(callId)}`, { decision }); }
  upload(file) { return this.req('POST', '/api/attachments', undefined, file); }

  // Composer (package C): `/` and `@` menus, the chat's MCP servers, commands, queue, files.
  /** `{kind: slash|mention, q, cwd?, session?, model?}` → `{sections, cacheMiss}`. */
  suggest(params) { return this.req('GET', `/api/composer/suggest?${new URLSearchParams(params)}`); }
  sessionMcp(id) { return this.req('GET', `/api/sessions/${enc(id)}/mcp`); }
  setSessionMcp(id, server, on) { return this.req(on ? 'POST' : 'DELETE', `/api/sessions/${enc(id)}/mcp/${enc(server)}`); }
  compact(id) { return this.req('POST', `/api/sessions/${enc(id)}/compact`); }
  /** `{action: edit|remove|steer, text?}` on one queued prompt. */
  updateQueue(id, item, body) { return this.req('POST', `/api/sessions/${enc(id)}/queue/${enc(item)}`, body); }
  /** Any file → `{path, name, bytes, sha256}`, stored read-only by the server. */
  uploadFile(file) { return this.req('POST', `/api/files?name=${enc(file.name || 'file')}`, undefined, file); }
  /** The session log (JSONL) as a Blob. */
  async exportLog(id) {
    const headers = this.token ? { Authorization: `Bearer ${this.token}` } : {};
    const r = await fetch(`/api/sessions/${enc(id)}/export`, { headers });
    if (!r.ok) throw new ApiError(r.status);
    return r.blob();
  }

  /** Object URL for an attachment; <img> cannot send the bearer header itself. */
  loadImage(ref) {
    const id = ref?.attachmentId;
    if (!id) return Promise.reject(new Error('no attachment'));
    if (!this.images.has(id)) {
      const headers = this.token ? { Authorization: `Bearer ${this.token}` } : {};
      const p = fetch(`/api/attachments/${enc(id)}`, { headers })
        .then((r) => (r.ok ? r.blob() : Promise.reject(new ApiError(r.status))))
        .then((b) => URL.createObjectURL(b));
      p.catch(() => this.images.delete(id));
      this.images.set(id, p);
    }
    return this.images.get(id);
  }

  /**
   * Live feed of one session. Reconnects with backoff and resumes after the
   * last event seq it delivered. `on.message(msg)`, `on.state('open'|'retry')`.
   * Without a session id it carries only status and approvals of all chats.
   */
  stream(sessionId, after, on) {
    let closed = false;
    let ws = null;
    let timer = 0;
    let tries = 0;
    let last = after;
    const open = () => {
      if (closed) return;
      const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
      const q = new URLSearchParams(sessionId ? { session: sessionId, after: String(last) } : {});
      if (this.token) q.set('token', this.token);
      ws = new WebSocket(`${proto}//${location.host}/api/stream?${q}`);
      ws.onopen = () => { tries = 0; on.state?.('open'); };
      ws.onmessage = (m) => {
        let msg;
        try { msg = JSON.parse(m.data); } catch { return; }
        if (msg.kind === 'event' && typeof msg.event?.seq === 'number') last = Math.max(last, msg.event.seq);
        on.message(msg);
      };
      ws.onclose = () => {
        if (closed) return;
        on.state?.('retry');
        timer = setTimeout(open, Math.min(8000, 400 * 2 ** tries++));
      };
    };
    open();
    return () => {
      closed = true;
      clearTimeout(timer);
      if (ws) { ws.onclose = null; ws.close(); }
    };
  }
}

// ---- fixture replay ------------------------------------------------------

const DEFAULT_INFO = {
  version: '0.3.0-dev',
  platform: 'darwin',
  locale: 'en',
  models: [
    ...[['deepseek-flash', 'DeepSeek-V4.1-Flash', ['text', 'image'], 0.30, 1.20, 0.006], ['deepseek-v4-pro', 'DeepSeek-V4-Pro', ['text'], 1.32, 3.96, 0.044]]
      .map(([id, name, inputModalities, input, output, cacheRead]) => ({
        id, name, contextWindow: 1000000, inputModalities, reasoningEfforts: ['off', 'low', 'high', 'max'], defaultReasoningEffort: 'high',
        provider: 'deepseek-official', providerName: 'DeepSeek', model: id, available: true,
        cost: { input, output, cacheRead, cacheWrite: input },
        offPeakCost: { input: input / 2, output: output / 2, cacheRead: cacheRead / 2, cacheWrite: input / 2 },
      })),
    ...[['claude-opus-5-5', 'Claude Opus 5.5', 4, 20], ['claude-sonnet-5-5', 'Claude Sonnet 5.5', 2, 10]].map(([model, name, input, output]) => ({
      id: `anthropic/${model}`, name, contextWindow: 1000000, maxTokens: 128000, inputModalities: ['text', 'image'],
      reasoningEfforts: ['low', 'medium', 'high', 'xhigh', 'max'], defaultReasoningEffort: 'high',
      provider: 'anthropic', providerName: 'Anthropic', model, available: true, cost: { input, output, cacheRead: input / 20, cacheWrite: input * 1.25 },
    })),
  ],
  providers: [{ id: 'deepseek-official', name: 'DeepSeek', available: true }, { id: 'anthropic', name: 'Anthropic', available: true }],
  settings: {
    model: 'deepseek-flash', effort: 'high', sandbox: 'workspace-write', approval: 'ask', defaultPreset: 'standard',
    compaction: { mode: 'threshold', thresholdTokens: 200000 },
  },
  presets: [{ id: 'standard', name: 'Standard' }, { id: 'minimal', name: 'Minimal' }, { id: 'ptc', name: 'PTC' }],
  apiKeyConfigured: true,
  tools: { browser: true, app: true, computer: true },
  ui: { workDetails: 'standard', performanceUsage: 'detailed' },
};

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const clone = (v) => JSON.parse(JSON.stringify(v));

/**
 * A long fixture chat without a long file: `count` copies of `events`, where
 * "{n}" is the turn number and "{p}" cycles through `prompts`.
 */
function repeatTurns(log, { count, events, prompts = [''] }) {
  const fill = (v, n) => {
    if (typeof v === 'string') return v === '{n}' ? n : v.replaceAll('{n}', String(n)).replaceAll('{p}', prompts[(n - 1) % prompts.length]);
    if (Array.isArray(v)) return v.map((x) => fill(x, n));
    if (v && typeof v === 'object') return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, fill(x, n)]));
    return v;
  };
  let time = log.length ? log[log.length - 1].time : Date.now() - count * 60_000;
  for (let n = 1; n <= count; n++) {
    for (const e of events) log.push({ ...fill(e, n), seq: log.length, time: (time += 1500) });
  }
}

/** The Models page of a fixture: built-in DeepSeek and Anthropic from the catalog. */
function fixtureProviders() {
  const claude = (id, name, input, output, efforts) => ({
    id, ref: `anthropic/${id}`, name, contextWindow: 1000000, maxTokens: 128000, input: ['text', 'image'],
    reasoningEfforts: efforts, defaultReasoningEffort: 'high', thinking: 'adaptive',
    cost: { input, output, cacheRead: input / 20, cacheWrite: input * 1.25 },
  });
  const full = ['low', 'medium', 'high', 'xhigh', 'max'];
  return [
    {
      id: 'deepseek-official', name: 'DeepSeek', api: 'anthropic-messages', baseUrl: 'https://api.deepseek.com/anthropic',
      defaultBaseUrl: 'https://api.deepseek.com/anthropic', apiKeyEnv: 'DEEPSEEK_API_KEY', builtin: true, catalog: true,
      modelsEditable: false, key: { configured: true, source: 'stored', locked: false, required: true }, headers: [],
      models: DEFAULT_INFO.models.filter((m) => m.provider === 'deepseek-official')
        .map((m) => ({ id: m.id, ref: m.id, name: m.name, contextWindow: m.contextWindow, input: m.inputModalities, reasoningEfforts: m.reasoningEfforts, cost: m.cost, offPeakCost: m.offPeakCost })),
    },
    {
      id: 'anthropic', name: 'Anthropic', api: 'anthropic-messages', baseUrl: 'https://api.anthropic.com',
      defaultBaseUrl: 'https://api.anthropic.com', apiKeyEnv: 'ANTHROPIC_API_KEY', builtin: false, catalog: true,
      modelsEditable: true, modelsCustomized: false, titleModel: 'claude-haiku-5-5', headers: [],
      key: { configured: false, source: null, locked: false, env: 'ANTHROPIC_API_KEY', required: true },
      models: [
        claude('claude-opus-5-5', 'Claude Opus 5.5', 4, 20, full),
        claude('claude-sonnet-5-5', 'Claude Sonnet 5.5', 2, 10, full),
        claude('claude-haiku-5-5', 'Claude Haiku 5.5', 0.1, 0.5, ['off', ...full]),
        claude('claude-fable-5-1', 'Claude Fable 5.1', 10, 50, full),
      ],
    },
  ];
}

/**
 * Replays ui/fixtures/<name>.json. Events before `live` are history; the rest
 * stream in with text/reasoning deltas, as dshx-server would send them.
 * `?speed=0` renders the final state at once (used for screenshots).
 */
class FixtureApi {
  constructor(name, { speed }) {
    this.name = name;
    this.fixture = name;
    this.speed = speed;
    this.ready = this.load();
    this.subs = new Set();
    this.uploads = new Map();
    this.cancelled = false;
  }

  async load() {
    const get = async (n) => {
      const r = await fetch(`fixtures/${n}.json`);
      if (!r.ok) throw new ApiError(r.status, `fixture "${n}" not found`);
      return r.json();
    };
    const [fx, shared] = await Promise.all([get(this.name), get('_shared')]);
    this.fx = fx;
    this.shared = shared;
    this.infoData = { ...DEFAULT_INFO, ...shared.info, ...fx.info };
    // ?view=compact|standard|detailed|verbose picks the work-details view for screenshots.
    const view = new URLSearchParams(location.search).get('view');
    if (view) this.infoData.ui = { ...this.infoData.ui, workDetails: view };
    const s = fx.session;
    if (s && fx.repeat) repeatTurns(s.events, fx.repeat);
    this.sessionData = s ? { header: s.header, events: s.events } : null;
    const live = this.speed === 0 ? s?.events.length ?? 0 : fx.live ?? s?.events.length ?? 0;
    this.history = s ? s.events.slice(0, live) : [];
    this.pendingLive = s ? s.events.slice(live) : [];
    // A replayed turn happens now: move its history next to the clock so
    // durations ("Worked for …") stay sane once live events carry Date.now().
    if (this.pendingLive.length && this.history.length) {
      const shift = Date.now() - 1500 - this.history[this.history.length - 1].time;
      for (const e of this.history) e.time += shift;
    }
    const now = Date.now();
    for (const x of shared.sessions ?? []) {
      if (x.updatedAgo != null) x.updatedAt = now - x.updatedAgo;
    }
    for (const w of shared.workspaces ?? []) {
      if (w.lastUsedAgo != null) w.lastUsed = now - w.lastUsedAgo;
    }
    // Live events are re-numbered after the history, so they keep their fixture order.
    this.seq = this.history.length ? this.history[this.history.length - 1].seq : -1;
    this.status = this.pendingLive.length ? 'running' : fx.status ?? 'idle';
  }

  async info() { await this.ready; return clone(this.infoData); }

  async updateSettings(patch) {
    await this.ready;
    const { apiKey, ui, compaction, ...rest } = patch;
    this.infoData.settings = { ...this.infoData.settings, ...rest };
    if (compaction) this.infoData.settings.compaction = { ...this.infoData.settings.compaction, ...compaction };
    if (ui) this.infoData.ui = { ...this.infoData.ui, ...ui };
    if (apiKey) this.infoData.apiKeyConfigured = true;
    return clone(this.infoData);
  }

  async workspaces() { await this.ready; return clone(this.shared.workspaces ?? []); }

  async addWorkspace(path) {
    await this.ready;
    const ws = { path, name: path.split('/').filter(Boolean).pop() || path, lastUsed: Date.now() };
    this.shared.workspaces = [ws, ...(this.shared.workspaces ?? []).filter((w) => w.path !== path)];
    return ws;
  }

  fsDirs(path, hidden) { return fixtureFs.dirs(path, hidden); }
  fsSuggestions() { return fixtureFs.suggestions(); }
  fsMkdir(path, name) { return fixtureFs.mkdir(path, name); }
  fsPick() { return fixtureFs.pick(); }

  async sessions() {
    await this.ready;
    const list = clone(this.shared.sessions ?? []);
    const own = this.sessionData?.header.id;
    for (const s of list) if (s.id === own) s.status = this.status === 'running' ? 'running' : s.status;
    if (this.created) list.unshift(this.created);
    return list;
  }

  async updateSession(id, patch) {
    await this.ready;
    const rows = [...(this.shared.sessions ?? []), ...(this.created ? [this.created] : [])];
    const row = rows.find((s) => s.id === id);
    if (!row) throw new ApiError(404, 'session not found');
    if ('title' in patch) row.title = patch.title || row.title;
    for (const k of ['pinned', 'archived']) if (k in patch) row[k] = !!patch[k];
    if (patch.seen) row.unread = false;
    if ('unread' in patch) row.unread = !!patch.unread;
    return clone(row);
  }

  async createSession(cwd, preset) {
    await this.ready;
    const id = `session-fixture-${Date.now().toString(36)}`;
    const scratch = !cwd;
    if (scratch) cwd = `/Users/alex/.dsh-codex/chats/chat-${id.slice(-8)}`;
    const agentPreset = preset ?? this.infoData.settings.defaultPreset ?? 'standard';
    this.sessionData = { header: { type: 'session', version: 4, id, createdAt: Date.now(), cwd, isSeeded: false, delegationDepth: 0, agentPreset }, events: [] };
    this.history = [];
    this.pendingLive = [];
    this.seq = -1;
    this.status = 'idle';
    this.created = { id, title: '', cwd, scratch, createdAt: Date.now(), updatedAt: Date.now(), status: 'idle', preset: agentPreset };
    return { id };
  }

  /** Like the server: the log through the turn's end, the inherited cut, the same folder and mode. */
  async forkSession(id, { turn, title }) {
    await this.ready;
    if (this.sessionData?.header.id !== id) throw new ApiError(404, 'session not found');
    const end = turn === 0 ? -1 : this.history.findIndex((e) => e.type === 'turn/end' && e.data?.turn === turn);
    if (end < 0 && turn !== 0) throw new ApiError(400, `turn ${turn} has not ended, so it cannot be branched`);
    const source = this.sessionData.header;
    const row = (this.shared.sessions ?? []).find((s) => s.id === id) ?? this.created ?? {};
    const child = `session-fixture-${Date.now().toString(36)}`;
    const seed = this.history.slice(0, end + 1);
    seed.push({ type: 'session/end-seed', seq: seed.length, time: seed.length ? seed[seed.length - 1].time : Date.now(), data: { inherited: true } });
    const { agentPreset } = source;
    this.sessionData = { header: { ...source, id: child, createdAt: Date.now(), isSeeded: true, parentSession: id }, events: seed };
    this.history = clone(seed);
    this.pendingLive = [];
    this.seq = seed.length - 1;
    this.status = 'idle';
    this.fx = { ...this.fx, approval: null, stats: null };
    this.created = { id: child, title: title || `${row.title || ''} (1)`, cwd: source.cwd, scratch: !!row.scratch,
      createdAt: Date.now(), updatedAt: Date.now(), status: 'idle', ...(agentPreset ? { preset: agentPreset } : {}) };
    return { id: child };
  }

  async session(id) {
    await this.ready;
    if (!this.sessionData || this.sessionData.header.id !== id) {
      // The sidebar lists every fixture; opening another one switches fixtures.
      const entry = (this.shared.sessions ?? []).find((s) => s.id === id);
      if (entry?.fixture) {
        const q = new URLSearchParams(location.search);
        q.set('fixture', entry.fixture);
        location.href = `${location.pathname}?${q}`;
        return new Promise(() => {});
      }
      throw new ApiError(404, 'session not found');
    }
    return { header: clone(this.sessionData.header), events: clone(this.history), status: this.status };
  }

  emit(msg) { for (const on of this.subs) on.message(msg); }

  push(event) {
    const ev = { ...clone(event), seq: ++this.seq, time: Date.now() };
    this.history.push(ev);
    this.emit({ kind: 'event', sessionId: this.sessionData.header.id, event: ev });
    if (ev.type === 'session/title' && this.created) this.created.title = ev.data.title;
  }

  setStatus(status, error) {
    this.status = status;
    this.emit({ kind: 'status', sessionId: this.sessionData?.header.id, status, ...(error ? { error } : {}) });
  }

  stream(sessionId, _after, on) {
    this.subs.add(on);
    on.state?.('open');
    if (!sessionId) return () => this.subs.delete(on); // the sidebar's status feed
    if (!this.started) {
      this.started = true;
      setTimeout(() => this.replay(), this.speed === 0 ? 0 : 500 / this.speed);
    }
    return () => this.subs.delete(on);
  }

  async wait(ms) { if (this.speed > 0) await sleep(ms / this.speed); }

  /** Streams the live tail: deltas first, then the event that settles them. */
  async play(events) {
    for (const raw of events) {
      if (this.cancelled) return false;
      const d = raw.data ?? {};
      if (raw.type === 'assistant/message' && this.speed > 0) {
        for (const [index, block] of (d.message?.content ?? []).entries()) {
          if (block.type !== 'text' && block.type !== 'reasoning') continue;
          const text = block.text ?? '';
          for (let i = 0; i < text.length && !this.cancelled;) {
            const n = 6 + Math.floor(Math.random() * 10);
            this.emit({ kind: 'delta', sessionId: this.sessionData.header.id, turn: d.turn, step: d.step, index,
              blockType: block.type, text: text.slice(i, i + n) });
            i += n;
            await this.wait(block.type === 'reasoning' ? 14 : 22);
          }
          await this.wait(120);
        }
      } else if (raw.type === 'tool/result') {
        const tool = d.meta?.tool;
        await this.wait(tool === 'browser' || tool === 'computer' || tool === 'app' ? 1700 : 380);
      } else if (raw.type === 'step/start') {
        await this.wait(260);
      } else if (raw.type === 'llm/retry-started') {
        await this.wait(1800);
      } else {
        await this.wait(30);
      }
      if (this.cancelled) return false;
      this.push(raw);
    }
    return true;
  }

  async replay() {
    await this.ready;
    const fx = this.fx;
    if (!this.pendingLive.length && !fx.approval) return; // nothing to stream; session() told the status
    if (this.pendingLive.length) {
      this.setStatus('running');
      const tail = this.pendingLive;
      this.pendingLive = [];
      if (!(await this.play(tail))) return;
    }
    if (fx.approval) {
      this.setStatus('waiting-approval');
      this.emit({ kind: 'approval', sessionId: this.sessionData.header.id, ...fx.approval });
    } else {
      this.setStatus(fx.status ?? 'idle', fx.statusError);
    }
  }

  async decide(callId, decision) {
    await this.ready;
    const next = this.fx.onApproval?.[decision];
    if (!next) return null;
    this.fx.approval = null;
    this.setStatus('running');
    (async () => {
      if (await this.play(next)) this.setStatus('idle');
    })();
    return null;
  }

  async prompt(id, body) {
    await this.ready;
    this.cancelled = false;
    fixtureComposer.onPrompt(this, body.text);
    const turn = 1 + this.history.filter((e) => e.type === 'turn/start').length;
    const msgId = `fixture-${Date.now().toString(36)}`;
    const content = [{ type: 'text', text: body.text }];
    for (const img of body.images ?? []) content.push({ type: 'image', attachment: img });
    const reply = t.fixtureReply;
    const events = [
      { type: 'turn/start', data: { turn } },
      { type: 'step/start', data: { turn, step: 1 } },
      { type: 'user/message', data: { role: 'user', id: msgId, source: { kind: 'user' }, content }, surfaceOp: 'append' },
      ...(this.created && !this.created.title
        ? [{ type: 'session/title', data: { title: body.text.slice(0, 40), messageSeqs: [], source: { kind: 'fallback' } } }]
        : []),
      { type: 'assistant/message', data: { turn, step: 1, message: { role: 'assistant', id: `${msgId}-a`,
        content: [{ type: 'reasoning', text: 'Fixture mode, so there is nothing to do but answer.' }, { type: 'text', text: reply }],
        source: { kind: 'model', provider: 'deepseek-official', model: body.model ?? this.infoData.settings.model } },
        stream: [], usage: { inputTokens: 900, outputTokens: 40, cacheReadTokens: 3200, totalTokens: 4140 } }, surfaceOp: 'append' },
      { type: 'step/end', data: { turn, step: 1 } },
      { type: 'turn/end', data: { turn, reason: { kind: 'completed' } } },
    ];
    this.setStatus('running');
    (async () => {
      // The user message lands at once, like the server's inbox commit.
      for (const ev of events.splice(0, 3)) this.push(ev);
      if (await this.play(events)) this.setStatus('idle');
    })();
    return null;
  }

  async cancel() {
    await this.ready;
    this.cancelled = true;
    const turn = this.history.filter((e) => e.type === 'turn/start').length;
    if (this.status !== 'idle') this.push({ type: 'turn/end', data: { turn, reason: { kind: 'aborted', reason: { kind: 'user' } } } });
    this.fx.approval = null;
    this.setStatus('idle');
    return null;
  }

  async preview() {
    await this.ready;
    return { base: 'fixtures/files/', root: this.sessionData?.header.cwd ?? '' };
  }

  // Fixtures have no plugins, automations or teams; skills, MCP servers
  // and connectors come from fixtures/extensions.json (fixture-ext.js).
  async plugins() { return []; }
  async marketplaces() { return []; }
  skills(cwd) { return fixtureExt.skills(cwd); }
  skill(name, cwd, path) { return fixtureExt.skill(name, path); }
  createSkill(body) { return fixtureExt.createSkill(body); }
  skillSettings(name, patch) { return fixtureExt.skillSettings(name, patch); }
  mcpServers() { return fixtureExt.servers(); }
  mcpServer(name) { return fixtureExt.server(name); }
  addMcpServer(body) { return fixtureExt.addServer(body); }
  updateMcpServer(name, body) { return fixtureExt.updateServer(name, body); }
  testMcpServer(body) { return fixtureExt.testServer(body); }
  mcpServerLog(name) { return fixtureExt.log(name); }
  mcpAction(name, action) { return fixtureExt.action(name, action); }
  removeMcpServer(name) { return fixtureExt.removeServer(name); }
  mcpToken(name, token) { return fixtureExt.token(name, token); }
  mcpSettings(name, patch) { return fixtureExt.serverSettings(name, patch); }
  connectors() { return fixtureExt.connectors(); }
  async schedules() { return []; }
  async goal() { return { goal: null }; }
  async team() { throw new ApiError(404, 'team'); }

  /** The fixture's `stats` came from the server's stats route; turns appear once they end. */
  async stats(_id, fromTurn) {
    await this.ready;
    const s = this.fx.stats;
    if (!s) throw new ApiError(404, 'stats');
    const ended = new Set(this.history.filter((e) => e.type === 'turn/end').map((e) => e.data?.turn));
    const out = clone({ ...s, turns: s.turns.filter((x) => ended.has(x.turn) && (fromTurn == null || x.turn >= fromTurn)) });
    // Like the server: the chat's rule follows Settings → Context.
    const rule = out.context?.compaction;
    const setting = this.infoData.settings.compaction;
    if (rule && setting) {
      rule.mode = rule.auto ? setting.mode : 'off';
      if (rule.auto && setting.mode === 'threshold') rule.turnEndTokens = setting.thresholdTokens;
    }
    return out;
  }

  /** `_shared.json` holds a 7- and a 30-day report with days counted back from today. */
  async usage({ since }) {
    await this.ready;
    const reports = this.shared.usage;
    if (!reports) throw new ApiError(404, 'usage');
    const span = Math.round((Date.now() - since) / 86400000);
    const r = clone(span > 8 ? reports.month : reports.week);
    const label = (ago) => {
      const d = new Date();
      d.setDate(d.getDate() - ago);
      return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
    };
    for (const d of r.days) d.day = label(d.ago);
    for (const c of r.topChats) c.updatedAt = Date.now() - (c.updatedAgo ?? 0);
    return { ...r, since, until: Date.now() };
  }
  assetUrl(url) { return url; }

  // Composer (package C), see fixture-composer.js.
  suggest(params) { return fixtureComposer.suggest(this, params); }
  sessionMcp(id) { return fixtureComposer.sessionMcp(this, id); }
  setSessionMcp(id, server, on) { return fixtureComposer.setSessionMcp(this, id, server, on); }
  compact(id) { return fixtureComposer.compact(this, id); }
  updateQueue(id, item, body) { return fixtureComposer.updateQueue(this, id, item, body); }
  uploadFile(file) { return fixtureComposer.uploadFile(this, file); }
  exportLog(id) { return fixtureComposer.exportLog(this, id); }

  // Models page: DeepSeek plus an Anthropic route, kept in memory.
  async providers() {
    await this.ready;
    this.providerRows ??= fixtureProviders();
    const added = new Set(this.providerRows.map((p) => p.id));
    return clone({
      providers: this.providerRows,
      catalog: [
        { id: 'deepseek-official', name: 'DeepSeek', api: 'anthropic-messages', models: 2, added: true },
        { id: 'anthropic', name: 'Anthropic', api: 'anthropic-messages', baseUrl: 'https://api.anthropic.com', apiKeyEnv: 'ANTHROPIC_API_KEY', models: 4, added: added.has('anthropic') },
      ],
    });
  }

  async saveProvider(id, body) {
    await this.providers();
    const rows = this.providerRows;
    const old = rows.find((p) => p.id === id);
    const models = (body.models ?? old?.models ?? []).map((m) => ({ ...m, ref: `${id}/${m.id}`, name: m.name ?? m.id }));
    const row = {
      ...(old ?? { id, builtin: false, catalog: id === 'anthropic', modelsEditable: true, headers: [] }),
      name: body.displayName || old?.name || (id === 'anthropic' ? 'Anthropic' : id),
      api: body.api ?? old?.api ?? 'anthropic-messages',
      baseUrl: body.baseUrl ?? old?.baseUrl ?? 'https://api.anthropic.com',
      models: id === 'deepseek-official' ? old.models : models,
      modelsCustomized: !!body.models,
      key: body.apiKey ? { configured: true, source: 'stored', locked: false, required: true } : old?.key ?? { configured: false, required: id === 'anthropic' },
    };
    this.providerRows = old ? rows.map((p) => (p.id === id ? row : p)) : [...rows, row];
    return clone(row);
  }

  async deleteProvider(id) {
    await this.providers();
    this.providerRows = this.providerRows.filter((p) => p.id !== id);
    return null;
  }

  async setProviderKey(id, key) {
    await this.providers();
    const row = this.providerRows.find((p) => p.id === id);
    row.key = { ...row.key, configured: !!key, source: key ? 'stored' : null };
    return clone(row);
  }

  async fetchProviderModels() {
    await this.wait(600);
    return { models: ['glm-5.3', 'glm-5.3-flash', 'kimi-k3', 'deepseek-v4.1-flash'].map((id) => ({ id, contextWindow: 1000000 })) };
  }

  async files(_id, dir) {
    await this.ready;
    const all = this.fx.files ?? this.shared.files ?? [];
    const prefix = dir ? `${dir}/` : '';
    const entries = all.filter((e) => e.path.startsWith(prefix) && !e.path.slice(prefix.length).includes('/'));
    return { dir: dir ?? '', entries, truncated: false };
  }

  async deleteSession(id) {
    await this.ready;
    this.shared.sessions = (this.shared.sessions ?? []).filter((s) => s.id !== id);
    if (this.created?.id === id) this.created = null;
    return null;
  }

  async upload(file) {
    const id = `upload-${Math.random().toString(36).slice(2, 10)}`;
    this.uploads.set(id, URL.createObjectURL(file));
    const dims = await new Promise((resolve) => {
      const img = new Image();
      img.onload = () => resolve({ width: img.naturalWidth, height: img.naturalHeight });
      img.onerror = () => resolve({ width: 0, height: 0 });
      img.src = this.uploads.get(id);
    });
    return { attachmentId: id, mediaType: file.type || 'image/png', bytes: file.size, ...dims, name: file.name };
  }

  async loadImage(ref) {
    await this.ready;
    const id = ref?.attachmentId;
    if (this.uploads.has(id)) return this.uploads.get(id);
    const path = this.fx.attachments?.[id] ?? this.shared.attachments?.[id];
    if (!path) throw new Error(`unknown attachment ${id}`);
    return `fixtures/${path}`;
  }
}
