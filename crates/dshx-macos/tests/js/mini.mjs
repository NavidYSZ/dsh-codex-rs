// Sends the mini-window requests of lib/browser.js to a fake helper:
//
//   HOME=<dir> node mini.mjs <repo root>
//
// The page is faked (Runtime.evaluate answers "example.com"); no Chrome runs.
import path from 'node:path';
import { pathToFileURL } from 'node:url';

Object.defineProperty(process, 'platform', { value: 'darwin' });
const repo = process.argv[2];
const { Browser } = await import(pathToFileURL(path.join(repo, 'lib/browser.js')).href);
const { call } = await import(pathToFileURL(path.join(repo, 'lib/cua-client.js')).href);

const browser = new Browser({ chrome: process.execPath, width: 1280, height: 800, locale: 'de' });
browser.send = async () => ({ result: { value: 'example.com' } });
await browser.mini('Klickt „Suche“', { x: 640, y: 200 }, true);
await browser.mini('Liest', { x: 1280, y: 0 });
await browser.mini('Scrollt');
browser.forwardFrame('AAAA');
while (browser.frameBusy) await new Promise((r) => setTimeout(r, 10));
await call({ cmd: 'mini_poll' }, { timeoutMs: 2000, launch: false });
clearInterval(browser.miniTimer);
