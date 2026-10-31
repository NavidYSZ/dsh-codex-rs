// Browser Use: saved settings, the current runtime, and explicit diagnostics.
import { useEffect, useLayoutEffect, useRef, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t, lang } from '../i18n.js';
import { api, store, openPlugins } from '../state.js';
import { useStore } from '../store.js';
import {
  absoluteBrowserPath, availableBrowserTools, BROWSER_CHECKS, browserConfig, browserConfigKey, browserDiagnosticKey, browserProbePassed, browserServer,
} from '../browser-settings.js';
import { IconAlert, IconCheck, IconGlobe, IconPlug, IconRefresh } from '../icons.js';
import { Field } from './settings.js';
import { Pill, Toggle, useLoad } from './page.js';
import { StatusDot } from './plugin-kit.js';
import { bodyOf, formOf, ProbeResult, useProbe } from './mcp-form.js';
import { openDetails } from './composer-extras.js';

const b = t.browserUse;
const errorText = (error) => error?.message || String(error);
const checkedAt = (ms) => new Date(ms).toLocaleString(lang, { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit', second: '2-digit' });

function BrowserResult({ result, busy }) {
  if (busy) return html`<div class="bu-result is-busy" role="status"><span class="spinner" />${b.testing}</div>`;
  if (!result) return html`<p class="bu-note">${b.notTested}</p>`;
  const ok = browserProbePassed(result);
  return html`<div class=${cx('bu-result', ok ? 'is-ok' : 'is-error')} role=${ok ? 'status' : 'alert'}>
    <div class="bu-result-title">${ok ? html`<${IconCheck} size=${15} />` : html`<${IconAlert} size=${15} />`}${ok ? b.testPassed : b.testFailed}</div>
    <ul class="bu-checks" aria-label=${b.checksLabel}>
      ${BROWSER_CHECKS.map((name) => {
        const check = result.checks?.find((item) => item.name === name);
        return html`<li key=${name} class=${check?.ok ? 'is-ok' : 'is-pending'}>${check?.ok ? html`<${IconCheck} size=${12} />` : '–'}${b.checks[name]}</li>`;
      })}
    </ul>
    ${!ok ? html`<p class="bu-error">${result.error || b.incompleteTest}</p>` : null}
    ${result.chrome ? html`<dl class="bu-facts bu-tested-binary"><div><dt>${b.testedBinary}</dt><dd class="mono">${result.chrome}</dd></div></dl>` : null}
    ${result.checkedAt ? html`<p class="bu-note">${b.testTime(checkedAt(result.checkedAt), Math.max(0, result.durationMs ?? 0) / 1000)}</p>` : null}
  </div>`;
}

function BackgroundBrowser() {
  const info = useStore(store, (s) => s.info);
  const [status, setStatus] = useState(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState(null);
  const [nonce, setNonce] = useState(0);
  const saved = browserConfig(info?.settings?.browser ?? status?.configured);
  const savedKey = browserConfigKey(saved);
  const diagnosticKey = status ? browserDiagnosticKey(saved, status.resolvedChrome) : null;
  const [draft, setDraft] = useState(saved);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState(null);
  const [savedNote, setSavedNote] = useState(false);
  const [testing, setTesting] = useState(false);
  const [result, setResult] = useState(null);
  const [testNotice, setTestNotice] = useState(null);
  const testGeneration = useRef(0);
  const diagnosticIdentity = useRef(null);
  const edited = useRef(false);
  const dirty = browserConfigKey(draft) !== savedKey;
  const invalidPath = !absoluteBrowserPath(draft.chrome?.trim());
  const locked = saving || testing;

  useEffect(() => {
    if (!edited.current) setDraft(saved);
  }, [savedKey]);
  useLayoutEffect(() => {
    if (diagnosticIdentity.current === diagnosticKey) return;
    diagnosticIdentity.current = diagnosticKey;
    setResult(null);
    testGeneration.current += 1;
  }, [diagnosticKey]);
  useEffect(() => () => { testGeneration.current += 1; }, []);
  useEffect(() => {
    let live = true;
    setLoading(true);
    setLoadError(null);
    api.browserStatus().then((data) => {
      if (!live) return;
      const identity = browserDiagnosticKey(data.configured, data.resolvedChrome);
      // Invalidate synchronously, before an old in-flight test can finish.
      if (identity !== diagnosticIdentity.current) {
        diagnosticIdentity.current = identity;
        testGeneration.current += 1;
        setResult(null);
      }
      setStatus(data);
      store.set((state) => ({ info: { ...state.info, settings: { ...state.info?.settings, browser: data.configured } } }));
      setLoading(false);
    }, (error) => {
      if (!live) return;
      setLoadError(error);
      setLoading(false);
    });
    return () => { live = false; };
  }, [nonce]);

  const edit = (patch) => {
    setDraft((current) => {
      const next = { ...current, ...patch };
      edited.current = browserConfigKey(next) !== savedKey;
      return next;
    });
    setResult(null);
    setTestNotice(null);
    setSaveError(null);
    setSavedNote(false);
    testGeneration.current += 1;
  };
  const save = async (event) => {
    event.preventDefault();
    if (locked || invalidPath || !dirty) return;
    setSaving(true);
    setSaveError(null);
    setResult(null);
    setTestNotice(null);
    testGeneration.current += 1;
    try {
      const updated = await api.updateSettings({ browser: browserConfig(draft) });
      edited.current = false;
      if (updated) store.set({ info: updated });
      setSavedNote(true);
      setNonce((value) => value + 1);
    } catch (error) { setSaveError(errorText(error)); }
    finally { setSaving(false); }
  };
  const test = async () => {
    if (locked || dirty || !saved.enabled || loading || loadError) return;
    const generation = ++testGeneration.current;
    const identity = diagnosticKey;
    const chrome = status.resolvedChrome ?? null;
    setTesting(true);
    setResult(null);
    setTestNotice(null);
    try {
      const answer = await api.testBrowser();
      if (generation !== testGeneration.current || identity !== diagnosticIdentity.current) return;
      if (answer.chrome !== chrome) {
        setTestNotice(b.testChanged);
        setNonce((value) => value + 1);
        return;
      }
      setResult({ report: answer, identity });
    } catch (error) {
      if (generation === testGeneration.current && identity === diagnosticIdentity.current) {
        setResult({ report: { ok: false, error: errorText(error), checks: [] }, identity });
      }
    } finally { setTesting(false); }
  };

  const active = status?.active;
  const activeLabel = !active ? b.unknown : !active.enabled ? b.activeDisabled : active.available ? b.available : b.unavailable;
  return html`<section class="card bu-card" aria-labelledby="bu-background-title">
    <h2 class="card-title" id="bu-background-title"><${IconGlobe} size=${15} />${b.background}</h2>
    ${loadError ? html`<div class="bu-load-error" role="alert"><p>${loadError.status === 404 || loadError.status === 405 ? b.unsupported : errorText(loadError)}</p>
      <button type="button" class="btn btn-small" disabled=${locked} onClick=${() => setNonce((value) => value + 1)}>${t.retry}</button></div>` : null}
    <form onSubmit=${save}>
      <${Field} label=${b.enabled} hint=${b.enabledHint}>
        <${Toggle} on=${draft.enabled} label=${b.enabled} disabled=${locked || loading || !!loadError} onChange=${(enabled) => edit({ enabled })} />
      </${Field}>
      <div class="bu-path-field">
        <label class="field-label" for="bu-chrome">${b.chrome}</label>
        <p class="field-hint" id="bu-chrome-hint">${b.chromeHint}</p>
        <input id="bu-chrome" class="input mono" type="text" value=${draft.chrome ?? ''} placeholder=${b.auto}
          aria-describedby="bu-chrome-hint" aria-invalid=${invalidPath || undefined} spellcheck="false" autocomplete="off"
          disabled=${locked || loading || !!loadError} onInput=${(event) => edit({ chrome: event.currentTarget.value })} />
        ${invalidPath ? html`<p class="bu-error" role="alert">${b.absolutePath}</p>` : null}
      </div>
      <div class="bu-save-row">
        <span class="bu-note" role="status">${dirty ? b.unsaved : savedNote ? t.saved : ''}</span>
        <button type="submit" class="btn btn-small" disabled=${locked || loading || !!loadError || !dirty || invalidPath}>
          ${saving ? html`<span class="spinner" />` : null}${t.save}</button>
      </div>
      ${saveError ? html`<p class="bu-error" role="alert">${saveError}</p>` : null}
    </form>
    <div class="bu-runtime" aria-busy=${loading}>
      <${Field} label=${b.active} hint=${active?.available ? active.running ? b.running : b.idle : null}>
        <span class="bu-status-actions"><${Pill} tone=${loading || loadError ? 'off' : active?.available ? 'ok' : 'warn'}>${loading ? b.loading : loadError ? b.unknown : activeLabel}</${Pill}>
          <button type="button" class="icon-btn" aria-label=${b.refreshStatus} disabled=${saving || loading} onClick=${() => setNonce((value) => value + 1)}><${IconRefresh} size=${13} /></button></span>
      </${Field}>
      <dl class="bu-facts">
        <div><dt>${b.savedBinary}</dt><dd class="mono">${status ? status.resolvedChrome || b.noBinary : '–'}</dd></div>
        <div><dt>${b.activeBinary}</dt><dd class="mono">${active?.chrome || '–'}</dd></div>
        <div><dt>${b.profile}</dt><dd class="mono">${active?.profile || '–'}</dd></div>
      </dl>
      ${status?.restartRequired && !loading ? html`<p class="bu-notice" role="status"><${IconAlert} size=${14} />${b.restart}</p>` : null}
      <p class="bu-note">${b.profileHint}</p>
    </div>
    <div class="bu-test">
      <div class="bu-test-heading"><h3>${b.testHeading}</h3>
        <button type="button" class="btn btn-small" disabled=${locked || loading || !!loadError || dirty || !saved.enabled} onClick=${test}>
          <${IconRefresh} size=${13} />${b.test}</button></div>
      <p class="bu-note">${b.testHint}</p>
      ${dirty ? html`<p class="bu-note">${b.saveBeforeTest}</p>` : null}
      ${testNotice ? html`<p class="bu-note" role="status">${testNotice}</p>` : null}
      <${BrowserResult} result=${result?.identity === diagnosticKey ? result.report : null} busy=${testing} />
    </div>
  </section>`;
}

function BrowserMcpServer({ server }) {
  const probe = useProbe();
  const body = bodyOf(formOf(server));
  const available = availableBrowserTools(server);
  return html`<div class="bu-mcp-server">
    <div class="bu-mcp-heading"><button type="button" class="link-btn" onClick=${() => openDetails('server', server.name)}>${server.name}</button>
      <${StatusDot} status=${server.status} retrying=${server.retrying} /></div>
    <p class="bu-note">${available.length ? b.mcpAvailable(available.length) : b.mcpUnavailable}</p>
    ${available.length ? html`<p class="bu-note">${server.always ? b.mcpAlways : b.mcpActivate(server.name)}</p>` : null}
    ${server.error ? html`<p class="bu-error">${server.error}</p>` : null}
    <div class="bu-mcp-actions">
      <button type="button" class="btn btn-small" onClick=${() => openDetails('server', server.name)}>${b.mcpDetails}</button>
      ${server.origin === 'plugin' ? null : html`<button type="button" class="btn btn-small" disabled=${probe.busy || server.enabled === false}
        onClick=${() => probe.run(body)}>${b.mcpTest}</button>`}
    </div>
    ${probe.result || probe.busy ? html`<${ProbeResult} result=${probe.result} busy=${probe.busy} elapsed=${probe.elapsed} stale=${probe.staleFor(body)} />` : null}
  </div>`;
}

function BrowserMcp() {
  const servers = useLoad(() => api.mcpServers());
  const list = (servers.data ?? []).filter(browserServer);
  useEffect(() => {
    if (!(servers.data ?? []).some((server) => server.status === 'connecting' || server.retrying)) return undefined;
    const timer = setTimeout(servers.reload, 1500);
    return () => clearTimeout(timer);
  }, [servers.data]);
  return html`<section class="card bu-card" aria-labelledby="bu-mcp-title">
    <h2 class="card-title" id="bu-mcp-title"><${IconPlug} size=${15} />${b.mcp}</h2>
    <p class="bu-note">${b.mcpHint}</p>
    ${servers.loading && !servers.data ? html`<p class="bu-note" role="status">${b.loading}</p>`
    : servers.error ? html`<p class="bu-error" role="alert">${t.mcpLoadFailed}</p><button type="button" class="btn btn-small" onClick=${servers.reload}>${t.retry}</button>`
      : list.length ? list.map((server) => html`<${BrowserMcpServer} key=${server.name} server=${server} />`)
        : html`<div class="bu-mcp-empty"><p>${b.mcpEmpty}</p><p class="bu-note">${b.mcpSetup}</p></div>`}
    <div class="bu-mcp-footer"><button type="button" class="btn btn-small" onClick=${() => openPlugins('mcp')}>${b.mcpManage}</button>
      <button type="button" class="icon-btn" aria-label=${b.mcpRefresh} onClick=${servers.reload}><${IconRefresh} size=${14} /></button></div>
  </section>`;
}

export function BrowserSection() {
  const desktop = useStore(store, (s) => s.desktop);
  const current = desktop?.instances?.find((instance) => instance.id === desktop.current);
  return html`<div class="bu-settings">
    <p class="bu-host">${current ? b.hostNamed(current.name) : b.host}</p>
    <${BackgroundBrowser} />
    <${BrowserMcp} />
    <p class="bu-guidance">${b.routing}</p>
  </div>`;
}
