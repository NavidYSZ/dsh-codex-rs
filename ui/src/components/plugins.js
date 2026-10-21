// Plugins page, like the Codex app's: installed plugins and marketplaces,
// connectors with sign-in, skills and MCP servers. Changes apply at once;
// open chats see new tools and skills with their next prompt.
import { useEffect, useRef, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t } from '../i18n.js';
import { useStore } from '../store.js';
import { store, api, fail, toast } from '../state.js';
import { IconMore, IconRefresh, IconTrash, IconPlus, IconChevronRight } from '../icons.js';
import { Menu, ActionItem } from './common.js';
import { PageShell, Tabs, useLoad, Toggle, Pill } from './page.js';
import { workspaceName } from './composer.js';

/** Runs one change, then reloads; `busy` names the change while it runs. */
function useAction(...reloads) {
  const [busy, setBusy] = useState(null);
  const run = async (key, fn) => {
    setBusy(key);
    try { return await fn(); } catch (e) { fail(e); return null; } finally {
      setBusy(null);
      for (const r of reloads) r.reload();
      api.info().then((info) => store.set({ info })).catch(() => {});
    }
  };
  return [busy, run];
}

function Logo({ url, color, name }) {
  const [broken, setBroken] = useState(false);
  if (url && !broken) return html`<img class="plug-logo" src=${api.assetUrl(url)} alt="" onError=${() => setBroken(true)} />`;
  return html`<span class="plug-logo plug-initial" style=${color ? `background:${color}` : undefined}>${(name || '?').slice(0, 1).toUpperCase()}</span>`;
}

function MoreMenu({ items }) {
  const [open, setOpen] = useState(false);
  const anchor = useRef(null);
  return html`<span class="chip-wrap">
    <button type="button" ref=${anchor} class="icon-btn" aria-label=${t.more} aria-haspopup="menu" onClick=${() => setOpen((v) => !v)}>
      <${IconMore} size=${15} />
    </button>
    <${Menu} open=${open} onClose=${() => setOpen(false)} anchor=${anchor} fixed side="bottom" align="end" class="menu-actions">
      ${items.map(([icon, label, fn, tone]) => html`<${ActionItem} key=${label} icon=${icon} tone=${tone} onClick=${fn}>${label}</${ActionItem}>`)}
    </${Menu}>
  </span>`;
}

// ---- plugins and marketplaces -------------------------------------------------

function PluginCard({ p, busy, run }) {
  const ui = p.interface ?? {};
  const name = ui.displayName || p.name;
  const problems = [p.error, ...(p.warnings ?? [])].filter(Boolean);
  return html`<div class=${cx('plug-card', !p.enabled && 'is-off')}>
    <${Logo} url=${p.logoUrl} color=${ui.brandColor} name=${name} />
    <div class="plug-text">
      <div class="plug-name">${name}<span class="plug-meta">${p.marketplace}${p.version ? ` · ${p.version}` : ''}</span></div>
      <div class="plug-desc">${ui.shortDescription || p.description || ''}</div>
      ${p.skills?.length || p.mcpServers?.length ? html`<div class="plug-parts">
        ${p.skills?.length ? html`<span>${p.skills.length} Skills</span>` : null}
        ${p.mcpServers?.length ? html`<span>${p.mcpServers.length} MCP</span>` : null}
      </div>` : null}
      ${problems.length ? html`<div class="plug-warn" title=${problems.join('\n')}>${problems[0]}</div>` : null}
    </div>
    <div class="plug-side">
      <${Toggle} on=${p.enabled} label=${name} disabled=${busy === p.id}
        onChange=${(on) => run(p.id, () => api.pluginAction(p.id, on ? 'enable' : 'disable'))} />
      <${MoreMenu} items=${[
        [html`<${IconRefresh} size=${14} />`, t.update, () => run(p.id, () => api.pluginAction(p.id, 'update'))],
        [html`<${IconTrash} size=${14} />`, t.remove, () => { if (confirm(t.confirmRemovePlugin(name))) run(p.id, () => api.removePlugin(p.id)); }, 'danger'],
      ]} />
    </div>
  </div>`;
}

function MarketPlugin({ m, e, busy, run }) {
  const ui = e.interface ?? {};
  const name = ui.displayName || e.name;
  const unsupported = e.source?.kind === 'unsupported';
  return html`<div class="plug-card">
    <${Logo} color=${ui.brandColor} name=${name} />
    <div class="plug-text">
      <div class="plug-name">${name}${e.version ? html`<span class="plug-meta">${e.version}</span>` : null}</div>
      <div class="plug-desc">${ui.shortDescription || e.description || ''}</div>
    </div>
    <div class="plug-side">
      ${e.installed ? html`<${Pill} tone="off">${t.installed}</${Pill}>`
    : html`<button type="button" class="btn btn-small" disabled=${unsupported || busy === `${m.name}/${e.name}`}
          title=${unsupported ? (e.source?.reason || '') : ''}
          onClick=${() => run(`${m.name}/${e.name}`, () => api.installPlugin(m.name, e.name))}>${t.install}</button>`}
    </div>
  </div>`;
}

function PluginsTab() {
  const plugins = useLoad(() => api.plugins());
  const markets = useLoad(() => api.marketplaces());
  const [busy, run] = useAction(plugins, markets);
  const [source, setSource] = useState('');
  const add = async (e) => {
    e.preventDefault();
    const raw = source.trim();
    if (!raw) return;
    const [src, ref] = raw.split('#');
    if (await run('market', () => api.addMarketplace(src, ref))) setSource('');
  };
  const installed = plugins.data ?? [];
  return html`
    <section class="page-section">
      <h2 class="section-title">${t.installed}</h2>
      ${installed.length ? html`<div class="plug-grid">${installed.map((p) => html`<${PluginCard} key=${p.id} p=${p} busy=${busy} run=${run} />`)}</div>`
    : html`<div class="page-empty">${plugins.loading ? t.loading : t.noPlugins}</div>`}
    </section>
    ${(markets.data ?? []).map((m) => html`<section class="page-section" key=${m.name}>
      <div class="section-head">
        <h2 class="section-title"><span class="section-name">${m.displayName || m.name}</span><span class="plug-meta" title=${m.url || m.source}>${m.url || m.source}</span></h2>
        <${MoreMenu} items=${[
          [html`<${IconRefresh} size=${14} />`, t.update, () => run('market', () => api.updateMarketplace(m.name))],
          [html`<${IconTrash} size=${14} />`, t.remove, () => { if (confirm(t.confirmRemoveMarket(m.name))) run('market', () => api.removeMarketplace(m.name)); }, 'danger'],
        ]} />
      </div>
      ${m.error ? html`<div class="plug-warn">${m.error}</div>` : null}
      <div class="plug-grid">${(m.plugins ?? []).map((e) => html`<${MarketPlugin} key=${e.id} m=${m} e=${e} busy=${busy} run=${run} />`)}</div>
    </section>`)}
    <form class="page-form" onSubmit=${add}>
      <input class="input" value=${source} onInput=${(e) => setSource(e.currentTarget.value)} placeholder=${t.marketplaceHint}
        aria-label=${t.addMarketplace} spellcheck="false" />
      <button type="submit" class="btn btn-small" disabled=${!source.trim() || busy === 'market'}><${IconPlus} size=${14} />${t.addMarketplace}</button>
    </form>`;
}

// ---- connectors ----------------------------------------------------------------

const STATUS = {
  connected: ['ok', 'connected'], 'needs-login': ['warn', 'needsLogin'], connecting: ['busy', 'connecting'],
  error: ['error', 'statusError'], disabled: ['off', 'statusDisabled'], stopped: ['off', 'statusStopped'],
};
const statusPill = (status) => {
  const [tone, key] = STATUS[status] ?? ['off', 'notAdded'];
  return html`<${Pill} tone=${tone}>${t[key]}</${Pill}>`;
};

/** Opens the sign-in page; the Mac app hands it to the default browser. */
function openExternal(url) {
  window.open(url, '_blank', 'noopener');
}

function ConnectorCard({ c, busy, run, waiting, onLogin }) {
  const [token, setToken] = useState('');
  const [askToken, setAskToken] = useState(false);
  const server = c.servers?.[0];
  const status = waiting ? 'connecting' : c.added ? c.status : null;
  const ensure = async () => server || (await api.addMcpServer({ connector: c.id }))?.name;
  const connect = () => run(c.id, async () => {
    if (c.auth === 'token') { setAskToken(true); return; }
    const name = await ensure();
    if (c.auth === 'oauth' && name) onLogin(c.id, name);
  });
  const saveToken = (e) => {
    e.preventDefault();
    run(c.id, async () => {
      const name = await ensure();
      await api.mcpToken(name, token.trim());
      setToken('');
      setAskToken(false);
    });
  };
  return html`<div class="plug-card conn-card">
    <${Logo} color=${c.brandColor} name=${c.name} />
    <div class="plug-text">
      <div class="plug-name">${c.name}${c.category ? html`<span class="plug-meta">${c.category}</span>` : null}</div>
      <div class="plug-desc">${c.description}</div>
      ${askToken ? html`<form class="page-form conn-token" onSubmit=${saveToken}>
        <input class="input mono" type="password" autocomplete="off" value=${token} placeholder=${t.tokenLabel}
          aria-label=${t.tokenLabel} onInput=${(e) => setToken(e.currentTarget.value)} />
        <button type="submit" class="btn btn-small btn-primary" disabled=${!token.trim()}>${t.tokenSave}</button>
      </form>${c.tokenHelp ? html`<div class="plug-meta">${c.tokenHelp}</div>` : null}` : null}
    </div>
    <div class="plug-side">
      ${status ? statusPill(status) : null}
      ${!c.added || status === 'needs-login'
    ? html`<button type="button" class="btn btn-small" disabled=${busy === c.id || waiting} onClick=${connect}>
          ${c.added ? t.reconnect : t.connect}</button>`
    : html`<${MoreMenu} items=${[
      ...(c.auth !== 'none' ? [[null, t.disconnect, () => run(c.id, () => api.mcpAction(server, 'logout'))]] : []),
      [html`<${IconTrash} size=${14} />`, t.remove, () => run(c.id, () => api.removeMcpServer(server)), 'danger'],
    ]} />`}
    </div>
  </div>`;
}

function ConnectorsTab() {
  const conns = useLoad(() => api.connectors());
  const [busy, run] = useAction(conns);
  const [waiting, setWaiting] = useState(null);
  // After the sign-in page opened: poll until the server is connected (3 min).
  useEffect(() => {
    if (!waiting) return undefined;
    const until = Date.now() + 180_000;
    const id = setInterval(async () => {
      try {
        const servers = await api.mcpServers();
        const s = servers.find((x) => x.name === waiting.server);
        if (s?.status === 'connected' || Date.now() > until) { setWaiting(null); conns.reload(); }
      } catch { /* keep polling */ }
    }, 2000);
    return () => clearInterval(id);
  }, [waiting]);
  const onLogin = async (connector, server) => {
    const r = await api.mcpAction(server, 'login');
    if (r?.authorizationUrl) {
      openExternal(r.authorizationUrl);
      toast(t.loginOpened);
      setWaiting({ connector, server });
    }
  };
  const list = conns.data ?? [];
  return html`<div class="plug-grid">${list.map((c) => html`<${ConnectorCard} key=${c.id} c=${c} busy=${busy} run=${run}
    waiting=${waiting?.connector === c.id} onLogin=${onLogin} />`)}</div>
    ${!list.length ? html`<div class="page-empty">${conns.loading ? t.loading : '–'}</div>` : null}`;
}

// ---- skills and MCP servers --------------------------------------------------------

function SkillsTab() {
  const cwd = useStore(store, (s) => s.cwd);
  const skills = useLoad(() => api.skills(cwd), [cwd]);
  const list = skills.data ?? [];
  if (!list.length) return html`<div class="page-empty">${skills.loading ? t.loading : t.noSkills}</div>`;
  return html`${cwd ? html`<div class="plug-meta page-note">${t.inProject(workspaceName(cwd))}</div>` : null}
    <div class="skill-list">${list.map((s) => html`<div key=${`${s.source}:${s.name}`} class=${cx('skill-row', !s.active && 'is-off')}>
      <div class="plug-text">
        <div class="plug-name mono">${s.name}<span class="plug-meta">${s.plugin || t.skillSource[s.source] || s.source}</span></div>
        <div class="plug-desc">${s.description}</div>
      </div>
    </div>`)}</div>`;
}

/** "npx -y pkg ~/x" or "https://…/mcp" → a `[mcp_servers.x]` table. */
function serverConfig(text) {
  const raw = text.trim();
  if (/^https?:\/\//i.test(raw)) return { url: raw };
  const parts = raw.match(/"[^"]*"|'[^']*'|\S+/g)?.map((p) => p.replace(/^["']|["']$/g, '')) ?? [];
  return { command: parts[0], args: parts.slice(1) };
}

function ServerRow({ s, busy, run }) {
  const [open, setOpen] = useState(false);
  const where = s.url || [s.command, ...(s.args ?? [])].filter(Boolean).join(' ');
  return html`<div class=${cx('mcp-row', !s.enabled && 'is-off')}>
    <div class="mcp-head">
      <button type="button" class=${cx('mcp-toggle', open && 'is-open')} onClick=${() => setOpen((v) => !v)} aria-expanded=${open}>
        <span class="pf-chev"><${IconChevronRight} size=${12} /></span>
        <span class="plug-name mono">${s.name}</span>
        ${s.plugin ? html`<span class="plug-meta">${s.plugin}</span>` : null}
        <span class="plug-meta mcp-where" title=${where}>${where}</span>
      </button>
      ${statusPill(s.status)}
      <span class="plug-meta">${s.tools?.length ?? 0} ${t.tools}</span>
      <${Toggle} on=${s.enabled} label=${s.name} disabled=${busy === s.name}
        onChange=${(on) => run(s.name, () => api.mcpAction(s.name, on ? 'enable' : 'disable'))} />
      <${MoreMenu} items=${[
        [html`<${IconRefresh} size=${14} />`, t.restart, () => run(s.name, () => api.mcpAction(s.name, 'restart'))],
        ...(s.status === 'needs-login' ? [[null, t.reconnect, () => run(s.name, async () => {
          const r = await api.mcpAction(s.name, 'login');
          if (r?.authorizationUrl) { openExternal(r.authorizationUrl); toast(t.loginOpened); }
        })]] : []),
        ...(s.auth?.login ? [[null, t.disconnect, () => run(s.name, () => api.mcpAction(s.name, 'logout'))]] : []),
        ...(s.origin === 'config' ? [[html`<${IconTrash} size=${14} />`, t.remove, () => { if (confirm(t.confirmRemoveServer(s.name))) run(s.name, () => api.removeMcpServer(s.name)); }, 'danger']] : []),
      ]} />
    </div>
    ${s.error ? html`<div class="plug-warn">${s.error}</div>` : null}
    ${open ? html`<div class="mcp-tools">${(s.tools ?? []).map((tool) => html`<div key=${tool.name} class="mcp-tool">
      <span class="mono">${tool.rawName || tool.name}</span><span class="plug-desc">${tool.description}</span></div>`)}</div>` : null}
  </div>`;
}

function McpTab() {
  const servers = useLoad(() => api.mcpServers());
  const [busy, run] = useAction(servers);
  const [name, setName] = useState('');
  const [target, setTarget] = useState('');
  const add = async (e) => {
    e.preventDefault();
    if (await run('add', () => api.addMcpServer({ name: name.trim(), config: serverConfig(target) }))) { setName(''); setTarget(''); }
  };
  const list = servers.data ?? [];
  return html`
    ${list.length ? html`<div class="mcp-list">${list.map((s) => html`<${ServerRow} key=${s.name} s=${s} busy=${busy} run=${run} />`)}</div>`
    : html`<div class="page-empty">${servers.loading ? t.loading : t.noServers}</div>`}
    <form class="page-form" onSubmit=${add}>
      <input class="input mcp-name" value=${name} onInput=${(e) => setName(e.currentTarget.value)} placeholder=${t.serverName}
        aria-label=${t.serverName} spellcheck="false" />
      <input class="input" value=${target} onInput=${(e) => setTarget(e.currentTarget.value)} placeholder=${t.serverCommandHint}
        aria-label=${t.serverCommand} spellcheck="false" />
      <button type="submit" class="btn btn-small" disabled=${!name.trim() || !target.trim() || busy === 'add'}><${IconPlus} size=${14} />${t.addServer}</button>
    </form>`;
}

export function PluginsPage() {
  const [tab, setTab] = useState('plugins');
  return html`<${PageShell} title=${t.plugins} lead=${t.pluginsLead} wide>
    <${Tabs} value=${tab} labels=${t.tabsPlugins} onChange=${setTab} />
    ${tab === 'plugins' ? html`<${PluginsTab} />` : tab === 'connectors' ? html`<${ConnectorsTab} />`
    : tab === 'skills' ? html`<${SkillsTab} />` : html`<${McpTab} />`}
  </${PageShell}>`;
}
