// A small made-up home folder for the folder picker in fixture mode
// (?fixture=…), shaped like the answers of /api/fs (docs/rust/SPEC.md §6).
import { ApiError } from './api.js';

const HOME = '/Users/alex';
// Folder → subfolders; a trailing * marks a git repository.
const TREE = {
  '/Users/alex': ['.config', '.ssh', 'Desktop', 'Documents', 'Downloads', 'Library', 'code', 'notes'],
  '/Users/alex/code': ['dsh-codex*', 'experiments', 'storefront*', 'tally*', 'timeparse*'],
  '/Users/alex/code/experiments': ['rust-ws*', 'wasm-demo'],
  '/Users/alex/code/dsh-codex': ['.git', 'crates', 'desktop', 'docs', 'ui'],
  '/Users/alex/Documents': ['Invoices', 'Talks'],
};
const HIDDEN = new Set(['Library']);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const join = (dir, name) => (dir === '/' ? `/${name}` : `${dir}/${name}`);
const parentOf = (p) => p.slice(0, p.lastIndexOf('/')) || '/';

function entries(dir) {
  return (TREE[dir] ?? []).map((raw) => {
    const name = raw.replace(/\*$/, '');
    return { name, path: join(dir, name), git: raw.endsWith('*'), hidden: name.startsWith('.') || HIDDEN.has(name) };
  });
}

function fail(status, code, message) {
  return Object.assign(new ApiError(status, message), { code });
}

export const fixtureFs = {
  async dirs(path, hidden) {
    await sleep(120);
    let dir = (path || HOME).replace(/^~(?=\/|$)/, HOME).replace(/\/+$/, '') || '/';
    if (!dir.startsWith('/')) throw fail(400, 'invalid', 'the path must be absolute');
    if (dir !== HOME && !dir.startsWith(`${HOME}/`)) throw fail(403, 'outside-roots', `${dir} is outside the folders this server lists`);
    const known = dir === HOME || entries(parentOf(dir)).some((e) => e.path === dir) || TREE[dir];
    if (!known) throw fail(404, 'not-found', `${dir} does not exist`);
    const list = entries(dir).filter((e) => (hidden || !e.hidden) && e.name !== '.git');
    return {
      path: dir, parent: dir === HOME ? null : parentOf(dir), root: HOME, home: HOME, roots: [HOME],
      git: (TREE[dir] ?? []).includes('.git'), entries: list.sort((a, b) => a.name.localeCompare(b.name)), truncated: false,
    };
  },

  async suggestions() {
    await sleep(200);
    const repos = Object.keys(TREE).flatMap(entries).filter((e) => e.git);
    return { repos: repos.map(({ name, path }) => ({ name, path })) };
  },

  async mkdir(path, name) {
    await sleep(80);
    if (!name || name.includes('/') || name === '.' || name === '..') throw fail(400, 'invalid', 'not a valid folder name');
    if (entries(path).some((e) => e.name === name)) throw fail(409, 'exists', `${join(path, name)} already exists`);
    TREE[path] = [...(TREE[path] ?? []), name];
    return { path: join(path, name) };
  },

  // No native dialog in fixture mode: the UI shows its own browser.
  async pick() { throw fail(501, 'unsupported', 'no native folder dialog'); },
};
