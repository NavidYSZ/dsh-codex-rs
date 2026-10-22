// Plugins page, like the Codex app's: installed plugins and marketplaces,
// apps (connectors with sign-in), skills and MCP servers. Changes apply at
// once; open chats see new tools and skills with their next prompt.
import { useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t } from '../i18n.js';
import { api } from '../state.js';
import { IconRefresh, IconTrash, IconPlus } from '../icons.js';
import { PageShell, Tabs, useLoad, Toggle, Pill } from './page.js';
import { Logo, MoreMenu, useAction, statusPill, useLoginWait, PageToast } from './plugin-kit.js';
import { SkillsTab } from './skills.js';
import { McpTab, ServerDrawer } from './mcp.js';

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

// ---- apps (connectors) ---------------------------------------------------------------

function ConnectorCard({ c, busy, run, waiting, onLogin, onOpen }) {
  const [token, setToken] = useState('');
  const [askToken, setAskToken] = useState(false);
  const server = c.servers?.[0];
  const status = waiting ? 'connecting' : c.added ? c.status : null;
  const ensure = async () => server || (await api.addMcpServer({ connector: c.id }))?.name;
  const connect = () => run(c.id, async () => {
    if (c.auth === 'token') { setAskToken(true); return; }
    const name = await ensure();
    if (c.auth === 'oauth' && name) await onLogin(name, c.id);
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
  const text = html`<${Logo} color=${c.brandColor} name=${c.name} />
    <span class="plug-text">
      <span class="plug-name">${c.name}${c.category ? html`<span class="plug-meta">${c.category}</span>` : null}</span>
      <span class="plug-desc">${c.description}</span>
    </span>`;
  return html`<div class="plug-card conn-card">
    ${server ? html`<button type="button" class="conn-main" onClick=${() => onOpen(server)} aria-label=${`${c.name}: ${t.mcpDetails}`}>${text}</button>`
    : html`<div class="conn-main">${text}</div>`}
    <div class="plug-side">
      ${status ? statusPill(status) : null}
      ${!c.added || status === 'needs-login'
    ? html`<button type="button" class="btn btn-small" disabled=${busy === c.id || waiting} onClick=${connect}>
          ${c.added ? t.reconnect : t.connect}</button>`
    : html`<${MoreMenu} items=${[
      [null, t.mcpDetails, () => onOpen(server)],
      c.auth !== 'none' ? [null, t.disconnect, () => run(c.id, () => api.mcpAction(server, 'logout'))] : null,
      [html`<${IconTrash} size=${14} />`, t.remove, () => run(c.id, () => api.removeMcpServer(server)), 'danger'],
    ]} />`}
    </div>
    ${askToken ? html`<form class="page-form conn-token" onSubmit=${saveToken}>
      <input class="input mono" type="password" autocomplete="off" value=${token} placeholder=${t.tokenLabel}
        aria-label=${t.tokenLabel} onInput=${(e) => setToken(e.currentTarget.value)} />
      <button type="submit" class="btn btn-small btn-primary" disabled=${!token.trim()}>${t.tokenSave}</button>
    </form>${c.tokenHelp ? html`<div class="plug-meta conn-help">${c.tokenHelp}</div>` : null}` : null}
  </div>`;
}

function ConnectorsTab() {
  const conns = useLoad(() => api.connectors());
  const [busy, run] = useAction(conns);
  const { waiting, login } = useLoginWait(() => conns.reload());
  const [opened, setOpened] = useState(null);
  const list = conns.data ?? [];
  return html`<div class="plug-grid">${list.map((c) => html`<${ConnectorCard} key=${c.id} c=${c} busy=${busy} run=${run}
    waiting=${waiting?.key === c.id} onLogin=${login} onOpen=${setOpened} />`)}</div>
    ${!list.length ? html`<div class="page-empty">${conns.loading ? t.loading : '–'}</div>` : null}
    ${opened ? html`<${ServerDrawer} key=${opened} name=${opened} connectors=${list} onClose=${() => setOpened(null)}
      onChanged=${conns.reload} onLogin=${(name) => login(name, list.find((c) => c.servers?.includes(name))?.id)} />` : null}`;
}

// The tab survives a reload of this browser tab.
const TAB_KEY = 'dshx-plugins-tab';
function readTab() {
  try { return t.tabsPlugins[sessionStorage.getItem(TAB_KEY)] ? sessionStorage.getItem(TAB_KEY) : 'plugins'; } catch { return 'plugins'; }
}

export function PluginsPage() {
  const [tab, setTab] = useState(readTab);
  const pick = (k) => {
    setTab(k);
    try { sessionStorage.setItem(TAB_KEY, k); } catch { /* storage blocked */ }
  };
  return html`<${PageShell} title=${t.plugins} lead=${t.pluginsLead} wide>
    <${Tabs} value=${tab} labels=${t.tabsPlugins} onChange=${pick} />
    ${tab === 'plugins' ? html`<${PluginsTab} />` : tab === 'connectors' ? html`<${ConnectorsTab} />`
    : tab === 'skills' ? html`<${SkillsTab} />` : html`<${McpTab} />`}
    <${PageToast} />
  </${PageShell}>`;
}
