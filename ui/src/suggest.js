// The composer's `/` and `@` grammar (after dsh-client-ui-input-trigger and
// dsh-file-reference): where a trigger is live, how a menu query ranks,
// and which tokens of a prompt render as pills. Pure functions plus a small
// cache of the names the menus returned.
import { createStore } from './store.js';

const WORD = /[\p{L}\p{N}_]/u;
const SPACE = /\s/u;
/** Skill and command names: kebab-case words. */
const NAME = /^[a-z0-9][a-z0-9-]*$/i;
const SLASH_QUERY = /^[a-z0-9-]*$/i;

/**
 * The trigger at the caret: `{trigger: '/'|'@', start, end, query, quoted}`
 * or null. `/` opens only at the start or after whitespace and only while
 * the query is a name (never inside `/usr/bin` or a URL); `@` opens at the
 * start, after whitespace or punctuation (never inside an e-mail address),
 * and `@"…` keeps a quoted path open across spaces.
 */
export function detectTrigger(text, caret) {
  const before = text.slice(0, caret);
  const quoted = /(?:^|\s)(@"([^"]*))$/u.exec(before);
  if (quoted) return { trigger: '@', start: caret - quoted[1].length, end: caret, query: quoted[2], quoted: true };
  for (let i = caret - 1; i >= 0; i--) {
    const ch = text[i];
    if (SPACE.test(ch)) return null;
    if (ch === '@') {
      if (i > 0 && WORD.test(text[i - 1])) return null;
      return { trigger: '@', start: i, end: caret, query: text.slice(i + 1, caret), quoted: false };
    }
    if (ch === '/' || ch === '$') {
      if (i > 0 && !SPACE.test(text[i - 1])) continue;
      const query = text.slice(i + 1, caret);
      if (!SLASH_QUERY.test(query)) return null;
      // `$` is the Codex spelling for skills only; it needs a letter first.
      if (ch === '$' && !/^[a-z]/i.test(query)) return null;
      return { trigger: ch === '$' ? '$' : '/', start: i, end: caret, query, quoted: false };
    }
  }
  return null;
}

/**
 * Match quality of `query` in `text`, like the server: exact, prefix, word
 * prefix, substring, subsequence; null = no match.
 */
export function rank(query, text) {
  if (!query) return 1;
  const q = query.toLowerCase();
  const s = String(text ?? '').toLowerCase();
  if (s === q) return 10000;
  if (s.startsWith(q)) return 9000 - Math.min(999, s.length - q.length);
  const at = s.indexOf(q);
  if (at >= 0) return (/[-_/. ]/.test(s[at - 1] ?? '') ? 7000 : 5000) - Math.min(999, at);
  let gaps = 0;
  let j = 0;
  for (const c of q) {
    let skipped = 0;
    while (j < s.length && s[j] !== c) { j++; skipped++; }
    if (j >= s.length) return null;
    j++;
    gaps += Math.min(50, skipped);
  }
  return Math.max(100, 3000 - gaps * 20);
}

/** `text` split into `{text, hit}` parts where `query` matched (bold in the menu). */
export function highlight(query, text) {
  const s = String(text ?? '');
  if (!query) return [{ text: s, hit: false }];
  const lower = s.toLowerCase();
  const q = query.toLowerCase();
  const at = lower.indexOf(q);
  const marks = new Array(s.length).fill(false);
  if (at >= 0) {
    for (let i = at; i < at + q.length; i++) marks[i] = true;
  } else {
    let j = 0;
    for (const c of q) {
      while (j < lower.length && lower[j] !== c) j++;
      if (j >= lower.length) return [{ text: s, hit: false }];
      marks[j++] = true;
    }
  }
  const parts = [];
  for (let i = 0; i < s.length; i++) {
    const last = parts[parts.length - 1];
    if (last && last.hit === marks[i]) last.text += s[i];
    else parts.push({ text: s[i], hit: marks[i] });
  }
  return parts;
}

// ---- names the menus know ----------------------------------------------------

/** Skill, command, server and plugin names seen in suggestions. */
export const known = createStore({ skills: [], commands: [], servers: [], plugins: [], rev: 0 });

/** Remembers the names of one suggest answer, for pills. */
export function learn(answer) {
  const add = { skills: [], commands: [], servers: [], plugins: [] };
  for (const section of answer?.sections ?? []) {
    for (const item of section.items ?? []) {
      if (item.type === 'skill') add.skills.push(item.name);
      else if (item.type === 'command') add.commands.push(item.name);
      else if (item.type === 'server') add.servers.push(item.name);
      else if (item.type === 'plugin') add.plugins.push(item.name);
    }
  }
  known.set((k) => {
    const merge = (a, b) => (b.every((x) => a.includes(x)) ? a : [...new Set([...a, ...b])]);
    const next = { skills: merge(k.skills, add.skills), commands: merge(k.commands, add.commands), servers: merge(k.servers, add.servers), plugins: merge(k.plugins, add.plugins) };
    const same = Object.keys(next).every((key) => next[key] === k[key]);
    return same ? null : { ...next, rev: k.rev + 1 };
  });
}

const lowerHas = (list, name) => list.some((x) => x.toLowerCase() === name.toLowerCase());

/**
 * Pill ranges of a prompt: `/skill` (and `$skill`), `/command`, `@server`,
 * `@plugin` and `@path` (a path has `/` or `.`, or is quoted). Each is
 * `{start, end, kind, name, label}`.
 */
export function pills(text, names = known.get()) {
  const out = [];
  if (!text) return out;
  const re = /@"([^"\n]+)"?|@([^\s@]+)|([/$])([a-z0-9][a-z0-9-]*)/giu;
  let m;
  while ((m = re.exec(text))) {
    const start = m.index;
    const prev = start > 0 ? text[start - 1] : '';
    const end = start + m[0].length;
    const next = text[end] ?? '';
    if (m[3]) {
      // Slash tokens stand alone: start or whitespace before, whitespace or end after.
      if (prev && !SPACE.test(prev)) continue;
      if (next && !SPACE.test(next)) continue;
      const name = m[4];
      if (!NAME.test(name)) continue;
      if (names.skills.includes(name)) out.push({ start, end, kind: 'skill', name, label: name });
      else if (m[3] === '/' && names.commands.includes(name)) out.push({ start, end, kind: 'command', name, label: name });
      continue;
    }
    if (prev && WORD.test(prev)) continue;
    if (m[1] !== undefined) {
      const path = m[1];
      out.push({ start, end, kind: path.endsWith('/') ? 'folder' : 'file', name: path, label: baseName(path) });
      continue;
    }
    let token = m[2];
    // Sentence punctuation after a name is not part of it.
    const trail = /[.,;:!?)\]]+$/.exec(token);
    if (trail && !/[/.]\w/.test(token.slice(0, trail.index + 1))) token = token.slice(0, trail.index);
    const tokenEnd = start + 1 + token.length;
    if (/^[\w-]+$/.test(token) && (lowerHas(names.servers, token) || lowerHas(names.plugins, token))) {
      out.push({ start, end: tokenEnd, kind: lowerHas(names.servers, token) ? 'server' : 'plugin', name: token, label: token });
    } else if (/[/.]/.test(token)) {
      out.push({ start, end: start + m[0].length, kind: token.endsWith('/') ? 'folder' : 'file', name: token, label: baseName(token) });
    }
  }
  return out;
}

/** Last path segment ("src/" → "src", "/a/b.md" → "b.md"). */
export function baseName(path) {
  const parts = String(path).replace(/\/+$/, '').split('/');
  return parts[parts.length - 1] || path;
}

/** dsh `formatFileMention` for an absolute path of an uploaded file. */
export function fileMention(path) {
  return /\s/.test(path) ? `@"${path}"` : `@${path}`;
}

/** Text parts and pills in order, for rendering. */
export function segments(text, names) {
  const out = [];
  let at = 0;
  for (const p of pills(text, names)) {
    if (p.start > at) out.push({ text: text.slice(at, p.start) });
    out.push({ text: text.slice(p.start, p.end), pill: p });
    at = p.end;
  }
  if (at < text.length) out.push({ text: text.slice(at) });
  return out;
}

/** A typed command the composer runs instead of sending: `/view compact`. */
export function typedCommand(text, commands) {
  const m = /^\s*\/([a-z]+)(?:\s+(\S+))?\s*$/i.exec(text);
  if (!m || !commands.includes(m[1].toLowerCase())) return null;
  return { name: m[1].toLowerCase(), arg: m[2] ?? '' };
}
