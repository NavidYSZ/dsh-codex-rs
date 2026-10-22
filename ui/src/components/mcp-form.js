// The MCP server form of "Add server" and the drawer's Settings tab, after
// Codex's "Add custom server": name, Local (STDIO) or Remote (HTTP);
// command, arguments, environment (masked, stored in credentials.toml),
// passed-through variables and working folder; or URL, sign-in and
// headers. Plus the result of "Test connection".
import { useEffect, useRef, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t } from '../i18n.js';
import { api } from '../state.js';
import { IconPlus, IconX, IconFolder, IconCheck, IconAlert } from '../icons.js';
import { Field, Segmented } from './drawer.js';
import { chooseFolder } from './folders.js';
import { SERVER_NAME, splitCommand } from '../mcp-import.js';

// ---- the form's state --------------------------------------------------------------

export function emptyForm() {
  return {
    name: '', type: 'stdio', command: '', args: [], env: [], envVars: [], cwd: '',
    url: '', auth: 'oauth', bearerToken: '', bearerEnv: '', headers: [], storedToken: false,
  };
}

/** A server row of `/api/mcp/servers` → the form (stored values stay masked). */
export function formOf(s) {
  const secret = (names) => names.map((key) => ({ key, value: '', stored: true }));
  const bearer = s.secrets?.bearerToken || !!s.bearerTokenEnvVar;
  const headers = (s.headers ?? []).filter((h) => !(s.secrets?.bearerToken && h.toLowerCase() === 'authorization'));
  return {
    ...emptyForm(),
    name: s.name,
    type: s.transport === 'stdio' ? 'stdio' : 'http',
    command: s.command ?? '', args: [...(s.args ?? [])], env: secret(s.env ?? []), envVars: [...(s.envVars ?? [])], cwd: s.cwd ?? '',
    url: s.url ?? '', headers: secret(headers), bearerEnv: s.bearerTokenEnvVar ?? '', storedToken: !!s.secrets?.bearerToken,
    auth: bearer ? 'bearer' : s.oauthEnabled === false ? (headers.length ? 'headers' : 'none') : 'oauth',
  };
}

/** A pasted server (mcp-import.js) → the form; its values are new. */
export function fromImport(f) {
  return {
    ...emptyForm(), ...f,
    env: (f.env ?? []).map((e) => ({ ...e, stored: false })),
    headers: (f.headers ?? []).map((h) => ({ ...h, stored: false })),
    args: f.args ?? [], envVars: f.envVars ?? [], cwd: f.cwd ?? '', auth: f.auth ?? 'oauth',
    bearerToken: f.bearerToken ?? '', bearerEnv: f.bearerEnv ?? '',
  };
}

const loopback = (host) => ['localhost', '127.0.0.1', '[::1]', '::1'].includes(host);

/** Field errors of a form; empty when it can be saved. */
export function validate(form, { editing } = {}) {
  const errors = {};
  if (!editing) {
    if (!form.name.trim()) errors.name = t.mcpNameMissing;
    else if (!SERVER_NAME.test(form.name.trim())) errors.name = t.mcpNameInvalid;
  }
  if (form.type === 'stdio') {
    if (!form.command.trim()) errors.command = t.mcpCommandMissing;
  } else {
    let url = null;
    try { url = new URL(form.url.trim()); } catch { /* below */ }
    if (!form.url.trim()) errors.url = t.mcpUrlMissing;
    else if (!url || !/^https?:$/.test(url.protocol)) errors.url = t.mcpUrlInvalid;
    else if (url.protocol === 'http:' && !loopback(url.hostname)) errors.url = t.mcpUrlPlain;
  }
  const rows = form.type === 'stdio' ? form.env : form.headers;
  const keys = rows.map((r) => r.key.trim()).filter(Boolean);
  if (new Set(keys).size !== keys.length) errors.table = t.mcpDuplicateKey;
  else if (rows.some((r) => !r.key.trim() && r.value)) errors.table = t.mcpKeyMissing;
  if (form.type === 'http' && form.auth === 'bearer' && !form.bearerToken.trim() && !form.storedToken && !form.bearerEnv.trim()) {
    errors.bearer = t.mcpTokenMissing;
  }
  return errors;
}

/** Table rows → `{KEY: value | null}` (`null` keeps a stored value). */
function patch(rows) {
  const out = {};
  for (const r of rows) {
    const key = r.key.trim();
    if (key) out[key] = r.stored && r.value === '' ? null : r.value;
  }
  return out;
}

/** The body of the add, update and test routes. */
export function bodyOf(form) {
  const name = form.name.trim();
  if (form.type === 'stdio') {
    return {
      name,
      config: {
        transport: 'stdio', command: form.command.trim(), args: form.args.filter((a) => a !== ''),
        env_vars: form.envVars, cwd: form.cwd.trim() || null,
      },
      secrets: { env: patch(form.env), headers: {}, bearerToken: '' },
    };
  }
  const bearer = form.auth === 'bearer';
  const config = { transport: 'streamable-http', url: form.url.trim(), bearer_token_env_var: bearer ? form.bearerEnv.trim() || null : null };
  if (!bearer) config.oauth = { enabled: form.auth === 'oauth' };
  return {
    name,
    config,
    secrets: {
      env: {},
      headers: patch(form.headers),
      bearerToken: bearer ? (form.bearerToken.trim() || (form.storedToken ? null : '')) : '',
    },
  };
}

// ---- controls ------------------------------------------------------------------------

const IconEye = ({ off }) => html`<svg width="14" height="14" viewBox="0 0 16 16" aria-hidden="true">
  <path d="M1.75 8s2.3-4.25 6.25-4.25S14.25 8 14.25 8 11.95 12.25 8 12.25 1.75 8 1.75 8Z" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"/>
  <circle cx="8" cy="8" r="1.9" fill="none" stroke="currentColor" stroke-width="1.3"/>
  ${off ? html`<path d="m2.75 2.75 10.5 10.5" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>` : null}</svg>`;

/** Key/value table with masked values; stored values show as dots until replaced. */
function KeyValues({ rows, onChange, keyLabel, keyPlaceholder, addLabel, readOnly }) {
  const [shown, setShown] = useState({});
  const set = (i, patchRow) => onChange(rows.map((r, j) => (j === i ? { ...r, ...patchRow } : r)));
  return html`<div class="mf-table">
    ${rows.map((r, i) => html`<div class="mf-kv" key=${i}>
      <input class="input mono mf-key" value=${r.key} placeholder=${keyPlaceholder} aria-label=${keyLabel} spellcheck=${false}
        autocomplete="off" disabled=${readOnly || r.stored} onInput=${(e) => set(i, { key: e.currentTarget.value })} />
      <span class="mf-value">
        <input class="input mono" type=${shown[i] ? 'text' : 'password'} value=${r.value} autocomplete="new-password" spellcheck=${false}
          disabled=${readOnly} aria-label=${`${t.mcpValue} ${r.key}`}
          placeholder=${r.stored ? t.mcpStored : t.mcpValue} onInput=${(e) => set(i, { value: e.currentTarget.value })} />
        <button type="button" class="icon-btn mf-eye" aria-label=${shown[i] ? t.mcpHide : t.mcpShow} title=${shown[i] ? t.mcpHide : t.mcpShow}
          aria-pressed=${!!shown[i]} disabled=${r.stored && !r.value} onClick=${() => setShown({ ...shown, [i]: !shown[i] })}><${IconEye} off=${shown[i]} /></button>
      </span>
      ${readOnly ? null : html`<button type="button" class="icon-btn" aria-label=${t.mcpRemoveRow(r.key || keyLabel)} title=${t.remove}
        onClick=${() => onChange(rows.filter((_, j) => j !== i))}><${IconX} size=${13} /></button>`}
    </div>`)}
    ${readOnly ? null : html`<button type="button" class="link-btn mf-add" onClick=${() => onChange([...rows, { key: '', value: '', stored: false }])}>
      <${IconPlus} size=${13} />${addLabel}</button>`}
  </div>`;
}

/** Arguments, one field each (a pasted line with spaces stays one argument). */
function Args({ args, onChange, readOnly }) {
  return html`<div class="mf-table">
    ${args.map((a, i) => html`<div class="mf-arg" key=${i}>
      <span class="mf-index">${i + 1}</span>
      <input class="input mono" value=${a} spellcheck=${false} autocomplete="off" disabled=${readOnly} aria-label=${t.mcpArgN(i + 1)}
        onInput=${(e) => onChange(args.map((x, j) => (j === i ? e.currentTarget.value : x)))}
        onKeyDown=${(e) => {
    if (e.key === 'Enter') { e.preventDefault(); onChange([...args.slice(0, i + 1), '', ...args.slice(i + 1)]); requestAnimationFrame(() => e.currentTarget.closest('.mf-table')?.querySelectorAll('.mf-arg input')[i + 1]?.focus()); }
  }} />
      ${readOnly ? null : html`<button type="button" class="icon-btn" aria-label=${t.mcpRemoveRow(a || t.mcpArgN(i + 1))} title=${t.remove}
        onClick=${() => onChange(args.filter((_, j) => j !== i))}><${IconX} size=${13} /></button>`}
    </div>`)}
    ${readOnly ? null : html`<button type="button" class="link-btn mf-add" onClick=${() => onChange([...args, ''])}><${IconPlus} size=${13} />${t.mcpAddArg}</button>`}
  </div>`;
}

/** Names as chips; Enter, comma or space adds, Backspace on empty removes the last. */
function Chips({ values, onChange, placeholder, label, readOnly }) {
  const [draft, setDraft] = useState('');
  const add = () => {
    const names = draft.split(/[\s,]+/).map((v) => v.trim()).filter((v) => v && !values.includes(v));
    if (names.length) onChange([...values, ...names]);
    setDraft('');
  };
  return html`<div class=${cx('mf-chips', readOnly && 'is-readonly')}>
    ${values.map((v) => html`<span class="mf-chip mono" key=${v}>${v}
      ${readOnly ? null : html`<button type="button" aria-label=${t.mcpRemoveRow(v)} onClick=${() => onChange(values.filter((x) => x !== v))}><${IconX} size=${11} /></button>`}</span>`)}
    ${readOnly ? null : html`<input class="mf-chip-input mono" value=${draft} placeholder=${values.length ? '' : placeholder} aria-label=${label}
      spellcheck=${false} autocomplete="off" onInput=${(e) => setDraft(e.currentTarget.value.replace(/[^A-Za-z0-9_,\s]/g, ''))} onBlur=${add}
      onKeyDown=${(e) => {
    if (['Enter', ',', ' '].includes(e.key)) { e.preventDefault(); add(); }
    else if (e.key === 'Backspace' && !draft && values.length) onChange(values.slice(0, -1));
  }} />`}
  </div>`;
}

// ---- the form ---------------------------------------------------------------------------

/**
 * `form`/`onChange` hold the state; `editing` locks the name; `readOnly`
 * shows a plugin's server; `errors` come from `validate`.
 */
export function ServerForm({ form, onChange, editing, readOnly, errors = {} }) {
  const set = (p) => onChange({ ...form, ...p });
  const splitCommandLine = () => {
    // "npx -y @scope/server ~/dir" typed into Command: split it.
    if (form.args.length || !/\s/.test(form.command.trim())) return;
    const [command, ...args] = splitCommand(form.command);
    set({ command, args });
  };
  const pickFolder = async () => {
    const path = await chooseFolder();
    if (path) set({ cwd: path });
  };
  return html`<div class="md-form mf-form">
    <${Field} label=${t.mcpName} error=${errors.name} hint=${editing ? t.mcpNameFixed : t.mcpNameHint}>
      <input class="input mono" value=${form.name} spellcheck=${false} autocomplete="off" maxlength="32" placeholder="github"
        disabled=${editing || readOnly} onInput=${(e) => set({ name: e.currentTarget.value.replace(/\s+/g, '-').replace(/[^A-Za-z0-9_-]/g, '') })} />
    </${Field}>
    <${Field} label=${t.mcpType} as="div">
      <${Segmented} label=${t.mcpType} value=${form.type} onChange=${(type) => !readOnly && set({ type })}
        options=${[['stdio', t.mcpLocalStdio], ['http', t.mcpRemoteHttp]]} />
    </${Field}>
    ${form.type === 'stdio' ? html`
      <${Field} label=${t.mcpCommand} wide error=${errors.command} hint=${t.mcpCommandHint}>
        <input class="input mono" value=${form.command} spellcheck=${false} autocomplete="off" placeholder="npx" disabled=${readOnly}
          onInput=${(e) => set({ command: e.currentTarget.value })} onBlur=${splitCommandLine} />
      </${Field}>
      <${Field} label=${t.mcpArgs} wide as="div">
        <${Args} args=${form.args} readOnly=${readOnly} onChange=${(args) => set({ args })} />
      </${Field}>
      <${Field} label=${t.mcpEnv} wide as="div" error=${errors.table} hint=${t.mcpEnvHint}>
        <${KeyValues} rows=${form.env} readOnly=${readOnly} onChange=${(env) => set({ env })} keyLabel=${t.mcpVariable}
          keyPlaceholder="API_KEY" addLabel=${t.mcpAddVariable} />
      </${Field}>
      <${Field} label=${t.mcpPassthrough} wide as="div" hint=${t.mcpPassthroughHint}>
        <${Chips} values=${form.envVars} readOnly=${readOnly} onChange=${(envVars) => set({ envVars })} placeholder="GITHUB_TOKEN" label=${t.mcpPassthrough} />
      </${Field}>
      <${Field} label=${t.mcpCwd} wide as="div" hint=${t.mcpCwdHint}>
        <div class="md-inline">
          <input class="input mono" value=${form.cwd} spellcheck=${false} autocomplete="off" placeholder="~/code/project" disabled=${readOnly}
            aria-label=${t.mcpCwd} onInput=${(e) => set({ cwd: e.currentTarget.value })} />
          ${readOnly ? null : html`<button type="button" class="btn btn-small" onClick=${pickFolder}><${IconFolder} size=${14} />${t.chooseFolder}</button>`}
        </div>
      </${Field}>`
    : html`
      <${Field} label=${t.mcpUrl} wide error=${errors.url}>
        <input class="input mono" type="url" value=${form.url} spellcheck=${false} autocomplete="off" placeholder="https://mcp.example.com/mcp"
          disabled=${readOnly} onInput=${(e) => set({ url: e.currentTarget.value.trim() })} />
      </${Field}>
      <${Field} label=${t.mcpAuth} wide as="div" hint=${t.mcpAuthHints[form.auth]}>
        <${Segmented} label=${t.mcpAuth} value=${form.auth} onChange=${(auth) => !readOnly && set({ auth })}
          options=${['none', 'bearer', 'oauth', 'headers'].map((k) => [k, t.mcpAuthModes[k]])} />
      </${Field}>
      ${form.auth === 'bearer' ? html`
        <${Field} label=${t.mcpToken} error=${errors.bearer}>
          <input class="input mono" type="password" value=${form.bearerToken} autocomplete="new-password" spellcheck=${false} disabled=${readOnly}
            placeholder=${form.storedToken ? t.mcpStored : t.mcpTokenPlaceholder} onInput=${(e) => set({ bearerToken: e.currentTarget.value })} />
        </${Field}>
        <${Field} label=${t.mcpTokenEnv} hint=${t.mcpTokenEnvHint}>
          <input class="input mono" value=${form.bearerEnv} spellcheck=${false} autocomplete="off" placeholder="LINEAR_API_KEY" disabled=${readOnly}
            onInput=${(e) => set({ bearerEnv: e.currentTarget.value.replace(/[^A-Za-z0-9_]/g, '') })} />
        </${Field}>` : null}
      <${Field} label=${form.auth === 'headers' ? t.mcpHeaders : t.mcpHeadersExtra} wide as="div" error=${errors.table}
        hint=${form.auth === 'headers' ? t.mcpHeadersHint : null}>
        <${KeyValues} rows=${form.headers} readOnly=${readOnly} onChange=${(headers) => set({ headers })} keyLabel=${t.mcpHeader}
          keyPlaceholder=${form.auth === 'headers' ? 'Authorization' : 'X-Api-Key'} addLabel=${t.mcpAddHeader} />
      </${Field}>`}
  </div>`;
}

// ---- test result ---------------------------------------------------------------------------

/** "Connected – 12 tools" or the error with the stderr excerpt. */
export function ProbeResult({ result, busy, elapsed, stale }) {
  const box = useRef(null);
  // A result below the fold scrolls into view.
  useEffect(() => {
    const calm = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches;
    box.current?.scrollIntoView?.({ block: 'nearest', behavior: calm ? 'auto' : 'smooth' });
  }, [result, busy]);
  return html`<div ref=${box} class="mf-probe-slot"><${Probe} result=${result} busy=${busy} elapsed=${elapsed} stale=${stale} /></div>`;
}

function Probe({ result, busy, elapsed, stale }) {
  if (busy) {
    return html`<div class="mf-probe is-busy" role="status"><span class="spinner" />${t.mcpTesting}${elapsed >= 2 ? html`<span class="mf-probe-time">${elapsed} s</span>` : null}</div>`;
  }
  if (!result) return null;
  if (result.ok) {
    const shown = result.tools.slice(0, 12);
    return html`<div class=${cx('mf-probe is-ok', stale && 'is-stale')} role="status">
      <div class="mf-probe-head"><${IconCheck} size=${14} />${t.mcpTestOk(result.tools.length)}${stale ? html`<span class="mf-probe-time">${t.mcpTestStale}</span>` : null}</div>
      ${shown.length ? html`<div class="mf-probe-tools">${shown.map((tool) => html`<span class="mf-chip mono" key=${tool.name} title=${tool.description}>${tool.name}</span>`)}
        ${result.tools.length > shown.length ? html`<span class="mf-more">+${result.tools.length - shown.length}</span>` : null}</div>` : null}
    </div>`;
  }
  if (result.needsLogin) {
    return html`<div class=${cx('mf-probe is-warn', stale && 'is-stale')} role="status">
      <div class="mf-probe-head"><${IconAlert} size=${14} />${t.mcpTestLogin}</div></div>`;
  }
  return html`<div class=${cx('mf-probe is-error', stale && 'is-stale')} role="alert">
    <div class="mf-probe-head"><${IconAlert} size=${14} />${t.mcpTestFailed}</div>
    <div class="mf-probe-error">${result.error}</div>
    ${result.stderr?.length ? html`<pre class="mf-stderr">${result.stderr.slice(-12).join('\n')}</pre>` : null}
  </div>`;
}

/** Runs "Test connection"; `stale` once the form changed after it. */
export function useProbe() {
  const [state, setState] = useState({ busy: false, result: null, key: null, elapsed: 0 });
  const run = async (body) => {
    const key = JSON.stringify(body);
    const started = Date.now();
    const timer = setInterval(() => setState((s) => ({ ...s, elapsed: Math.round((Date.now() - started) / 1000) })), 1000);
    setState({ busy: true, result: null, key, elapsed: 0 });
    try {
      const result = await api.testMcpServer(body);
      setState({ busy: false, result, key, elapsed: 0 });
      return result;
    } catch (e) {
      const result = { ok: false, tools: [], error: e?.message ?? String(e), stderr: [] };
      setState({ busy: false, result, key, elapsed: 0 });
      return result;
    } finally {
      clearInterval(timer);
    }
  };
  const staleFor = (body) => !!state.result && state.key !== JSON.stringify(body);
  const okFor = (body) => !!state.result?.ok && state.key === JSON.stringify(body);
  return { ...state, run, staleFor, okFor, reset: () => setState({ busy: false, result: null, key: null, elapsed: 0 }) };
}
