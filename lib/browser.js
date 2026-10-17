// Background browser for the "browser" tool: its own Chrome profile driven
// over the DevTools protocol through a pipe (no debugging port is opened),
// headless unless the user asks to see it. Screencast frames and the agent
// cursor go to the helper's mini window when it is available.
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { call } from './cua-client.js';

const CHROMES = [
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  '/Applications/Chromium.app/Contents/MacOS/Chromium',
  '/Applications/Brave Browser.app/Contents/MacOS/Brave Browser',
  '/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge',
  path.join(os.homedir(), 'Applications/Google Chrome.app/Contents/MacOS/Google Chrome'),
  '/usr/bin/google-chrome', '/usr/bin/chromium', '/usr/bin/chromium-browser',
];

export function findChrome(explicit) {
  for (const p of explicit ? [explicit, ...CHROMES] : CHROMES) if (fs.existsSync(p)) return p;
  return null;
}

/** The .app bundle around a Chrome binary, for the mini window's icon. */
function appBundle(binary) {
  const i = binary.indexOf('.app/');
  return i > 0 ? binary.slice(0, i + 4) : null;
}

/** DevTools protocol over --remote-debugging-pipe (NUL-delimited JSON). */
class Pipe {
  constructor(child) {
    this.writer = child.stdio[3];
    this.reader = child.stdio[4];
    this.id = 1;
    this.pending = new Map();
    this.listeners = new Set();
    let buffer = '';
    this.reader.setEncoding('utf8');
    this.reader.on('data', (chunk) => {
      buffer += chunk;
      let end;
      while ((end = buffer.indexOf('\0')) >= 0) {
        const raw = buffer.slice(0, end);
        buffer = buffer.slice(end + 1);
        let msg;
        try { msg = JSON.parse(raw); } catch { continue; }
        if (msg.id !== undefined && this.pending.has(msg.id)) {
          const { resolve, reject, timer } = this.pending.get(msg.id);
          clearTimeout(timer);
          this.pending.delete(msg.id);
          if (msg.error) reject(new Error(`${msg.error.message}${msg.error.data ? `: ${msg.error.data}` : ''}`));
          else resolve(msg.result ?? {});
        } else if (msg.method) {
          for (const fn of this.listeners) { try { fn(msg); } catch { /* listener errors stay local */ } }
        }
      }
    });
    const fail = () => {
      for (const { reject, timer } of this.pending.values()) { clearTimeout(timer); reject(new Error('browser closed')); }
      this.pending.clear();
    };
    this.reader.on('close', fail);
    this.reader.on('error', fail);
    this.writer.on('error', fail);
  }

  send(method, params = {}, sessionId, timeoutMs = 30_000) {
    const id = this.id++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`browser did not answer ${method} within ${timeoutMs / 1000}s`));
      }, timeoutMs);
      this.pending.set(id, { resolve, reject, timer });
      this.writer.write(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }) + '\0');
    });
  }

  on(fn) { this.listeners.add(fn); return () => this.listeners.delete(fn); }
}

// Runs in the page: indexes visible interactive elements (data-dsh-ix) and
// reports what the model needs to act on them.
const SNAPSHOT = `(() => {
  const SEL = 'a[href],button,input:not([type=hidden]),textarea,select,summary,label[for],[role=button],[role=link],' +
    '[role=checkbox],[role=radio],[role=tab],[role=menuitem],[role=option],[role=switch],[role=combobox],[role=textbox],' +
    '[role=searchbox],[contenteditable=""],[contenteditable=true],[onclick],[tabindex]:not([tabindex="-1"])';
  for (const e of document.querySelectorAll('[data-dsh-ix]')) e.removeAttribute('data-dsh-ix');
  const W = innerWidth, H = innerHeight, items = [];
  const clean = (s) => String(s || '').replace(/\\s+/g, ' ').trim();
  for (const el of document.querySelectorAll(SEL)) {
    if (items.length >= 150) break;
    const r = el.getBoundingClientRect();
    if (r.width < 3 || r.height < 3 || r.bottom <= 0 || r.right <= 0 || r.top >= H || r.left >= W) continue;
    const st = getComputedStyle(el);
    if (st.visibility === 'hidden' || st.display === 'none' || Number(st.opacity) === 0) continue;
    const x = Math.min(Math.max(r.left + r.width / 2, 1), W - 1), y = Math.min(Math.max(r.top + r.height / 2, 1), H - 1);
    const hit = document.elementFromPoint(x, y);
    if (hit && hit !== el && !el.contains(hit) && !hit.contains(el)) continue;
    const tag = el.tagName.toLowerCase(), type = (el.getAttribute('type') || '').toLowerCase(), role = el.getAttribute('role');
    const field = tag === 'input' || tag === 'textarea' || tag === 'select';
    let name = clean(el.getAttribute('aria-label')) || clean(el.labels && el.labels[0] && el.labels[0].innerText) ||
      (field ? clean(el.getAttribute('placeholder')) : '') || clean(el.innerText) || clean(el.getAttribute('title')) ||
      clean(el.getAttribute('alt')) || (field ? '' : clean(el.value));
    if (!name && tag === 'a') name = clean(el.getAttribute('href')).slice(0, 60);
    if (!name) { const img = el.querySelector('img[alt],svg[aria-label]'); name = clean(img && (img.getAttribute('alt') || img.getAttribute('aria-label'))); }
    const item = { i: items.length, kind: role || (tag === 'input' ? 'input' + (type ? '[' + type + ']' : '') : tag), name: name.slice(0, 80), x: Math.round(x), y: Math.round(y) };
    if (field) { if (type === 'password') item.password = true; else if (type !== 'checkbox' && type !== 'radio') item.value = clean(el.value).slice(0, 60); }
    if (type === 'checkbox' || type === 'radio') item.checked = !!el.checked;
    if (el.disabled || el.getAttribute('aria-disabled') === 'true') item.disabled = true;
    el.setAttribute('data-dsh-ix', String(item.i));
    items.push(item);
  }
  const active = document.activeElement && document.activeElement.getAttribute('data-dsh-ix');
  return { title: document.title, url: location.href, scrollY: Math.round(scrollY),
    scrollHeight: document.documentElement.scrollHeight, width: W, height: H, items, focused: active };
})()`;

const TEXT = `(() => {
  const root = document.querySelector('main, article, [role=main]') || document.body;
  return { title: document.title, url: location.href, text: (root ? root.innerText : '').replace(/\\n{3,}/g, '\\n\\n').trim() };
})()`;

const KEYS = {
  enter: ['Enter', 'Enter', 13, '\r'], return: ['Enter', 'Enter', 13, '\r'], tab: ['Tab', 'Tab', 9],
  escape: ['Escape', 'Escape', 27], esc: ['Escape', 'Escape', 27], backspace: ['Backspace', 'Backspace', 8],
  delete: ['Delete', 'Delete', 46], space: [' ', 'Space', 32, ' '], arrowup: ['ArrowUp', 'ArrowUp', 38],
  arrowdown: ['ArrowDown', 'ArrowDown', 40], arrowleft: ['ArrowLeft', 'ArrowLeft', 37], arrowright: ['ArrowRight', 'ArrowRight', 39],
  up: ['ArrowUp', 'ArrowUp', 38], down: ['ArrowDown', 'ArrowDown', 40], left: ['ArrowLeft', 'ArrowLeft', 37], right: ['ArrowRight', 'ArrowRight', 39],
  pageup: ['PageUp', 'PageUp', 33], pagedown: ['PageDown', 'PageDown', 34], home: ['Home', 'Home', 36], end: ['End', 'End', 35],
};
const EDIT_COMMANDS = { a: 'selectAll', c: 'copy', v: 'paste', x: 'cut', z: 'undo' };

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

export class Browser {
  constructor(options = {}) {
    this.options = {
      chrome: findChrome(options.chrome),
      profileDir: options.profileDir ?? path.join(os.homedir(), 'Library/Application Support/dsh-codex/browser-profile'),
      width: options.width ?? 1280,
      height: options.height ?? 800,
      idleMinutes: options.idleMinutes ?? 15,
      mini: options.mini !== false,
      locale: options.locale ?? 'de',
    };
    this.visible = false;
    this.child = null;
    this.pipe = null;
    this.page = null;            // { targetId, sessionId }
    this.popups = [];
    this.dialogs = [];
    this.loading = null;
    this.lastUse = 0;
    this.idleTimer = null;
    this.casting = false;
    this.frameBusy = false;
    this.miniTimer = null;
  }

  get available() { return !!this.options.chrome; }

  // ---- lifecycle ---------------------------------------------------------

  async ensure() {
    this.lastUse = Date.now();
    this.armIdle();
    if (this.child && this.child.exitCode === null && this.page) return;
    await this.launch(this.visible);
  }

  async launch(visible, urls = []) {
    if (!this.options.chrome) throw new Error('No Chrome, Chromium, Brave or Edge found for the background browser');
    fs.mkdirSync(this.options.profileDir, { recursive: true, mode: 0o700 });
    const { width, height } = this.options;
    const args = [
      `--user-data-dir=${this.options.profileDir}`, '--remote-debugging-pipe', '--no-first-run',
      '--no-default-browser-check', '--disable-sync', '--disable-features=Translate,MediaRouter,OptimizationHints',
      '--hide-crash-restore-bubble', `--window-size=${width},${height + (visible ? 88 : 0)}`,
      `--lang=${this.options.locale === 'de' ? 'de-DE' : 'en-US'}`,
      ...(visible ? [] : ['--headless=new']),
      'about:blank',
    ];
    const child = spawn(this.options.chrome, args, { stdio: ['ignore', 'ignore', 'ignore', 'pipe', 'pipe'] });
    this.child = child;
    this.pipe = new Pipe(child);
    this.visible = visible;
    child.on('exit', () => { if (this.child === child) { this.child = null; this.page = null; this.casting = false; } });
    this.pipe.on((msg) => this.onEvent(msg));
    await this.pipe.send('Browser.getVersion', {}, undefined, 20_000);
    await this.pipe.send('Target.setDiscoverTargets', { discover: true });
    await this.pipe.send('Browser.setDownloadBehavior', { behavior: 'deny' }).catch(() => {});
    const { targetInfos } = await this.pipe.send('Target.getTargets');
    const first = targetInfos.find((t) => t.type === 'page');
    const targetId = first ? first.targetId : (await this.pipe.send('Target.createTarget', { url: 'about:blank' })).targetId;
    await this.attach(targetId);
    for (const [i, url] of urls.entries()) {
      if (i === 0) await this.navigate(url);
      else await this.pipe.send('Target.createTarget', { url, background: true });
    }
  }

  async close() {
    clearTimeout(this.idleTimer);
    clearInterval(this.miniTimer);
    this.miniTimer = null;
    const child = this.child;
    this.child = null;
    this.page = null;
    if (!child) return;
    try { await this.pipe.send('Browser.close', {}, undefined, 3000); } catch { /* fall through to kill */ }
    setTimeout(() => { if (child.exitCode === null) child.kill('SIGKILL'); }, 2000).unref();
  }

  armIdle() {
    clearTimeout(this.idleTimer);
    if (this.visible) return;  // the user is looking at it; never pull it away
    this.idleTimer = setTimeout(() => this.close(), this.options.idleMinutes * 60_000);
    this.idleTimer.unref?.();
  }

  /** Show the browser window to the user (or hide it again), keeping tabs. */
  async setVisible(on) {
    if (this.child && this.visible === on) return;
    const urls = [];
    if (this.child) {
      const { targetInfos } = await this.pipe.send('Target.getTargets').catch(() => ({ targetInfos: [] }));
      const current = targetInfos.find((t) => t.targetId === this.page?.targetId);
      if (current?.url && current.url !== 'about:blank') urls.push(current.url);
      for (const t of targetInfos) if (t.type === 'page' && t !== current && /^https?:/.test(t.url)) urls.push(t.url);
      await this.close();
      await sleep(600);
    }
    await this.launch(on, urls);
  }

  async attach(targetId) {
    const { sessionId } = await this.pipe.send('Target.attachToTarget', { targetId, flatten: true });
    this.page = { targetId, sessionId };
    const s = (m, p) => this.pipe.send(m, p, sessionId);
    await s('Page.enable');
    await s('Page.setLifecycleEventsEnabled', { enabled: true }).catch(() => {});
    if (!this.visible) {
      await s('Emulation.setDeviceMetricsOverride', { width: this.options.width, height: this.options.height, deviceScaleFactor: 1, mobile: false });
    }
    this.casting = false;
    if (this.options.mini) await this.startCast().catch(() => {});
  }

  send(method, params = {}) { return this.pipe.send(method, params, this.page.sessionId); }

  onEvent({ method, params, sessionId }) {
    const info = params?.targetInfo;
    if (method === 'Target.targetCreated' && info.type === 'page' && info.openerId && info.openerId === this.page?.targetId
      && info.targetId !== this.page?.targetId) {
      this.popups.push(info.targetId);
    } else if (method === 'Page.javascriptDialogOpening' && sessionId === this.page?.sessionId) {
      const accept = params.type === 'alert' || params.type === 'beforeunload';
      this.dialogs.push(`${params.type} "${String(params.message).slice(0, 200)}" was ${accept ? 'accepted' : 'dismissed'}`);
      this.send('Page.handleJavaScriptDialog', { accept }).catch(() => {});
    } else if (method === 'Page.frameStartedLoading' && sessionId === this.page?.sessionId) {
      this.loading ??= Date.now();
    } else if ((method === 'Page.loadEventFired' || method === 'Page.frameStoppedLoading') && sessionId === this.page?.sessionId) {
      this.loading = null;
    } else if (method === 'Page.screencastFrame' && sessionId === this.page?.sessionId) {
      this.send('Page.screencastFrameAck', { sessionId: params.sessionId }).catch(() => {});
      this.forwardFrame(params.data);
    }
  }

  // ---- mini window -------------------------------------------------------

  async startCast() {
    if (this.casting || !this.page) return;
    this.casting = true;
    await this.send('Page.startScreencast', { format: 'jpeg', quality: 55, maxWidth: 720, maxHeight: 460, everyNthFrame: 2 });
  }

  forwardFrame(data) {
    if (this.frameBusy) return;   // drop frames while the helper is busy
    this.frameBusy = true;
    call({ cmd: 'mini_frame', data }, { timeoutMs: 4000, launch: false })
      .then((res) => this.handleRequests(res))
      .catch(() => {})
      .finally(() => { this.frameBusy = false; });
  }

  handleRequests(res) {
    if (res?.requests?.includes('show')) this.setVisible(true).catch(() => {});
  }

  /** Shows the mini window with the current page and step; never fails the action. */
  async mini(activity, point, click = false) {
    if (!this.options.mini) return;
    try {
      const host = await this.send('Runtime.evaluate', { expression: 'location.host', returnByValue: true })
        .then((r) => r.result?.value || '').catch(() => '');
      const title = host ? `Browser · ${host}` : 'Browser';
      const show = await call({ cmd: 'mini_show', title, icon: appBundle(this.options.chrome) ?? 'globe',
        width: this.options.width, height: this.options.height, activity }, { timeoutMs: 5000 });
      this.handleRequests(show);
      if (show?.ok === false && show.cancelled) throw Object.assign(new Error(show.error), { cancelled: true });
      if (point) {
        const res = await call({ cmd: 'mini_step', x: point.x / this.options.width, y: point.y / this.options.height, click },
          { timeoutMs: 5000 });
        if (res?.ok === false && res.cancelled) throw Object.assign(new Error(res.error), { cancelled: true });
      }
      clearInterval(this.miniTimer);
      // Picks up "show" clicks on the mini window while the agent is thinking.
      const until = Date.now() + 40_000;
      this.miniTimer = setInterval(() => {
        if (Date.now() > until || !this.child) { clearInterval(this.miniTimer); this.miniTimer = null; return; }
        call({ cmd: 'mini_poll' }, { timeoutMs: 2000, launch: false }).then((r) => this.handleRequests(r)).catch(() => {});
      }, 900);
      this.miniTimer.unref?.();
    } catch (err) {
      if (err.cancelled) throw err;
    }
  }

  // ---- page helpers ------------------------------------------------------

  async evaluate(expression) {
    const res = await this.send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
    if (res.exceptionDetails) throw new Error(res.exceptionDetails.exception?.description || res.exceptionDetails.text);
    return res.result?.value;
  }

  async settle(maxMs = 12_000) {
    await sleep(250);
    const start = Date.now();
    while (this.loading && Date.now() - start < maxMs) await sleep(120);
    await sleep(this.loading ? 0 : 200);
  }

  async navigate(raw) {
    let url = String(raw || '').trim();
    if (!/^[a-z][a-z0-9+.-]*:/i.test(url)) {
      url = /^[^\s]+\.[^\s]{2,}(\/.*)?$/.test(url) || /^localhost(:\d+)?/.test(url)
        ? `https://${url}` : `https://duckduckgo.com/?q=${encodeURIComponent(url)}`;
    }
    if (!/^(https?:|about:blank)/i.test(url)) throw new Error('only http(s) pages can be opened in the background browser');
    this.loading = Date.now();
    const res = await this.send('Page.navigate', { url });
    if (res.errorText) { this.loading = null; throw new Error(`could not open ${url}: ${res.errorText}`); }
    await this.settle(15_000);
  }

  async center(index) {
    const at = await this.evaluate(`(() => {
      const el = document.querySelector('[data-dsh-ix="${Number(index)}"]');
      if (!el) return null;
      el.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
      const r = el.getBoundingClientRect();
      return { x: r.left + r.width / 2, y: r.top + r.height / 2, password: el.type === 'password',
        tag: el.tagName.toLowerCase(), editable: el.isContentEditable };
    })()`);
    if (!at) throw new Error(`element [${index}] is gone; look at the latest state and use a fresh index`);
    return at;
  }

  async mouseClick(x, y, count = 1, button = 'left') {
    await this.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x, y });
    for (let i = 1; i <= count; i++) {
      await this.send('Input.dispatchMouseEvent', { type: 'mousePressed', x, y, button, clickCount: i });
      await this.send('Input.dispatchMouseEvent', { type: 'mouseReleased', x, y, button, clickCount: i });
    }
  }

  async pressKey(combo) {
    const parts = String(combo).split('+').map((p) => p.trim()).filter(Boolean);
    const name = parts.pop() ?? '';
    let modifiers = 0;
    let meta = false;
    for (const m of parts.map((p) => p.toLowerCase())) {
      if (m === 'alt' || m === 'option') modifiers |= 1;
      else if (m === 'ctrl' || m === 'control') modifiers |= 2;
      else if (m === 'cmd' || m === 'command' || m === 'meta') { modifiers |= 4; meta = true; }
      else if (m === 'shift') modifiers |= 8;
      else throw new Error(`unknown modifier "${m}"`);
    }
    const known = KEYS[name.toLowerCase()];
    let key, code, keyCode, text;
    if (known) [key, code, keyCode, text] = known;
    else if (name.length === 1) {
      key = name;
      code = /[a-z]/i.test(name) ? `Key${name.toUpperCase()}` : /\d/.test(name) ? `Digit${name}` : undefined;
      keyCode = name.toUpperCase().charCodeAt(0);
      text = modifiers & 7 ? undefined : name;
    } else throw new Error(`unknown key "${name}"`);
    const commands = (meta || modifiers & 2) && EDIT_COMMANDS[name.toLowerCase()] ? [EDIT_COMMANDS[name.toLowerCase()]] : undefined;
    await this.send('Input.dispatchKeyEvent', { type: text ? 'keyDown' : 'rawKeyDown', key, code, windowsVirtualKeyCode: keyCode, modifiers, text, commands });
    await this.send('Input.dispatchKeyEvent', { type: 'keyUp', key, code, windowsVirtualKeyCode: keyCode, modifiers });
  }

  async adoptPopup() {
    const id = this.popups.pop();
    this.popups = [];
    if (!id) return null;
    await sleep(300);
    await this.attach(id);
    await this.settle();
    return id;
  }

  // ---- state -------------------------------------------------------------

  async state({ quality = 0.7 } = {}) {
    const snap = await this.evaluate(SNAPSHOT);
    const metrics = await this.send('Page.getLayoutMetrics').catch(() => null);
    const vv = metrics?.cssVisualViewport;
    const dpr = this.visible ? await this.evaluate('devicePixelRatio').catch(() => 1) : 1;
    const params = { format: 'jpeg', quality: Math.round(quality * 100) };
    if (dpr !== 1 && vv) params.clip = { x: vv.pageX, y: vv.pageY, width: vv.clientWidth, height: vv.clientHeight, scale: 1 / dpr };
    const shot = await this.send('Page.captureScreenshot', params);
    const { targetInfos } = await this.pipe.send('Target.getTargets');
    const tabs = targetInfos.filter((t) => t.type === 'page');
    return { snap, image: { mime: 'image/jpeg', data: shot.data, width: snap.width, height: snap.height }, tabs };
  }

  describe(state) {
    const { snap, tabs } = state;
    const index = tabs.findIndex((t) => t.targetId === this.page?.targetId);
    const lines = [
      `Background browser (${this.visible ? 'visible window' : 'headless, the user sees it in the mini window'}) · tab ${index + 1}/${tabs.length}`,
      `Title: ${snap.title || '(none)'}`,
      `URL: ${snap.url}`,
      `Viewport ${snap.width}×${snap.height}, scrolled ${snap.scrollY} of ${Math.max(snap.scrollHeight - snap.height, 0)} px. Screenshot pixels = page coordinates.`,
    ];
    if (this.dialogs.length) { lines.push(`Dialogs: ${this.dialogs.join('; ')}`); this.dialogs = []; }
    lines.push('Interactive elements, [index] kind "name" @(x,y):');
    for (const it of snap.items) {
      let line = `[${it.i}] ${it.kind}${it.name ? ` "${it.name}"` : ''}`;
      if (it.password) line += ' (password)';
      else if (it.value !== undefined && it.value !== '') line += ` value="${it.value}"`;
      if (it.checked !== undefined) line += it.checked ? ' checked' : ' unchecked';
      if (it.disabled) line += ' disabled';
      if (snap.focused === String(it.i)) line += ' focused';
      lines.push(`${line} @(${it.x},${it.y})`);
    }
    if (!snap.items.length) lines.push('(none visible; scroll, wait or read the text)');
    return lines.join('\n');
  }

  async readText(max = 12_000) {
    const r = await this.evaluate(TEXT);
    const text = r.text.length > max ? `${r.text.slice(0, max)}\n… (${r.text.length - max} more characters; scroll and read again for more)` : r.text;
    return `Title: ${r.title}\nURL: ${r.url}\n\n${text}`;
  }
}
