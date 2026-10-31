// Real Browser Use pane regression, using Chrome's CDP and a temporary profile.
// DSHX_TEST_CHROME=/path/to/chrome node ui/dev/browser_settings_dom_check.mjs
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { access, mkdtemp, readFile, rm } from 'node:fs/promises';
import { constants } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../', import.meta.url));
let executable;
for (const candidate of [process.env.DSHX_TEST_CHROME, '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', '/usr/bin/google-chrome', '/usr/bin/chromium']) {
  if (!candidate) continue;
  try { await access(candidate, constants.X_OK); executable = candidate; break; } catch { /* next */ }
}
if (!executable) {
  console.log('browser-settings DOM: skipped; set DSHX_TEST_CHROME to run the real UI regression');
  process.exit(0);
}

const types = { '.html': 'text/html', '.js': 'text/javascript', '.mjs': 'text/javascript', '.css': 'text/css', '.json': 'application/json', '.svg': 'image/svg+xml', '.jpg': 'image/jpeg', '.png': 'image/png' };
const server = createServer(async (request, response) => {
  try {
    const file = path.resolve(root, `.${new URL(request.url, 'http://local').pathname}`);
    if (!file.startsWith(`${root.replace(/\/$/, '')}/`)) throw new Error('outside UI');
    const data = await readFile(file);
    response.writeHead(200, { 'Content-Type': types[path.extname(file)] || 'application/octet-stream', 'Cache-Control': 'no-store' });
    response.end(data);
  } catch { response.writeHead(404); response.end(); }
});
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
const profile = await mkdtemp(path.join(tmpdir(), 'aether-browser-ui-'));
const chrome = spawn(executable, [
  '--headless=new', '--disable-gpu', '--no-first-run', '--disable-background-networking', '--use-mock-keychain',
  ...(process.getuid?.() === 0 ? ['--no-sandbox'] : []), '--remote-debugging-port=0', `--user-data-dir=${profile}`, 'about:blank',
], { stdio: ['ignore', 'ignore', 'pipe'], detached: process.platform !== 'win32' });
let socket;
try {
  const endpoint = await new Promise((resolve, reject) => {
    let output = '';
    const timeout = setTimeout(() => reject(new Error('Chrome startup timed out')), 10000);
    const fail = (error) => { clearTimeout(timeout); reject(error); };
    chrome.once('error', fail);
    chrome.once('exit', (code) => fail(new Error(`Chrome exited ${code}: ${output.slice(-1000)}`)));
    chrome.stderr.on('data', (data) => {
      output += data;
      const match = output.match(/DevTools listening on (ws:\/\/[^\s]+)/);
      if (match) { clearTimeout(timeout); resolve(match[1]); }
    });
  });
  const tabs = await fetch(`http://${new URL(endpoint).host}/json/list`).then((response) => response.json());
  socket = new WebSocket(tabs.find((tab) => tab.type === 'page').webSocketDebuggerUrl);
  await new Promise((resolve, reject) => { socket.onopen = resolve; socket.onerror = reject; });
  let serial = 0;
  const pending = new Map();
  const errors = [];
  socket.onmessage = ({ data }) => {
    const message = JSON.parse(data);
    if (message.id) {
      const call = pending.get(message.id);
      if (call) { pending.delete(message.id); message.error ? call.reject(message.error) : call.resolve(message.result); }
    } else if (message.method === 'Runtime.exceptionThrown') {
      errors.push(message.params.exceptionDetails.exception?.description || message.params.exceptionDetails.text);
    } else if (message.method === 'Runtime.consoleAPICalled' && message.params.type === 'error') {
      errors.push(message.params.args.map((arg) => arg.value || arg.description).join(' '));
    }
  };
  const cdp = (method, params = {}) => new Promise((resolve, reject) => {
    const id = ++serial;
    pending.set(id, { resolve, reject });
    socket.send(JSON.stringify({ id, method, params }));
  });
  const evaluate = async (expression) => {
    const result = await cdp('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true, replMode: true });
    if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description || result.exceptionDetails.text);
    return result.result.value;
  };
  const wait = async (expression) => {
    const deadline = Date.now() + 8000;
    while (Date.now() < deadline) {
      if (await evaluate(expression)) return;
      await new Promise((resolve) => setTimeout(resolve, 35));
    }
    throw new Error(`UI timeout: ${expression}`);
  };
  await cdp('Page.enable');
  await cdp('Runtime.enable');
  await cdp('Page.navigate', { url: `http://127.0.0.1:${server.address().port}/index.html?fixture=hero&speed=0&lang=en&ext=empty#/settings/browser` });
  await wait('!!document.querySelector("#bu-chrome") && !document.querySelector("#bu-chrome").disabled');
  await evaluate('globalThis.browserApi = (await import("/src/state.js")).api; globalThis.originalTest = browserApi.testBrowser.bind(browserApi)');
  const refresh = async (chromePath) => {
    await evaluate(`browserApi.browserFixture.setDetectedChrome(${JSON.stringify(chromePath)}); document.querySelector('.bu-status-actions button').click()`);
    await wait(`document.querySelector('.bu-facts dd').textContent === ${JSON.stringify(chromePath ?? 'No executable found')} && document.querySelector('.bu-runtime').getAttribute('aria-busy') === 'false'`);
  };
  const test = async () => {
    await evaluate('document.querySelector(".bu-test button").click()');
    await wait('!!document.querySelector(".bu-result.is-ok")');
  };
  const noPass = async () => assert.equal(await evaluate('!!document.querySelector(".bu-result.is-ok")'), false);

  await refresh('/fixture/chrome-a');
  await test();
  assert.equal(await evaluate('document.querySelector(".bu-tested-binary dd").textContent'), '/fixture/chrome-a');
  await refresh('/fixture/chrome-b');
  await noPass();
  await refresh('/fixture/chrome-a');
  await noPass(); // Returning to A must not resurrect its discarded result.
  await test();
  await refresh(null);
  await noPass();
  assert.equal(await evaluate('(await browserApi.info()).settings.browser.chrome'), null, 'saved auto-detection stays unchanged');

  await refresh('/fixture/chrome-a');
  await evaluate('globalThis.finishProbe = null; browserApi.testBrowser = async () => { const report = await originalTest(); return new Promise(resolve => { globalThis.finishProbe = () => resolve(report); }); }; document.querySelector(".bu-test button").click()');
  await wait('typeof finishProbe === "function"');
  await refresh('/fixture/chrome-b');
  await evaluate('finishProbe()');
  await wait('!document.querySelector(".bu-result.is-busy")');
  await noPass(); // A late successful response for A cannot validate B.

  await refresh('/fixture/chrome-a');
  await evaluate('browserApi.testBrowser = async () => ({ ...await originalTest(), chrome: "/fixture/chrome-b" }); document.querySelector(".bu-test button").click()');
  await wait('document.querySelector(".bu-test").textContent.includes("detected executable changed") && !document.querySelector(".bu-result.is-busy")');
  await noPass(); // The report must match the executable resolved at test start.
  assert.deepEqual(errors, []);
  console.log('browser-settings DOM: A→B/null invalidation, no result resurrection, in-flight invalidation and mismatched report rejection passed');
} finally {
  socket?.close();
  try { process.platform === 'win32' ? chrome.kill('SIGTERM') : process.kill(-chrome.pid, 'SIGTERM'); } catch { /* exited */ }
  await new Promise((resolve) => server.close(resolve));
  await new Promise((resolve) => setTimeout(resolve, 200));
  try { process.platform === 'win32' ? chrome.kill('SIGKILL') : process.kill(-chrome.pid, 'SIGKILL'); } catch { /* exited */ }
  await rm(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
}
