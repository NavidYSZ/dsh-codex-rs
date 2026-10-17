// Model-facing "computer" tool, independent of the DSH tool API: schema,
// description and the mapping from tool input to helper commands.
import { call } from './cua-client.js';

export const TOOL_NAME = 'computer';

export const ACTIONS = [
  'screenshot', 'click', 'double_click', 'right_click', 'move', 'drag',
  'scroll', 'type', 'key', 'open_app', 'wait', 'cursor_position',
];

export const DESCRIPTION = `Operate the user's whole Mac screen like a person: look at the screen, move a visible agent cursor, click, type and press keys. This takes over the user's real mouse and keyboard while it runs, so prefer "browser" for websites and "app" for a single native app; both work in the background. Use "computer" when those cannot do the job (several apps at once, drag and drop, context menus) or when the user wants to watch on their own screen.

Workflow:
- Start with action "screenshot". All x/y coordinates are pixels in the most recent screenshot.
- Every pointer, keyboard and app action returns a fresh screenshot of the result, so you rarely need a separate screenshot call.
- Prefer keyboard shortcuts and "open_app" over hunting for icons. Use "key" for shortcuts ("cmd+l", "Return", "cmd+shift+t") and "type" for text.
- Click the centre of controls. After typing into a search or address field, press "Return" with "key".
- Scroll with dy > 0 to move content down (reveal what is below), dy < 0 to go up.
- The user sees your cursor and can press Esc at any time; if a result says the user cancelled, stop immediately and ask how to continue.
- Never enter passwords, payment data or other secrets, and ask before sending, buying, deleting or publishing anything.
- macOS permission prompts ("Allow"/"Erlauben"), login or Touch ID dialogs, System Settings, keychains and password managers are off limits: never click or type there. If one appears or blocks you, stop and ask the user to handle it. The helper refuses such targets anyway.`;

export const INPUT_SCHEMA = {
  type: 'object',
  properties: {
    action: { type: 'string', enum: ACTIONS, description: 'What to do.' },
    x: { type: 'number', description: 'X in screenshot pixels (click, move, drag start, scroll).' },
    y: { type: 'number', description: 'Y in screenshot pixels.' },
    to_x: { type: 'number', description: 'Drag end X.' },
    to_y: { type: 'number', description: 'Drag end Y.' },
    text: { type: 'string', description: 'Text for "type"; key combination for "key" (e.g. "cmd+c", "Return", "Escape"); app name or bundle id for "open_app".' },
    modifiers: { type: 'array', items: { type: 'string', enum: ['cmd', 'shift', 'alt', 'ctrl'] }, description: 'Modifier keys held during a click.' },
    dx: { type: 'number', description: 'Horizontal scroll in pixels.' },
    dy: { type: 'number', description: 'Vertical scroll in pixels (positive = further down the page).' },
    seconds: { type: 'number', description: 'For "wait": seconds to wait (max 10).' },
  },
  required: ['action'],
  additionalProperties: false,
};

const POINTER = new Set(['click', 'double_click', 'right_click', 'move', 'drag', 'scroll']);

function need(input, ...keys) {
  for (const k of keys) {
    if (typeof input[k] !== 'number' || Number.isNaN(input[k])) throw new Error(`"${input.action}" needs numeric "${k}"`);
  }
}

/** Human-readable one-liner, used for the transcript row. */
export function describe(input) {
  const at = typeof input.x === 'number' ? ` (${Math.round(input.x)}, ${Math.round(input.y)})` : '';
  switch (input.action) {
    case 'screenshot': return 'Screenshot';
    case 'click': return `Click${at}`;
    case 'double_click': return `Double-click${at}`;
    case 'right_click': return `Right-click${at}`;
    case 'move': return `Move pointer${at}`;
    case 'drag': return `Drag${at} → (${Math.round(input.to_x)}, ${Math.round(input.to_y)})`;
    case 'scroll': return `Scroll ${(input.dy ?? 0) > 0 ? 'down' : 'up'}${at}`;
    case 'type': return `Type “${(input.text ?? '').slice(0, 40)}${(input.text ?? '').length > 40 ? '…' : ''}”`;
    case 'key': return `Press ${input.text}`;
    case 'open_app': return `Open ${input.text}`;
    case 'wait': return `Wait ${input.seconds ?? 1}s`;
    case 'cursor_position': return 'Cursor position';
    default: return input.action;
  }
}

const ACTIVITY = {
  de: {
    click: 'Klickt', double_click: 'Doppelklickt', right_click: 'Rechtsklick', move: 'Bewegt den Zeiger',
    drag: 'Zieht', scroll: 'Scrollt', type: 'Tippt …', key: 'Drückt', open_app: 'Öffnet', wait: 'Wartet',
  },
  en: {
    click: 'Clicking', double_click: 'Double-clicking', right_click: 'Right-clicking', move: 'Moving',
    drag: 'Dragging', scroll: 'Scrolling', type: 'Typing …', key: 'Pressing', open_app: 'Opening', wait: 'Waiting',
  },
};

/** Short status shown in the pill next to the on-screen agent cursor. */
export function activityLabel(input, locale = 'en') {
  const words = ACTIVITY[locale] ?? ACTIVITY.en;
  const verb = words[input.action];
  if (!verb) return undefined;
  if (input.action === 'key' || input.action === 'open_app') return `${verb} ${input.text ?? ''}`.trim();
  return verb;
}

/**
 * Runs one action. Returns { text, image?, cursor?, screen? } where image is
 * { mime, data(base64), width, height }.
 */
export async function run(input, settings = {}) {
  const { maxWidth = 1440, quality = 0.8, settle = 0.5, signal, locale = 'en' } = settings;
  const activity = activityLabel(input, locale);
  const after = { screenshotAfter: true, settle, maxWidth, quality, ...(activity ? { activity } : {}) };
  let request;
  switch (input.action) {
    case 'screenshot':
      request = { cmd: 'screenshot', maxWidth, quality };
      break;
    case 'click':
    case 'double_click':
    case 'right_click':
      need(input, 'x', 'y');
      request = {
        cmd: 'click', x: input.x, y: input.y,
        button: input.action === 'right_click' ? 'right' : 'left',
        count: input.action === 'double_click' ? 2 : 1,
        modifiers: input.modifiers ?? [], ...after,
      };
      break;
    case 'move':
      need(input, 'x', 'y');
      request = { cmd: 'move', x: input.x, y: input.y, ...after };
      break;
    case 'drag':
      need(input, 'x', 'y', 'to_x', 'to_y');
      request = { cmd: 'drag', x: input.x, y: input.y, toX: input.to_x, toY: input.to_y, ...after };
      break;
    case 'scroll':
      need(input, 'x', 'y');
      request = { cmd: 'scroll', x: input.x, y: input.y, dx: Math.round(input.dx ?? 0), dy: Math.round(input.dy ?? 0), ...after };
      break;
    case 'type':
      if (typeof input.text !== 'string') throw new Error('"type" needs "text"');
      request = { cmd: 'type', text: input.text, ...after };
      break;
    case 'key':
      if (typeof input.text !== 'string') throw new Error('"key" needs "text", e.g. "cmd+c"');
      request = { cmd: 'key', keys: input.text, ...after };
      break;
    case 'open_app':
      if (typeof input.text !== 'string') throw new Error('"open_app" needs "text" with the app name');
      request = { cmd: 'open_app', name: input.text, ...after, settle: 1.0 };
      break;
    case 'wait': {
      const ms = Math.min(10, Math.max(0, input.seconds ?? 1)) * 1000;
      await new Promise((resolve) => setTimeout(resolve, ms));
      request = { cmd: 'screenshot', maxWidth, quality };
      break;
    }
    case 'cursor_position':
      request = { cmd: 'cursor_position' };
      break;
    default:
      throw new Error(`unknown action "${input.action}"`);
  }

  if (signal?.aborted) throw new Error('aborted');
  const res = await call(request);
  if (!res.ok) {
    const err = new Error(res.error || 'computer-use helper failed');
    err.cancelled = !!res.cancelled;
    throw err;
  }

  const shot = request.cmd === 'screenshot' ? res : res.screenshot;
  const cursor = res.cursor ?? shot?.cursor;
  const lines = [`${describe(input)} — done.`];
  if (shot) {
    lines.push(`Screenshot ${shot.width}×${shot.height}px of the display` +
      (shot.frontmostApp ? `; frontmost app: ${shot.frontmostApp}` : '') + '.');
  }
  if (cursor) lines.push(`Agent cursor at (${cursor.x}, ${cursor.y}).`);
  return {
    text: lines.join(' '),
    image: shot?.data ? { mime: shot.mime, data: shot.data, width: shot.width, height: shot.height } : undefined,
    cursor,
    pointer: POINTER.has(input.action),
  };
}

export async function status() {
  return call({ cmd: 'status' });
}

export async function configure(options) {
  return call({ cmd: 'configure', ...options });
}
