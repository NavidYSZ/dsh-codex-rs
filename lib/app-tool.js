// Model-facing "app" tool: background control of one Mac app window through
// the helper's Accessibility layer (see helper/Sources/AppControl.swift).
import { call } from './cua-client.js';

export const TOOL_NAME = 'app';

export const ACTIONS = ['state', 'click', 'type', 'set_value', 'key', 'scroll', 'menu', 'action', 'list_apps', 'show'];

export const DESCRIPTION = `Operate a Mac app in the BACKGROUND like a person, without taking over the screen. The app stays behind the user's windows, their pointer and keyboard are untouched, and a small floating mini window shows the app live with your cursor and a Stop button. Prefer this tool over "computer" for native apps. Use "browser" for websites.

Workflow:
- Start with action "state" and "app" (name or bundle id, e.g. "Notes", "Rechner", "com.apple.Notes"). It launches the app in the background if needed and returns a screenshot of its window plus an outline: [index] role "title" value="…" @(x,y).
- Act with element indices from the latest outline: "click", "type" (with index it focuses that field first, then inserts "text" at the caret), "set_value" (replace a field's whole value), "scroll", "action" (an action listed for that element, e.g. AXIncrement, AXOpen). Every action returns a fresh state; indices change, so always use the newest outline.
- Menu bar: "menu" with "path" in the app's language, e.g. ["Ablage", "Neue Notiz"]. A one-entry path lists that menu's items. Prefer menus over ⌘ shortcuts, which background apps often ignore.
- "key" sends keys to the app ("Return", "Escape", "Tab", "Down"). Return and Escape use Accessibility when the focused element supports it.
- x/y are screenshot pixels of the app window; use them only when the element is not in the outline. "window" picks another window by id or title. Right-click menus are not available in the background.
- "show" brings the app to the front, only when the user wants to see it. "list_apps" lists running apps.
- Never type passwords or payment data; password fields are refused. System Settings, permission and login dialogs, keychains and password managers are off limits, and the helper refuses them. Ask the user before sending, deleting, buying or publishing anything.
- If a result says the user stopped the agent, stop immediately and ask how to continue.`;

export const INPUT_SCHEMA = {
  type: 'object',
  properties: {
    action: { type: 'string', enum: ACTIONS, description: 'What to do.' },
    app: { type: 'string', description: 'For "state": app name or bundle id. Later actions use the app of the latest state.' },
    window: { type: 'string', description: 'For "state": window id or part of its title (default: main window).' },
    index: { type: 'integer', description: 'Element [index] from the latest outline.' },
    x: { type: 'number', description: 'X in screenshot pixels when no index fits.' },
    y: { type: 'number', description: 'Y in screenshot pixels when no index fits.' },
    text: { type: 'string', description: 'For "type": text to insert.' },
    value: { type: 'string', description: 'For "set_value": the new value.' },
    keys: { type: 'string', description: 'For "key": e.g. "Return", "Escape", "Tab", "Down", "cmd+s".' },
    direction: { type: 'string', enum: ['up', 'down', 'left', 'right'], description: 'For "scroll" (default down).' },
    amount: { type: 'number', description: 'For "scroll": steps (default 1).' },
    path: { type: 'array', items: { type: 'string' }, description: 'For "menu": titles from the menu bar down.' },
    name: { type: 'string', description: 'For "action": the AX action name shown in the outline.' },
    count: { type: 'integer', description: 'For "click": 2 for a double-click.' },
  },
  required: ['action'],
  additionalProperties: false,
};

const clip = (s, n) => (s.length > n ? `${s.slice(0, n)}…` : s);

// Element titles of the latest outline, so transcript rows can name targets.
const lastNames = new Map();
function remember(tree) {
  lastNames.clear();
  for (const m of String(tree ?? '').matchAll(/^\[(\d+)\] \S+ "([^"]*)"/gm)) lastNames.set(Number(m[1]), m[2]);
}

export function describe(input) {
  const at = typeof input.index === 'number' ? ` [${input.index}]` : typeof input.x === 'number' ? ` (${Math.round(input.x)}, ${Math.round(input.y)})` : '';
  switch (input.action) {
    case 'state': return `Look at ${input.app ?? 'app'}`;
    case 'type': return `Type “${clip(input.text ?? '', 40)}”${at}`;
    case 'set_value': return `Set value${at}`;
    case 'key': return `Press ${input.keys ?? ''}`;
    case 'menu': return `Menu ${(input.path ?? []).join(' › ')}`;
    case 'action': return `${input.name ?? 'Action'}${at}`;
    case 'scroll': return `Scroll ${input.direction ?? 'down'}${at}`;
    case 'list_apps': return 'List apps';
    case 'show': return 'Show app';
    default: return `${input.action[0].toUpperCase()}${input.action.slice(1)}${at}`;
  }
}

const VERBS = {
  de: { state: 'Sieht sich das Fenster an', click: 'Klickt', type: 'Tippt', set_value: 'Trägt ein', key: 'Drückt',
    scroll: 'Scrollt', menu: 'Menü', action: 'Bedient', show: 'Holt die App nach vorn' },
  en: { state: 'Looking at the window', click: 'Clicking', type: 'Typing', set_value: 'Filling in', key: 'Pressing',
    scroll: 'Scrolling', menu: 'Menu', action: 'Operating', show: 'Bringing the app forward' },
};

function activity(input, locale) {
  const verb = (VERBS[locale] ?? VERBS.en)[input.action] ?? input.action;
  const q = locale === 'de' ? (s) => `„${s}“` : (s) => `“${s}”`;
  if (input.action === 'type') return `${verb} ${q(clip(input.text ?? '', 36))}`;
  if (input.action === 'key') return `${verb} ${input.keys ?? ''}`;
  if (input.action === 'menu') return `${verb} ${(input.path ?? []).join(' › ')}`;
  return verb;
}

function fail(res) {
  const err = new Error(res.error || 'app control failed');
  err.cancelled = !!res.cancelled;
  throw err;
}

function stateText(s, header) {
  const lines = [header].filter(Boolean);
  lines.push(`App: ${s.app} (${s.bundleId})${s.frontmost ? ', in front' : ', in the background'}`);
  lines.push(`Window ${s.window.id}: "${s.window.title || '(untitled)'}" ${s.width}×${s.height} px screenshot`);
  if (s.otherWindows?.length) lines.push(`Other windows: ${s.otherWindows.join(' | ')}`);
  lines.push('Outline, [index] role "title" @(x,y) in screenshot pixels:');
  lines.push(s.tree || '(empty)');
  return lines.join('\n');
}

/** Runs one action; returns { text, image?, cursor?, app?, screen? }. */
export async function run(input, settings = {}) {
  const { locale = 'en', maxWidth = 1280, quality = 0.8, settle = 0.6 } = settings;
  const common = { activity: activity(input, locale), maxWidth, quality };
  let request;
  switch (input.action) {
    case 'list_apps': {
      const res = await call({ cmd: 'app_list' });
      if (!res.ok) fail(res);
      return { text: 'Running apps:\n' + res.apps.map((a) => `${a.name} (${a.bundleId})${a.active ? ', in front' : ''}${a.hidden ? ', hidden' : ''}`).join('\n') };
    }
    case 'state':
      if (!input.app) throw new Error('"state" needs "app"');
      request = { cmd: 'app_state', app: input.app, ...(input.window ? { window: input.window } : {}), ...common };
      break;
    case 'click':
      request = { cmd: 'app_click', index: input.index, x: input.x, y: input.y, count: input.count ?? 1, ...common };
      break;
    case 'type':
      if (typeof input.text !== 'string') throw new Error('"type" needs "text"');
      request = { cmd: 'app_type', index: input.index, text: input.text, ...common };
      break;
    case 'set_value':
      request = { cmd: 'app_set_value', index: input.index, value: input.value ?? '', ...common };
      break;
    case 'key':
      if (!input.keys) throw new Error('"key" needs "keys"');
      request = { cmd: 'app_key', keys: input.keys, ...common };
      break;
    case 'scroll':
      request = { cmd: 'app_scroll', index: input.index, x: input.x, y: input.y, direction: input.direction ?? 'down', amount: input.amount ?? 1, ...common };
      break;
    case 'menu': {
      const path = Array.isArray(input.path) ? input.path : [];
      request = { cmd: 'app_menu', path, press: path.length > 1, ...common };
      break;
    }
    case 'action':
      request = { cmd: 'app_action', index: input.index, action: input.name ?? '', ...common };
      break;
    case 'show':
      request = { cmd: 'app_reveal', ...common };
      break;
    default:
      throw new Error(`unknown action "${input.action}"`);
  }
  if (input.action !== 'state') request.stateAfter = true;
  request.settle = settle;

  const target = typeof input.index === 'number' ? lastNames.get(input.index) ?? null : null;
  const res = await call(request, { timeoutMs: 90_000 });
  if (!res.ok) fail(res);
  const state = input.action === 'state' ? res : res.state;
  if (state?.tree) remember(state.tree);
  let header = '';
  if (res.items) header = `Menu ${(input.path ?? []).join(' › ') || 'bar'} contains: ${res.items.join(' | ')}`;
  else if (input.action !== 'state') header = `${describe(input)} — done via ${res.method}.`;
  if (!state) return { text: header };
  return {
    text: stateText(state, header),
    image: { mime: state.mime, data: state.data, width: state.width, height: state.height },
    cursor: res.aim ?? null,
    target,
    app: state.app,
    window: state.window?.title ?? '',
  };
}
