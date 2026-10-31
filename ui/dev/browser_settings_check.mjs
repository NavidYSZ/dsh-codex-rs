// node --test ui/dev/browser_settings_check.mjs
// Browser diagnostic claims, MCP availability, and the real HTTP/fixture API.
import assert from 'node:assert/strict';
import { registerHooks } from 'node:module';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import { availableBrowserTools, browserConfigKey, browserDiagnosticKey, browserProbePassed, browserServer } from '../src/browser-settings.js';

const vendor = new URL('../vendor/', import.meta.url);
const map = { preact: 'preact.mjs', 'preact/hooks': 'hooks.mjs', htm: 'htm.mjs', marked: 'marked.mjs' };
registerHooks({ resolve: (spec, context, next) => map[spec]
  ? { url: new URL(map[spec], vendor).href, shortCircuit: true } : next(spec, context) });
globalThis.location = { search: '?lang=en', hash: '', pathname: '/', protocol: 'http:', host: 'localhost' };
globalThis.window = { addEventListener() {}, matchMedia: () => ({ matches: false, addEventListener() {} }) };
globalThis.document = { documentElement: { dataset: {} }, visibilityState: 'hidden' };
globalThis.localStorage = { getItem: () => null, setItem() {} };
globalThis.sessionStorage = { getItem: () => 'fixture-token', setItem() {} };

const { createApi } = await import('../src/api.js');
// Load the entire pane dependency graph: it shares existing Settings/MCP modules.
const { BrowserSection } = await import('../src/components/browser-settings.js');
assert.equal(typeof BrowserSection, 'function');

test('a browser pass requires isolation and every interaction, not just launch or MCP success', () => {
  const complete = { ok: true, isolated: true, checks: ['launch', 'open', 'type', 'click', 'read'].map((name) => ({ name, ok: true })) };
  assert.equal(browserProbePassed(complete), true);
  assert.equal(browserProbePassed({ ...complete, checks: complete.checks.slice(0, 4) }), false);
  assert.equal(browserProbePassed({ ...complete, isolated: false }), false);
  assert.equal(browserProbePassed({ ok: true, tools: [{ name: 'browser_open' }] }), false);
  assert.equal(browserProbePassed({ ...complete, checks: complete.checks.map((check) => ({ ...check, ok: check.name !== 'type' })) }), false);
});

test('browser MCP availability distinguishes discovered, disabled, disconnected and on-demand tools', () => {
  const tools = [{ name: 'mcp__web__browser_open', rawName: 'browser_open' }, { name: 'mcp__web__browser_read', rawName: 'browser_read' }];
  const server = { name: 'web', enabled: true, status: 'connected', tools, disabledTools: ['browser_read'] };
  assert.equal(browserServer(server), true);
  assert.deepEqual(availableBrowserTools(server), [tools[0]]);
  assert.deepEqual(availableBrowserTools({ ...server, enabled: false }), []);
  assert.deepEqual(availableBrowserTools({ ...server, status: 'needs-login' }), []);
  assert.equal(browserServer({ name: 'huntproxy', tools: [] }), true);
  assert.deepEqual(availableBrowserTools({ name: 'huntproxy', status: 'connected', tools: [] }), []);
  assert.equal(browserServer({ name: 'github', tools: [{ rawName: 'search_code' }] }), false);
});

test('browser HTTP methods use the authenticated dedicated routes and scoped settings patch', async () => {
  const requests = [];
  globalThis.fetch = async (path, options) => {
    requests.push({ path, ...options });
    return new Response('{}', { headers: { 'Content-Type': 'application/json' } });
  };
  const api = createApi();
  await api.browserStatus();
  await api.testBrowser();
  await api.updateSettings({ browser: { enabled: false, chrome: null } });
  assert.deepEqual(requests.map(({ path, method }) => [path, method]), [
    ['/api/browser/status', 'GET'], ['/api/browser/test', 'POST'], ['/api/settings', 'PUT'],
  ]);
  assert.ok(requests.every(({ headers }) => headers.Authorization === 'Bearer fixture-token'));
  assert.deepEqual(JSON.parse(requests[2].body), { browser: { enabled: false, chrome: null } });
});

const fixtureFetch = async (path) => new Response(await readFile(new URL(`../${path}`, import.meta.url)), {
  headers: { 'Content-Type': 'application/json' },
});

test('fixture saves preserve active settings, reject invalid paths, and test saved configuration', async () => {
  globalThis.fetch = fixtureFetch;
  location.search = '?fixture=hero&speed=0&browser=disabled';
  const api = createApi();
  const initial = await api.browserStatus();
  assert.equal(initial.active.enabled, false);
  assert.equal(initial.restartRequired, false);
  await api.updateSettings({ browser: { enabled: true } });
  const enabled = await api.browserStatus();
  assert.equal(enabled.configured.enabled, true);
  assert.equal(enabled.active.enabled, false);
  assert.equal(enabled.active.available, false);
  assert.equal(enabled.restartRequired, true);
  const result = await api.testBrowser();
  assert.equal(browserProbePassed(result), true);
  assert.equal((await api.browserStatus()).active.available, false, 'a passing isolated probe does not activate the live tool');
  await assert.rejects(api.updateSettings({ browser: { chrome: 'relative/chrome' } }), /absolute path/);
  assert.equal((await api.browserStatus()).configured.chrome, null, 'a rejected edit preserves the saved configuration');
  await api.updateSettings({ browser: { chrome: '/opt/chrome' } });
  assert.notEqual(browserConfigKey((await api.browserStatus()).configured), browserConfigKey(enabled.configured), 'an executable change invalidates previous diagnostic identity');
  await api.updateSettings({ browser: { chrome: null, enabled: false } });
  assert.equal((await api.browserStatus()).restartRequired, false);
});

test('missing executable and failed interaction fixtures cannot pass', async () => {
  globalThis.fetch = fixtureFetch;
  for (const variant of ['missing', 'failure']) {
    location.search = `?fixture=hero&speed=0&browser=${variant}`;
    const api = createApi();
    const result = await api.testBrowser();
    assert.equal(browserProbePassed(result), false);
    assert.ok(result.error);
  }
});

test('automatic executable changes invalidate diagnostics without changing the saved settings', async () => {
  globalThis.fetch = fixtureFetch;
  location.search = '?fixture=hero&speed=0';
  const api = createApi();
  await api.ready;
  api.browserFixture.setDetectedChrome('/fixture/chrome-a');
  const before = await api.browserStatus();
  const pending = api.testBrowser();
  await Promise.resolve(); // The API waits for fixture readiness before starting the test.
  api.browserFixture.setDetectedChrome('/fixture/chrome-b');
  const changed = await api.browserStatus();
  assert.equal(browserConfigKey(before.configured), browserConfigKey(changed.configured));
  assert.notEqual(browserDiagnosticKey(before.configured, before.resolvedChrome), browserDiagnosticKey(changed.configured, changed.resolvedChrome));
  assert.equal((await pending).chrome, '/fixture/chrome-a', 'the report identifies the binary actually selected when the test started');
  api.browserFixture.setDetectedChrome(null);
  const removed = await api.browserStatus();
  assert.equal(removed.resolvedChrome, null);
  assert.notEqual(browserDiagnosticKey(changed.configured, changed.resolvedChrome), browserDiagnosticKey(removed.configured, removed.resolvedChrome));
});

test('browser MCP fixture discovers tools without claiming a browser interaction pass', async () => {
  globalThis.fetch = fixtureFetch;
  location.search = '?ext=browser';
  const { fixtureExt } = await import('../src/fixture-ext.js?browser-settings-check');
  const server = (await fixtureExt.servers()).find((item) => item.name === 'huntproxy');
  assert.ok(browserServer(server));
  assert.equal(availableBrowserTools(server).length, 4);
  const probe = await fixtureExt.testServer({ name: server.name, config: { command: server.command, args: server.args } });
  assert.equal(probe.ok, true);
  assert.equal(probe.tools.length, 4);
  assert.equal(browserProbePassed(probe), false);
});
