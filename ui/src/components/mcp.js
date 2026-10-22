// MCP servers tab of the Plugins page: cards with status, tools and the
// "Active" / "Always load" switches, the server drawer (Tools, Settings,
// Log, Sign-in; shared with the Apps tab) and the "Add server" dialog
// (catalog, custom server with "Test connection", pasted JSON). Servers
// reach a chat through @name unless "Always load" is on (ux-plan §1, §8).
import { useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t, quote } from '../i18n.js';
import { api, fail, toast, ask } from '../state.js';
import { copyText } from '../markdown.js';
import { IconPlus, IconRefresh, IconTrash, IconSearch, IconCopy, IconChevronRight, IconAlert, IconKey, IconFile, IconPlug } from '../icons.js';
import { useLoad, Toggle, Pill } from './page.js';
import { Logo, MoreMenu, StatusDot, useAction, useLoginWait, useRoute, routeMissing, isMissingRoute, usePluginFocus } from './plugin-kit.js';
import { Drawer, Dialog, Field } from './drawer.js';
import { ServerForm, ProbeResult, emptyForm, formOf, fromImport, validate, bodyOf, useProbe } from './mcp-form.js';
import { parseServers } from '../mcp-import.js';

const SETTINGS_ROUTE = '/api/mcp/servers/_/settings';
const isRemote = (s) => s.transport && s.transport !== 'stdio';
const where = (s) => s.url || [s.command, ...(s.args ?? [])].filter(Boolean).join(' ');
const clock = (ms) => new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });

/** Per-chat settings (`always`, `disabled_tools`) need the settings route. */
export function useMcpSettings(reload) {
  const route = useRoute(SETTINGS_ROUTE);
  const [unsupported, setUnsupported] = useState(false);
  const [busy, setBusy] = useState(null);
  const supported = !unsupported && route !== false;
  const change = async (s, patch) => {
    setBusy(s.name);
    try {
      return await api.mcpSettings(s.name, patch);
    } catch (e) {
      if (isMissingRoute(e)) { routeMissing(SETTINGS_ROUTE); setUnsupported(true); toast(t.mcpSettingsUnsupported); } else fail(e);
      return null;
    } finally {
      setBusy(null);
      reload();
    }
  };
  return { supported, checking: route === null, busy, change };
}

/** Logo of a server: its connector's colour, else the initial. */
function ServerLogo({ s, connectors, size }) {
  const c = connectors?.find((x) => x.id === s.connector);
  return html`<${Logo} color=${c?.brandColor} name=${c?.name ?? s.name} size=${size} />`;
}

const toolCount = (s) => {
  const n = s.tools?.length ?? 0;
  const off = (s.disabledTools ?? []).filter((d) => s.tools?.some((tool) => tool.rawName === d)).length;
  return off ? t.mcpToolsOff(n, off) : t.mcpToolsN(n);
};

// ---- cards -------------------------------------------------------------------------------

function ServerCard({ s, connectors, busy, run, settings, onOpen, onLogin }) {
  const plugin = s.origin === 'plugin';
  const remote = isRemote(s);
  const items = [
    s.enabled ? [html`<${IconRefresh} size=${14} />`, t.restart, () => run(s.name, () => api.mcpAction(s.name, 'restart'))] : null,
    s.status === 'needs-login' ? [html`<${IconKey} size=${14} />`, t.mcpSignIn, () => run(s.name, () => onLogin(s.name))] : null,
    s.auth?.login ? [null, t.disconnect, () => run(s.name, () => api.mcpAction(s.name, 'logout'))] : null,
    [html`<${IconFile} size=${14} />`, t.mcpShowLog, () => onOpen(s.name, 'log')],
    plugin ? null : [html`<${IconTrash} size=${14} />`, t.remove, async () => { if (await ask({ title: t.confirmRemoveServer(s.name), action: t.remove })) run(s.name, () => api.removeMcpServer(s.name)); }, 'danger'],
  ];
  return html`<div class=${cx('plug-card mcp-card', !s.enabled && 'is-off')}>
    <button type="button" class="mcp-card-main" onClick=${() => onOpen(s.name)} aria-label=${`${s.name}: ${t.mcpDetails}`}>
      <${ServerLogo} s=${s} connectors=${connectors} />
      <span class="plug-text">
        <span class="plug-name"><span class="mcp-card-name">${s.name}</span><${Pill} tone="off">${remote ? t.mcpRemote : t.mcpLocal}</${Pill}></span>
        <span class="mcp-card-status"><${StatusDot} status=${s.status} retrying=${s.retrying} />
          ${s.status === 'connected' ? html`<span class="mcp-sep">·</span><span>${toolCount(s)}</span>` : null}</span>
        <span class="plug-meta mono mcp-card-where" title=${where(s)}>${plugin ? `${t.mcpFromPlugin(s.plugin)} · ` : ''}${where(s)}</span>
        ${s.error && s.status !== 'connected' ? html`<span class="plug-warn" title=${s.error}>${s.error}</span>` : null}
      </span>
    </button>
    <${MoreMenu} items=${items} />
    <div class="mcp-card-foot">
      <label class="mcp-switch" title=${plugin ? t.mcpPluginOwned(s.plugin) : ''}>
        <${Toggle} on=${s.enabled} label=${t.mcpActiveFor(s.name)} disabled=${plugin || busy === s.name}
          onChange=${(on) => run(s.name, () => api.mcpAction(s.name, on ? 'enable' : 'disable'))} />
        <span>${t.mcpActive}</span>
      </label>
      <label class="mcp-switch" title=${settings.supported ? t.mcpAlwaysHint : t.mcpSettingsUnsupported}>
        <${Toggle} on=${s.always === true} label=${t.mcpAlwaysFor(s.name)} disabled=${!settings.supported || settings.checking || settings.busy === s.name || !s.enabled}
          onChange=${(on) => settings.change(s, { always: on })} />
        <span>${t.mcpAlways}</span>
      </label>
    </div>
  </div>`;
}

function CardSkeleton() {
  return html`<div class="plug-grid" aria-busy="true" aria-label=${t.loading}>${[0, 1, 2, 3].map((i) => html`<div key=${i} class="plug-card mcp-card sk-skeleton">
    <span class="mcp-card-main"><span class="plug-logo sk-ghost" /><span class="sk-text"><span class="sk-bar" style="width:40%" /><span class="sk-bar" style="width:70%" /></span></span>
  </div>`)}</div>`;
}

// ---- drawer: tools ----------------------------------------------------------------------------

/** Parameters of a JSON schema as rows, plus the raw schema. */
function Schema({ schema }) {
  const [raw, setRaw] = useState(false);
  const props = Object.entries(schema?.properties ?? {});
  const required = new Set(schema?.required ?? []);
  return html`<div class="mcp-schema">
    ${props.length ? html`<ul class="mcp-params">${props.map(([name, p]) => html`<li key=${name}>
      <span class="mono mcp-param">${name}${required.has(name) ? html`<span class="mcp-req" title=${t.mcpRequired}>*</span>` : null}</span>
      <span class="mcp-ptype mono">${Array.isArray(p?.type) ? p.type.join(' | ') : p?.type ?? (p?.enum ? 'enum' : '')}${p?.enum ? ` (${p.enum.join(', ')})` : ''}</span>
      ${p?.description ? html`<span class="mcp-pdesc">${p.description}</span>` : null}
    </li>`)}</ul>` : html`<p class="sk-note">${t.mcpNoParams}</p>`}
    <button type="button" class="link-btn mcp-raw" aria-expanded=${raw} onClick=${() => setRaw((v) => !v)}>${raw ? t.mcpHideJson : t.mcpShowJson}</button>
    ${raw ? html`<pre class="mcp-json">${JSON.stringify(schema ?? {}, null, 2)}</pre>` : null}
  </div>`;
}

function ToolRow({ tool, off, canToggle, busy, onToggle }) {
  const [open, setOpen] = useState(false);
  return html`<li class=${cx('mcp-tool-row', off && 'is-off')}>
    <div class="mcp-tool-line">
      <button type="button" class=${cx('mcp-tool-open', open && 'is-open')} aria-expanded=${open} onClick=${() => setOpen((v) => !v)}>
        <span class="pf-chev"><${IconChevronRight} size=${12} /></span>
        <span class="mcp-tool-text">
          <span class="mono mcp-tool-name">${tool.rawName || tool.name}</span>
          ${tool.description ? html`<span class="mcp-tool-desc">${tool.description}</span>` : null}
        </span>
      </button>
      <span title=${canToggle ? '' : t.mcpSettingsUnsupported}>
        <${Toggle} on=${!off} label=${t.mcpToolFor(tool.rawName || tool.name)} disabled=${!canToggle || busy} onChange=${onToggle} /></span>
    </div>
    ${open ? html`<${Schema} schema=${tool.inputSchema} />` : null}
  </li>`;
}

function ToolsTab({ s, settings }) {
  const [query, setQuery] = useState('');
  const tools = s.tools ?? [];
  const disabled = new Set(s.disabledTools ?? []);
  const canToggle = settings.supported && !settings.checking && s.disabledTools !== undefined;
  const q = query.trim().toLowerCase();
  const shown = q ? tools.filter((x) => [x.rawName, x.description].some((v) => v?.toLowerCase().includes(q))) : tools;
  const set = (names) => settings.change(s, { disabledTools: [...names].sort() });
  if (!tools.length) {
    // The drawer shows the error above the tabs.
    return html`<div class="dr-empty"><p>${s.status === 'connected' ? t.mcpNoTools : t.mcpToolsWhenConnected}</p></div>`;
  }
  return html`<div class="mcp-tools-tab">
    <div class="mcp-tools-bar">
      ${tools.length > 8 ? html`<label class="xe-search">
        <${IconSearch} size=${14} />
        <input type="search" value=${query} placeholder=${t.mcpSearchTools} aria-label=${t.mcpSearchTools} onInput=${(e) => setQuery(e.currentTarget.value)} />
      </label>` : html`<span class="spacer" />`}
      <span class="mcp-tools-count">${t.mcpToolsActive(tools.length - tools.filter((x) => disabled.has(x.rawName)).length, tools.length)}</span>
      ${canToggle ? html`<button type="button" class="link-btn" disabled=${!disabled.size} onClick=${() => set([])}>${t.mcpAllOn}</button>
        <button type="button" class="link-btn" onClick=${() => set(tools.map((x) => x.rawName))}>${t.mcpAllOff}</button>` : null}
    </div>
    ${!canToggle && !settings.checking ? html`<p class="dr-note">${t.mcpToolSwitchesLater}</p>` : null}
    <ul class="mcp-tool-list">${shown.map((tool) => html`<${ToolRow} key=${tool.name} tool=${tool} off=${disabled.has(tool.rawName)}
      canToggle=${canToggle} busy=${settings.busy === s.name}
      onToggle=${(on) => { const next = new Set(disabled); if (on) next.delete(tool.rawName); else next.add(tool.rawName); set(next); }} />`)}</ul>
    ${q && !shown.length ? html`<p class="sk-note">${t.mcpNoToolMatch(quote(query.trim()))}</p>` : null}
  </div>`;
}

// ---- drawer: settings, log, sign-in ---------------------------------------------------------

function SettingsTab({ s, onSaved }) {
  const [form, setForm] = useState(() => formOf(s));
  const [errors, setErrors] = useState({});
  const [saving, setSaving] = useState(false);
  const probe = useProbe();
  const readOnly = s.origin === 'plugin';
  const body = bodyOf(form);
  const changed = JSON.stringify(body) !== JSON.stringify(bodyOf(formOf(s)));
  const check = () => {
    const e = validate(form, { editing: true });
    setErrors(e);
    return !Object.keys(e).length;
  };
  const save = async () => {
    if (!check()) return;
    setSaving(true);
    try {
      const saved = await api.updateMcpServer(s.name, body);
      toast(t.mcpSaved(s.name));
      setForm(formOf(saved));
      onSaved();
    } catch (e) { fail(e); } finally { setSaving(false); }
  };
  return html`<div class="mcp-settings">
    ${readOnly ? html`<p class="dr-note">${t.mcpPluginOwned(s.plugin)}</p>` : null}
    <${ServerForm} form=${form} onChange=${(f) => { setForm(f); setErrors({}); }} editing readOnly=${readOnly} errors=${errors} />
    <${ProbeResult} result=${probe.result} busy=${probe.busy} elapsed=${probe.elapsed} stale=${probe.staleFor(body)} />
    ${readOnly ? null : html`<div class="mcp-settings-actions">
      <button type="button" class="btn btn-small" disabled=${probe.busy} onClick=${() => check() && probe.run(body)}><${IconPlug} size=${14} />${t.mcpTest}</button>
      <span class="spacer" />
      <button type="button" class="btn btn-small" disabled=${!changed || saving} onClick=${() => { setForm(formOf(s)); setErrors({}); probe.reset(); }}>${t.mcpReset}</button>
      <button type="button" class="btn btn-small btn-primary" disabled=${!changed || saving} onClick=${save}>${saving ? html`<span class="spinner" />` : null}${t.save}</button>
    </div>`}
  </div>`;
}

function LogTab({ name }) {
  const [lines, setLines] = useState(null);
  const [failed, setFailed] = useState(false);
  const box = useRef(null);
  const stick = useRef(true);
  useEffect(() => {
    let live = true;
    const load = () => api.mcpServerLog(name).then(
      (r) => { if (live) { setLines(r?.lines ?? []); setFailed(false); } },
      () => { if (live) setFailed(true); },
    );
    load();
    const id = setInterval(load, 2000);
    return () => { live = false; clearInterval(id); };
  }, [name]);
  useEffect(() => {
    const el = box.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [lines]);
  if (failed && !lines) return html`<div class="dr-empty"><p>${t.mcpLogFailed}</p></div>`;
  if (!lines) return html`<div class="dr-loading"><span class="spinner" />${t.loading}</div>`;
  if (!lines.length) return html`<div class="dr-empty"><p>${t.mcpLogEmpty}</p></div>`;
  const text = lines.map((l) => `${new Date(l.time).toISOString()} ${l.kind} ${l.text}`).join('\n');
  return html`<div class="mcp-log-tab">
    <div class="mcp-log-bar"><span class="plug-meta">${t.mcpLogLead(lines.length)}</span><span class="spacer" />
      <button type="button" class="btn btn-small" onClick=${() => copyText(text).then(() => toast(t.copied))}><${IconCopy} size=${13} />${t.copy}</button></div>
    <div ref=${box} class="mcp-log" role="log" aria-live="polite" tabindex="0"
      onScroll=${(e) => { const el = e.currentTarget; stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24; }}>
      ${lines.map((l, i) => html`<div key=${i} class=${`mcp-log-line is-${l.kind}`}>
        <span class="mcp-log-time">${clock(l.time)}</span><span class="mcp-log-kind">${t.mcpLogKinds[l.kind] ?? l.kind}</span><span class="mcp-log-text">${l.text}</span>
      </div>`)}
    </div>
  </div>`;
}

function expiry(ms) {
  const left = ms - Date.now();
  const date = new Date(ms).toLocaleString([], { dateStyle: 'medium', timeStyle: 'short' });
  if (left <= 0) return t.mcpExpired(date);
  const h = left / 3600000;
  return t.mcpExpires(date, h < 1 ? `${Math.max(1, Math.round(left / 60000))} min` : h < 48 ? `${Math.round(h)} h` : `${Math.round(h / 24)} d`);
}

function AuthTab({ s, run, onLogin }) {
  const [token, setToken] = useState('');
  const login = s.auth?.login;
  const why = s.secrets?.bearerToken ? t.mcpAuthByToken : s.bearerTokenEnvVar ? t.mcpAuthByEnv(s.bearerTokenEnvVar)
    : s.headers?.length ? t.mcpAuthByHeaders : t.mcpAuthOff;
  if (!s.auth?.oauth) {
    return html`<div class="mcp-auth"><div class="mcp-auth-card"><div class="mcp-auth-title">${t.mcpNoOauth}</div><p class="dr-note">${why}</p></div></div>`;
  }
  const saveToken = (e) => {
    e.preventDefault();
    run(s.name, async () => { await api.mcpToken(s.name, token.trim()); setToken(''); });
  };
  return html`<div class="mcp-auth">
    <div class="mcp-auth-card">
      <div class="mcp-auth-title">${login ? (login.kind === 'token' ? t.mcpSignedInToken : t.mcpSignedIn) : t.mcpSignedOut}
        <${StatusDot} status=${s.status} /></div>
      ${login ? html`<dl class="mcp-auth-facts">
        ${login.issuer ? html`<dt>${t.mcpIssuer}</dt><dd class="mono">${login.issuer}</dd>` : null}
        ${login.scope ? html`<dt>${t.mcpScope}</dt><dd class="mono">${login.scope}</dd>` : null}
        ${login.expiresAt ? html`<dt>${t.mcpExpiry}</dt><dd>${expiry(login.expiresAt)}${login.refreshable ? html`<span class="plug-meta"> · ${t.mcpRefreshable}</span>` : null}</dd>` : null}
      </dl>` : html`<p class="dr-note">${s.challenge?.error === 'insufficient_scope' ? t.mcpNeedsScope : t.mcpSignInHint}</p>`}
      <div class="mcp-auth-actions">
        <button type="button" class="btn btn-small btn-primary" onClick=${() => run(s.name, () => onLogin(s.name))}>${login ? t.reconnect : t.mcpSignIn}</button>
        ${login ? html`<button type="button" class="btn btn-small" onClick=${() => run(s.name, () => api.mcpAction(s.name, 'logout'))}>${t.disconnect}</button>` : null}
      </div>
    </div>
    <form class="mcp-auth-card" onSubmit=${saveToken}>
      <div class="mcp-auth-title">${t.mcpManualToken}</div>
      <p class="dr-note">${t.mcpManualTokenHint}</p>
      <div class="md-inline">
        <input class="input mono" type="password" autocomplete="off" value=${token} placeholder=${t.tokenLabel} aria-label=${t.tokenLabel}
          onInput=${(e) => setToken(e.currentTarget.value)} />
        <button type="submit" class="btn btn-small" disabled=${!token.trim()}>${t.tokenSave}</button>
      </div>
    </form>
  </div>`;
}

/** The drawer of one server; the Apps tab opens it too. `tab` picks the first tab. */
export function ServerDrawer({ name, tab: first, connectors, onClose, onChanged, onLogin }) {
  const detail = useLoad(() => api.mcpServer(name), [name]);
  const changed = () => { detail.reload(); onChanged?.(); };
  const [busy, run] = useAction({ reload: changed });
  const settings = useMcpSettings(changed);
  const s = detail.data;
  const [tab, setTab] = useState(first ?? 'tools');
  // Follow a connection in progress.
  useEffect(() => {
    if (!s || !['connecting', 'needs-login'].includes(s.status) && !(s.status === 'error' && s.retrying)) return undefined;
    const id = setTimeout(detail.reload, s.status === 'connecting' ? 1200 : 4000);
    return () => clearTimeout(id);
  }, [s]);
  if (!s) {
    const head = html`<div class="dr-titles"><h2 class="dr-title">${name}</h2></div>`;
    return html`<${Drawer} label=${name} head=${head} onClose=${onClose}>
      ${detail.error ? html`<div class="dr-error">${detail.error.status === 404 ? t.mcpGone : t.mcpDetailFailed}
        <button type="button" class="link-btn" onClick=${detail.reload}>${t.retry}</button></div>`
    : html`<div class="dr-loading"><span class="spinner" />${t.loading}</div>`}
    </${Drawer}>`;
  }
  const remote = isRemote(s);
  const tabs = { tools: t.mcpTabs.tools, settings: t.mcpTabs.settings, log: t.mcpTabs.log, ...(remote ? { auth: t.mcpTabs.auth } : {}) };
  const head = html`<${ServerLogo} s=${s} connectors=${connectors} size=${40} />
    <div class="dr-titles">
      <h2 class="dr-title">${s.name}</h2>
      <div class="dr-meta"><${Pill} tone="off">${remote ? t.mcpRemote : t.mcpLocal}</${Pill}><${StatusDot} status=${s.status} retrying=${s.retrying} />
        ${s.status === 'connected' ? html`<span class="plug-meta">${toolCount(s)}</span>` : null}</div>
    </div>`;
  const actions = html`${s.enabled ? html`<button type="button" class="icon-btn" aria-label=${t.restart} title=${t.restart} disabled=${busy === s.name}
      onClick=${() => run(s.name, () => api.mcpAction(s.name, 'restart'))}><${IconRefresh} size=${15} /></button>` : null}
    <${MoreMenu} items=${[
    s.auth?.login ? [null, t.disconnect, () => run(s.name, () => api.mcpAction(s.name, 'logout'))] : null,
    [html`<${IconCopy} size=${14} />`, t.mcpCopyName, () => copyText(s.name).then(() => toast(t.copied))],
    s.origin === 'plugin' ? null : [html`<${IconTrash} size=${14} />`, t.remove, async () => {
      if (await ask({ title: t.confirmRemoveServer(s.name), action: t.remove })) run(s.name, async () => { await api.removeMcpServer(s.name); onClose(); });
    }, 'danger'],
  ]} />`;
  return html`<${Drawer} label=${s.name} head=${head} actions=${actions} tabs=${tabs} tab=${tabs[tab] ? tab : 'tools'} onTab=${setTab} onClose=${onClose}>
    ${s.error && s.status !== 'connected' && tab !== 'log' ? html`<div class="dr-alert"><${IconAlert} size=${14} /><span>${s.error}</span>
      <button type="button" class="link-btn" onClick=${() => setTab('log')}>${t.mcpShowLog}</button></div>` : null}
    ${tab === 'settings' ? html`<${SettingsTab} key=${s.name} s=${s} onSaved=${changed} />`
    : tab === 'log' ? html`<${LogTab} name=${s.name} />`
      : tab === 'auth' && remote ? html`<${AuthTab} s=${s} run=${run} onLogin=${onLogin} />`
        : html`<${ToolsTab} s=${s} settings=${settings} />`}
  </${Drawer}>`;
}

// ---- add server -----------------------------------------------------------------------------

function Choose({ onPick }) {
  const choices = [
    ['catalog', t.mcpFromCatalog, t.mcpFromCatalogHint],
    ['custom', t.mcpCustom, t.mcpCustomHint],
    ['json', t.mcpPasteJson, t.mcpPasteJsonHint],
  ];
  return html`<div class="mo-catalog">${choices.map(([k, title, hint]) => html`<button type="button" key=${k} class="mo-choice" onClick=${() => onPick(k)}>
    <span class="mo-choice-text"><span class="mo-choice-title">${title}</span><span class="mo-choice-hint">${hint}</span></span>
    <${IconChevronRight} size=${14} />
  </button>`)}</div>`;
}

function Catalog({ connectors, onAdded, onLogin }) {
  const [query, setQuery] = useState('');
  const [busy, setBusy] = useState(null);
  const [asking, setAsking] = useState(null);
  const [token, setToken] = useState('');
  const q = query.trim().toLowerCase();
  const list = (connectors ?? []).filter((c) => !q || [c.name, c.description, c.category].some((v) => v?.toLowerCase().includes(q)));
  const add = async (c) => {
    setBusy(c.id);
    try {
      const server = c.servers?.[0] ?? (await api.addMcpServer({ connector: c.id }))?.name;
      if (c.auth === 'token') { setAsking({ c, server }); return; }
      if (c.auth === 'oauth') await onLogin(server);
      onAdded(server);
    } catch (e) { fail(e); } finally { setBusy(null); }
  };
  const saveToken = async (e) => {
    e.preventDefault();
    setBusy(asking.c.id);
    try { await api.mcpToken(asking.server, token.trim()); onAdded(asking.server); } catch (err) { fail(err); } finally { setBusy(null); }
  };
  return html`<div class="mcp-catalog">
    <label class="xe-search">
      <${IconSearch} size=${14} />
      <input type="search" value=${query} placeholder=${t.mcpSearchCatalog} aria-label=${t.mcpSearchCatalog} onInput=${(e) => setQuery(e.currentTarget.value)} />
    </label>
    <ul class="mcp-catalog-list">${list.map((c) => html`<li key=${c.id} class="mcp-catalog-row">
      <${Logo} color=${c.brandColor} name=${c.name} size=${32} />
      <span class="mcp-catalog-text">
        <span class="mcp-catalog-name">${c.name}${c.category ? html`<span class="plug-meta">${c.category}</span>` : null}</span>
        <span class="plug-desc">${c.description}</span>
        ${asking?.c.id === c.id ? html`<form class="md-inline mcp-catalog-token" onSubmit=${saveToken}>
          <input class="input mono" type="password" autocomplete="off" value=${token} placeholder=${t.tokenLabel} aria-label=${t.tokenLabel}
            ref=${(el) => el && !el.dataset.f && (el.dataset.f = '1', el.focus())} onInput=${(e) => setToken(e.currentTarget.value)} />
          <button type="submit" class="btn btn-small btn-primary" disabled=${!token.trim() || busy === c.id}>${t.tokenSave}</button>
        </form>${c.tokenHelp ? html`<span class="plug-meta">${c.tokenHelp}</span>` : null}` : null}
      </span>
      ${c.added && c.status === 'connected' ? html`<${Pill} tone="ok">${t.connected}</${Pill}>`
    : html`<button type="button" class="btn btn-small" disabled=${busy === c.id} onClick=${() => add(c)}>
          ${busy === c.id ? html`<span class="spinner" />` : null}${c.added ? (c.auth === 'none' ? t.mcpOpen : t.reconnect) : t.add}</button>`}
    </li>`)}</ul>
    ${!list.length ? html`<p class="sk-note">${t.mcpNoCatalogMatch}</p>` : null}
  </div>`;
}

const WARNINGS = { sse: () => t.mcpWarnSse, input: () => t.mcpWarnInput, command: () => t.mcpWarnCommand };

function PasteJson({ existing, onEdit, onAddMany }) {
  const [text, setText] = useState('');
  const parsed = useMemo(() => parseServers(text), [text]);
  const [picked, setPicked] = useState({});
  const servers = parsed.servers;
  const taken = (f) => existing.includes(f.name);
  const isOn = (i) => picked[i] ?? !taken(servers[i].form);
  const chosen = servers.filter((_, i) => isOn(i));
  return html`<div class="mcp-json-step">
    <${Field} label=${t.mcpJsonLabel} hint=${t.mcpJsonHint} wide>
      <textarea class="input mono mcp-json-input" rows="9" value=${text} spellcheck=${false} placeholder=${t.mcpJsonPlaceholder}
        onInput=${(e) => { setText(e.currentTarget.value); setPicked({}); }} />
    </${Field}>
    ${text.trim() && parsed.error ? html`<p class="md-error" role="alert">${t.mcpJsonUnreadable}</p>` : null}
    ${servers.length ? html`<div class="mcp-found">
      <div class="mcp-found-head">${t.mcpFound(servers.length)}</div>
      <ul class="mcp-found-list">${servers.map(({ form, warnings }, i) => html`<li key=${i} class="mcp-found-row">
        ${servers.length > 1 ? html`<input type="checkbox" checked=${isOn(i)} aria-label=${form.name}
          onChange=${(e) => setPicked({ ...picked, [i]: e.currentTarget.checked })} />` : null}
        <span class="mcp-found-text">
          <span class="mcp-found-name"><span class="mono">${form.name}</span><${Pill} tone="off">${form.type === 'http' ? t.mcpRemote : t.mcpLocal}</${Pill}>
            ${taken(form) ? html`<${Pill} tone="warn">${t.mcpNameTaken}</${Pill}>` : null}</span>
          <span class="plug-meta mono">${form.url || [form.command, ...(form.args ?? [])].join(' ')}</span>
          ${(form.env?.length || form.headers?.length) ? html`<span class="plug-meta">${t.mcpFoundSecrets((form.env?.length ?? 0) + (form.headers?.length ?? 0))}</span>` : null}
          ${warnings.map((w) => html`<span class="plug-warn" key=${w}>${WARNINGS[w]?.()}</span>`)}
        </span>
        <button type="button" class="btn btn-small" onClick=${() => onEdit(form)}>${servers.length > 1 ? t.mcpEdit : t.mcpContinue}</button>
      </li>`)}</ul>
      ${servers.length > 1 ? html`<div class="mcp-found-actions">
        <button type="button" class="btn btn-small btn-primary" disabled=${!chosen.length || chosen.some((x) => taken(x.form))}
          onClick=${() => onAddMany(chosen.map((x) => x.form))}>${t.mcpAddChosen(chosen.length)}</button>
      </div>` : null}
    </div>` : null}
  </div>`;
}

/**
 * "Add server": choose a way, then the catalog, the custom form or pasted
 * JSON. `onAdded(name)` gets the new server's name.
 */
export function AddServerDialog({ existing, connectors, onClose, onAdded, onLogin }) {
  const [step, setStep] = useState('choose');
  const [form, setForm] = useState(emptyForm);
  const [errors, setErrors] = useState({});
  const [saving, setSaving] = useState(false);
  const [untested, setUntested] = useState(false);
  const probe = useProbe();
  const body = bodyOf(form);
  const edit = (f) => { setForm(fromImport(f)); setErrors({}); probe.reset(); setStep('custom'); };
  const check = () => {
    const e = validate(form);
    if (!e.name && existing.includes(form.name.trim())) e.name = t.mcpNameTaken;
    setErrors(e);
    return !Object.keys(e).length;
  };
  const save = async () => {
    if (!check()) return;
    if (!probe.okFor(body) && !untested) { setUntested(true); return; }
    setSaving(true);
    try {
      const added = await api.addMcpServer(body);
      toast(t.mcpAdded(added.name));
      onAdded(added.name);
    } catch (e) { fail(e); } finally { setSaving(false); }
  };
  const addMany = async (forms) => {
    setSaving(true);
    const done = [];
    for (const f of forms) {
      try { done.push((await api.addMcpServer(bodyOf(fromImport(f)))).name); } catch (e) { toast(`${f.name}: ${e?.message ?? e}`); }
    }
    setSaving(false);
    if (done.length) { toast(t.mcpAddedMany(done.length)); onAdded(done[0]); }
  };
  const titles = { choose: t.addServer, catalog: t.mcpFromCatalog, custom: t.mcpCustom, json: t.mcpPasteJson };
  const warn = untested && !probe.okFor(body);
  const back = step === 'choose' ? null : () => { setStep('choose'); setUntested(false); };
  const footer = step === 'custom' ? html`
    ${warn ? html`<span class="mf-untested" role="alert"><${IconAlert} size=${13} />${t.mcpUntested}</span>` : null}
    <button type="button" class="btn btn-small" disabled=${probe.busy} onClick=${() => check() && probe.run(body)}><${IconPlug} size=${14} />${t.mcpTest}</button>
    <button type="button" class="btn btn-small btn-primary" disabled=${saving} onClick=${save}>
      ${saving ? html`<span class="spinner" />` : null}${warn ? t.mcpAddAnyway : t.add}</button>` : null;
  return html`<${Dialog} title=${titles[step]} description=${step === 'choose' ? t.mcpAddLead : null} wide=${step !== 'choose'}
    onClose=${onClose} back=${back} footer=${footer} step=${step}>
    ${step === 'choose' ? html`<${Choose} onPick=${(k) => { if (k === 'custom') { setForm(emptyForm()); probe.reset(); } setStep(k); }} />`
    : step === 'catalog' ? html`<${Catalog} connectors=${connectors} onAdded=${onAdded} onLogin=${onLogin} />`
      : step === 'json' ? html`<${PasteJson} existing=${existing} onEdit=${edit} onAddMany=${addMany} />`
        : html`<form onSubmit=${(e) => { e.preventDefault(); save(); }}>
          <${ServerForm} form=${form} onChange=${(f) => { setForm(f); setErrors({}); setUntested(false); }} errors=${errors} />
          <${ProbeResult} result=${probe.result} busy=${probe.busy} elapsed=${probe.elapsed} stale=${probe.staleFor(body)} />
          <button type="submit" hidden />
        </form>`}
  </${Dialog}>`;
}

// ---- the tab ----------------------------------------------------------------------------------

export function McpTab() {
  const servers = useLoad(() => api.mcpServers());
  const conns = useLoad(() => api.connectors().catch(() => []));
  const [busy, run] = useAction(servers);
  const settings = useMcpSettings(servers.reload);
  const { waiting, login } = useLoginWait(() => servers.reload());
  const [opened, setOpened] = useState(null);
  const [adding, setAdding] = useState(false);
  const list = servers.data ?? [];
  // Follow servers that are still connecting.
  useEffect(() => {
    if (!list.some((s) => s.status === 'connecting')) return undefined;
    const id = setTimeout(servers.reload, 1500);
    return () => clearTimeout(id);
  }, [servers.data]);
  const open = (name, tab) => setOpened({ name, tab });
  const onLogin = (name) => login(name);
  // An @server pill or an active-server chip; names compare like the composer's.
  usePluginFocus('server', servers, (name) => {
    const hit = list.find((s) => s.name.toLowerCase() === name.toLowerCase());
    if (hit) open(hit.name);
    return !!hit;
  });

  let body;
  if (servers.loading && !servers.data) body = html`<${CardSkeleton} />`;
  else if (servers.error && !servers.data) {
    body = html`<div class="page-empty xe-empty"><p>${t.mcpLoadFailed}</p>
      <button type="button" class="btn btn-small" onClick=${servers.reload}><${IconRefresh} size=${14} />${t.retry}</button></div>`;
  } else if (!list.length) {
    body = html`<div class="page-empty xe-empty"><p class="xe-empty-title">${t.noServers}</p><p>${t.mcpEmptyHint}</p>
      <button type="button" class="btn btn-small btn-primary" onClick=${() => setAdding(true)}><${IconPlus} size=${14} />${t.addServer}</button></div>`;
  } else {
    body = html`<div class="plug-grid mcp-grid">${list.map((s) => html`<${ServerCard} key=${s.name} s=${s} connectors=${conns.data}
      busy=${busy ?? (waiting?.server === s.name ? s.name : null)} run=${run} settings=${settings} onOpen=${open} onLogin=${onLogin} />`)}</div>`;
  }
  return html`<div class="mcp-page">
    <div class="xe-toolbar">
      <p class="plug-meta mcp-lead">${t.mcpLead}</p>
      <button type="button" class="btn btn-small btn-primary" onClick=${() => setAdding(true)}><${IconPlus} size=${14} />${t.addServer}</button>
    </div>
    ${body}
    ${opened ? html`<${ServerDrawer} key=${opened.name} name=${opened.name} tab=${opened.tab} connectors=${conns.data}
      onClose=${() => setOpened(null)} onChanged=${servers.reload} onLogin=${onLogin} />` : null}
    ${adding ? html`<${AddServerDialog} existing=${list.map((s) => s.name)} connectors=${conns.data} onClose=${() => setAdding(false)}
      onLogin=${onLogin} onAdded=${(name) => { setAdding(false); servers.reload(); conns.reload(); open(name); }} />` : null}
  </div>`;
}
