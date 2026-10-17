// Talks to "DSH Computer Use.app" over its Unix socket (one JSON line per
// request and response). Starts the helper on demand.
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import fs from 'node:fs';
import { execFile } from 'node:child_process';

export const SOCKET_PATH = path.join(os.homedir(), 'Library/Application Support/dsh-codex/cua.sock');
export const APP_PATH = path.join(os.homedir(), 'Applications/DSH Computer Use.app');

let nextId = 1;

function send(request, { timeoutMs = 60_000 } = {}) {
  return new Promise((resolve, reject) => {
    const socket = net.createConnection(SOCKET_PATH);
    const chunks = [];
    const timer = setTimeout(() => {
      socket.destroy();
      reject(new Error(`computer-use helper timed out on "${request.cmd}"`));
    }, timeoutMs);
    socket.on('connect', () => socket.write(JSON.stringify({ id: nextId++, ...request }) + '\n'));
    socket.on('data', (chunk) => chunks.push(chunk));
    socket.on('error', (err) => { clearTimeout(timer); reject(err); });
    socket.on('close', () => {
      clearTimeout(timer);
      const text = Buffer.concat(chunks).toString('utf8').trim();
      if (!text) return reject(new Error('computer-use helper closed the connection without answering'));
      try { resolve(JSON.parse(text)); } catch (err) { reject(err); }
    });
  });
}

function launch(appPath) {
  return new Promise((resolve, reject) => {
    if (!fs.existsSync(appPath)) {
      return reject(new Error(`computer-use helper not installed at ${appPath} (run helper/build.sh --install)`));
    }
    // -g: do not bring it to the front; it is a menu-less accessory app.
    execFile('/usr/bin/open', ['-g', appPath], (err) => (err ? reject(err) : resolve()));
  });
}

/** Sends one request. `launch: false` skips starting the helper (for frames). */
export async function call(request, options = {}) {
  try {
    return await send(request, options);
  } catch (err) {
    if (!['ENOENT', 'ECONNREFUSED'].includes(err.code) || options.launch === false) throw err;
    await launch(options.appPath ?? APP_PATH);
    for (let i = 0; i < 40; i++) {
      await new Promise((r) => setTimeout(r, 150));
      try { return await send(request, options); } catch (e) {
        if (!['ENOENT', 'ECONNREFUSED'].includes(e.code)) throw e;
      }
    }
    throw new Error('computer-use helper did not start');
  }
}
