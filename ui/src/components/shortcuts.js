// Codex's keyboard shortcuts for chats (⌘1–9, previous/next, the next chat
// that needs you, archive, pin, rename, mark all read, find) and the ⌘/
// overview. The Mac app's menus send the same commands. Plain keys never
// fire while typing; ⌘ (Ctrl elsewhere) combinations do.
import { html } from '../html.js';
import { t } from '../i18n.js';
import { useStore } from '../store.js';
import {
  store, openSession, setArchived, setPinned, startRename, markAllRead, toast,
} from '../state.js';
import { Dialog } from './models.js';
import { openFind } from './find.js';

export const MAC = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);

const SYMBOLS = MAC ? { mod: '⌘', alt: '⌥', shift: '⇧', ctrl: '⌃' } : { mod: 'Ctrl', alt: 'Alt', shift: 'Shift', ctrl: 'Ctrl' };
const ORDER = ['ctrl', 'alt', 'shift', 'mod'];

/** `['mod', 'shift', 'A']` → "⇧⌘A" on a Mac, "Ctrl+Shift+A" elsewhere. */
export function keyLabel(keys) {
  const mods = ORDER.filter((m) => keys.includes(m)).map((m) => SYMBOLS[m]);
  const rest = keys.filter((k) => !ORDER.includes(k));
  return MAC ? [...mods, ...rest].join('') : [...new Set(mods), ...rest].join('+');
}

// ---- chats in sidebar order ---------------------------------------------------------

const enc = encodeURIComponent;

/** Chat ids as the sidebar lists them (its view, folds and "show more" included). */
function sidebarOrder() {
  const ids = [...document.querySelectorAll('.sb-list .sb-row a.sb-link[href^="#/s/"]')]
    .map((a) => decodeURIComponent(a.getAttribute('href').slice(4)));
  return [...new Set(ids)];
}

function step(d) {
  const ids = sidebarOrder();
  if (!ids.length) return;
  const at = ids.indexOf(store.get().activeId);
  const next = at < 0 ? (d > 0 ? 0 : ids.length - 1) : (at + d + ids.length) % ids.length;
  openSession(ids[next]);
}

/** Codex ⌘⌥A: the next chat waiting for an approval, else the next unread one. */
function nextAttention() {
  const { sessions, activeId } = store.get();
  const shown = sidebarOrder();
  const rank = (s) => { const i = shown.indexOf(s.id); return i < 0 ? shown.length : i; };
  const live = sessions.filter((s) => !s.archived && s.id !== activeId)
    .sort((a, b) => rank(a) - rank(b) || (b.updatedAt ?? 0) - (a.updatedAt ?? 0));
  const pick = live.find((s) => s.status === 'waiting-approval') ?? live.find((s) => s.unread);
  if (pick) openSession(pick.id);
  else toast(t.nothingWaiting);
}

/** The open chat's sidebar row, or null when there is none. */
function openChat() {
  const s = store.get();
  if (s.view !== 'chat' || !s.activeId) { toast(t.noChatOpen); return null; }
  return s.sessions.find((x) => x.id === s.activeId) ?? null;
}

/** Renames in the sidebar row when it is on screen, else in the chat's title. */
function rename(row) {
  const link = document.querySelector(`.sb-list a.sb-link[href="#/s/${enc(row.id)}"]`);
  const box = link?.getBoundingClientRect();
  if (box && box.width > 0 && box.left >= 0) startRename(row.id);
  else store.set({ renameTitle: row.id });
}

export const COMMANDS = {
  find: () => openFind(),
  shortcuts: () => store.set((s) => ({ shortcuts: !s.shortcuts })),
  'previous-chat': () => step(-1),
  'next-chat': () => step(1),
  'next-attention': nextAttention,
  'mark-all-read': markAllRead,
  'archive-chat': () => { const row = openChat(); if (row) setArchived(row.id, !row.archived); },
  'pin-chat': () => { const row = openChat(); if (row) setPinned(row.id, !row.pinned); },
  'rename-chat': () => { const row = openChat(); if (row) rename(row); },
};

/** A letter by its character; with ⌥, which turns it into another one on a Mac, by its key. */
const key = (e, letter) => e.key.toLowerCase() === letter || (e.altKey && e.code === `Key${letter.toUpperCase()}`);
const typing = (el) => el instanceof HTMLElement && (el.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(el.tagName));
const modal = () => document.querySelector('.md-scrim, .palette-scrim');

// A key the page handled can still reach the Mac app's menu; run it once.
let last = { command: null, time: 0 };
function run(command, fromMenu) {
  if (fromMenu && last.command === command && Date.now() - last.time < 400) return;
  last = { command, time: Date.now() };
  COMMANDS[command]();
}

function onKey(e) {
  if (e.defaultPrevented || e.isComposing) return;
  // ⌘ on a Mac, Ctrl elsewhere; ⌃1–9 switch the Mac app's instances.
  const mod = MAC ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
  let command = null;
  // "/" needs Shift on some layouts (German: ⇧7).
  if (mod && !e.altKey && e.key === '/') command = 'shortcuts';
  else if (modal()) return;
  else if (mod && !e.altKey && !e.shiftKey && /^[1-9]$/.test(e.key)) {
    e.preventDefault();
    const id = sidebarOrder()[Number(e.key) - 1];
    if (id) openSession(id);
    return;
  } else if (mod && !e.altKey && !e.shiftKey && key(e, 'f')) command = 'find';
  else if (mod && e.shiftKey && !e.altKey && key(e, 'a')) command = 'archive-chat';
  else if (mod && e.altKey && !e.shiftKey && key(e, 'p')) command = 'pin-chat';
  else if (mod && e.altKey && !e.shiftKey && key(e, 'r')) command = 'rename-chat';
  else if (mod && e.altKey && !e.shiftKey && key(e, 'a')) command = 'next-attention';
  else if (mod && e.shiftKey && !e.altKey && (e.code === 'BracketLeft' || e.key === '[' || e.key === '{')) command = 'previous-chat';
  else if (mod && e.shiftKey && !e.altKey && (e.code === 'BracketRight' || e.key === ']' || e.key === '}')) command = 'next-chat';
  else if (e.ctrlKey && !e.metaKey && !e.altKey && e.key === 'Tab') command = e.shiftKey ? 'previous-chat' : 'next-chat';
  else if (e.key === 'Escape' && e.shiftKey && !mod && !e.altKey && !typing(e.target) && !document.querySelector('.menu')) command = 'mark-all-read';
  if (!command) return;
  e.preventDefault();
  run(command, false);
}

window.addEventListener('keydown', onKey);
// The Mac app's menu items for the same commands.
window.addEventListener('dshx-desktop-command', (e) => {
  const command = e.detail?.command;
  if (!COMMANDS[command]) return;
  e.preventDefault();
  run(command, true);
});

// ---- ⌘/ overview ----------------------------------------------------------------------

const GROUPS = () => [
  ['general', [
    ['palette', ['mod', 'K']], ['newChat', ['mod', 'shift', 'O']], ['openProject', ['mod', 'O']],
    ['sidebar', ['mod', 'B']], ['shortcuts', ['mod', '/']],
  ]],
  ['chats', [
    ['nthChat', ['mod', '1…9']], ['prevChat', ['mod', 'shift', '['], ['ctrl', 'shift', 'Tab']], ['nextChat', ['mod', 'shift', ']'], ['ctrl', 'Tab']],
    ['attention', ['mod', 'alt', 'A']], ['allRead', ['shift', 'Esc']], ['archive', ['mod', 'shift', 'A']],
    ['pin', ['mod', 'alt', 'P']], ['rename', ['mod', 'alt', 'R']],
  ]],
  ['chat', [
    ['find', ['mod', 'F']], ['send', ['↩']], ['newline', ['shift', '↩']], ['other', ['mod', '↩']], ['close', ['Esc']],
  ]],
];

function Overview() {
  const close = () => store.set({ shortcuts: false });
  return html`<${Dialog} title=${t.shortcuts} class="sc-dialog" onClose=${close}>
    <div class="sc-grid">
      ${GROUPS().map(([group, rows]) => html`<section class="sc-group" key=${group} aria-label=${t.shortcutGroups[group]}>
        <h3 class="sc-head">${t.shortcutGroups[group]}</h3>
        ${rows.map(([name, ...combos]) => html`<div class="sc-row" key=${name}>
          <span class="sc-label">${t.sc[name]}</span>
          <span class="sc-keys">${combos.map((keys) => html`<kbd class="kbd" key=${keys.join()}>${keyLabel(keys)}</kbd>`)}</span>
        </div>`)}
      </section>`)}
    </div>
  </${Dialog}>`;
}

/** Mounted once by the app shell. */
export function ShortcutsDialog() {
  const open = useStore(store, (s) => !!s.shortcuts);
  return open ? html`<${Overview} />` : null;
}
