// UI ↔ server protocol (docs/rust/SPEC.md §6) and the offline fixture mode.
//
// Both implementations expose the same methods, so the rest of the UI never
// knows whether it talks to dshx-server or replays ui/fixtures/<name>.json.
import { t } from './i18n.js';

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
      try {
        const j = await res.json();
        message = typeof j.error === 'string' ? j.error : j.error?.message ?? j.message ?? '';
      } catch { /* not JSON */ }
      throw new ApiError(res.status, message || res.statusText);
    }
    if (res.status === 202 || res.status === 204) return null;
    const type = res.headers.get('content-type') || '';
    return type.includes('json') ? res.json() : null;
  }

  info() { return this.req('GET', '/api/info'); }
  updateSettings(patch) { return this.req('PUT', '/api/settings', patch); }
  workspaces() { return this.req('GET', '/api/workspaces'); }
  addWorkspace(path) { return this.req('POST', '/api/workspaces', { path }); }
  sessions() { return this.req('GET', '/api/sessions'); }
  /** An empty `cwd` starts a chat without a project (the server gives it its own folder). */
  createSession(cwd) { return this.req('POST', '/api/sessions', cwd ? { cwd } : {}); }
  session(id) { return this.req('GET', `/api/sessions/${enc(id)}`); }
  /** `{title?, pinned?, archived?, seen?, unread?}` → the session's list row. */
  updateSession(id, patch) { return this.req('PATCH', `/api/sessions/${enc(id)}`, patch); }
  prompt(id, body) { return this.req('POST', `/api/sessions/${enc(id)}/prompt`, body); }
  cancel(id) { return this.req('POST', `/api/sessions/${enc(id)}/cancel`); }
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
  deleteSession(id) { return this.req('DELETE', `/api/sessions/${enc(id)}`); }
  decide(callId, decision) { return this.req('POST', `/api/approvals/${enc(callId)}`, { decision }); }
  upload(file) { return this.req('POST', '/api/attachments', undefined, file); }

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
    { id: 'deepseek-flash', name: 'deepseek-flash', contextWindow: 1000000, inputModalities: ['text', 'image'],
      reasoningEfforts: ['off', 'low', 'high', 'max'], defaultReasoningEffort: 'high',
      provider: 'deepseek-official', providerName: 'DeepSeek', model: 'deepseek-flash', available: true },
    { id: 'deepseek-v4-pro', name: 'deepseek-v4-pro', contextWindow: 1000000, inputModalities: ['text'],
      reasoningEfforts: ['off', 'low', 'high', 'max'], defaultReasoningEffort: 'high',
      provider: 'deepseek-official', providerName: 'DeepSeek', model: 'deepseek-v4-pro', available: true },
    ...[['claude-opus-5-5', 'Claude Opus 5.5', 4, 20], ['claude-sonnet-5-5', 'Claude Sonnet 5.5', 2, 10]].map(([model, name, input, output]) => ({
      id: `anthropic/${model}`, name, contextWindow: 1000000, maxTokens: 128000, inputModalities: ['text', 'image'],
      reasoningEfforts: ['low', 'medium', 'high', 'xhigh', 'max'], defaultReasoningEffort: 'high',
      provider: 'anthropic', providerName: 'Anthropic', model, available: true, cost: { input, output, cacheRead: input / 20, cacheWrite: input * 1.25 },
    })),
  ],
  providers: [{ id: 'deepseek-official', name: 'DeepSeek', available: true }, { id: 'anthropic', name: 'Anthropic', available: true }],
  settings: { model: 'deepseek-flash', effort: 'high', sandbox: 'workspace-write', approval: 'ask' },
  apiKeyConfigured: true,
  tools: { browser: true, app: true, computer: true },
};

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const clone = (v) => JSON.parse(JSON.stringify(v));

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
        .map((m) => ({ id: m.id, ref: m.id, name: m.name, contextWindow: m.contextWindow, input: m.inputModalities, reasoningEfforts: m.reasoningEfforts })),
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
    const s = fx.session;
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
    const { apiKey, ...rest } = patch;
    this.infoData.settings = { ...this.infoData.settings, ...rest };
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

  async createSession(cwd) {
    await this.ready;
    const id = `session-fixture-${Date.now().toString(36)}`;
    const scratch = !cwd;
    if (scratch) cwd = `/Users/alex/.dsh-codex/chats/chat-${id.slice(-8)}`;
    this.sessionData = { header: { type: 'session', version: 4, id, createdAt: Date.now(), cwd, isSeeded: false, delegationDepth: 0 }, events: [] };
    this.history = [];
    this.pendingLive = [];
    this.seq = -1;
    this.status = 'idle';
    this.created = { id, title: '', cwd, scratch, createdAt: Date.now(), updatedAt: Date.now(), status: 'idle' };
    return { id };
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

  // Fixtures have no plugins, automations or teams.
  async plugins() { return []; }
  async marketplaces() { return []; }
  async skills() { return []; }
  async mcpServers() { return []; }
  async connectors() { return []; }
  async schedules() { return []; }
  async goal() { return { goal: null }; }
  async team() { throw new ApiError(404, 'team'); }
  assetUrl(url) { return url; }

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
