// Transcript rows for the Mac tools "computer", "app" and "browser":
// a one-line label plus a screenshot preview in which the agent cursor
// glides to its target. Ported from the plugin's client.js.
import { useEffect, useRef, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { de, quote, clip } from '../i18n.js';
import { ARROW, IconGlobe, IconPointer, IconWindow } from '../icons.js';
import { Shimmer, useImage } from './common.js';

const T = de
  ? { title: 'Computer', screenshot: 'Bildschirm ansehen', click: 'Klick', double: 'Doppelklick', right: 'Rechtsklick',
      move: 'Zeiger bewegen', drag: 'Ziehen', scrollDown: 'Nach unten scrollen', scrollUp: 'Nach oben scrollen',
      type: 'Tippt', key: 'Taste', open: 'Öffnet', wait: 'Wartet', cursor: 'Zeigerposition', failed: 'Fehlgeschlagen', into: 'in' }
  : { title: 'Computer', screenshot: 'Look at screen', click: 'Click', double: 'Double-click', right: 'Right-click',
      move: 'Move pointer', drag: 'Drag', scrollDown: 'Scroll down', scrollUp: 'Scroll up',
      type: 'Type', key: 'Press', open: 'Open', wait: 'Wait', cursor: 'Pointer position', failed: 'Failed', into: 'into' };

const B = de
  ? { open: 'Öffnet', state: 'Seite ansehen', windowState: 'Fenster ansehen', read: 'Seite lesen', back: 'Zurück',
      forward: 'Vor', reload: 'Neu laden', tabs: 'Tabs ansehen', newTab: 'Neuer Tab', tab: 'Tab', closeTab: 'Tab schließen',
      select: 'Auswahl', setValue: 'Trägt ein', menu: 'Menü', apps: 'Apps auflisten', showApp: 'Nach vorn holen',
      showBrowser: 'Browser zeigen', hideBrowser: 'Zurück in den Hintergrund', scroll: 'Scrollen' }
  : { open: 'Open', state: 'Look at page', windowState: 'Look at window', read: 'Read page', back: 'Back',
      forward: 'Forward', reload: 'Reload', tabs: 'Tabs', newTab: 'New tab', tab: 'Tab', closeTab: 'Close tab',
      select: 'Select', setValue: 'Fill in', menu: 'Menu', apps: 'List apps', showApp: 'Bring to front',
      showBrowser: 'Show browser', hideBrowser: 'Back to background', scroll: 'Scroll' };

/** Label for the background tools (app windows and the headless browser). */
function bgLabel(tool, args, target) {
  const at = target ? ` ${quote(clip(target, 48))}` : typeof args.index === 'number' ? ` [${args.index}]`
    : typeof args.x === 'number' ? ` (${Math.round(args.x)}, ${Math.round(args.y)})` : '';
  const text = typeof args.text === 'string' ? args.text : '';
  const url = String(args.url ?? '').replace(/^https?:\/\//, '').replace(/\/$/, '');
  switch (args.action) {
    case 'open': return `${B.open} ${clip(url, 60)}`;
    case 'state': return tool === 'app' ? B.windowState : B.state;
    case 'click': return ((args.count ?? 1) > 1 ? T.double : T.click) + at;
    case 'type': return `${T.type} ${quote(clip(text, 48))}${target ? ` ${T.into} ${quote(clip(target, 40))}` : at}`;
    case 'key': return `${T.key} ${args.keys ?? ''}`;
    case 'scroll': return `${B.scroll}${at}`;
    case 'select': return `${B.select} ${quote(clip(String(args.value ?? ''), 40))}${at}`;
    case 'set_value': return `${B.setValue}${at}`;
    case 'menu': return `${B.menu} ${(args.path ?? []).join(' › ')}`;
    case 'action': return `${args.name ?? ''}${at}`;
    case 'read': return B.read;
    case 'back': return B.back;
    case 'forward': return B.forward;
    case 'reload': return B.reload;
    case 'tabs': return B.tabs;
    case 'new_tab': return `${B.newTab} ${clip(url, 50)}`;
    case 'switch_tab': return `${B.tab} ${args.tab ?? ''}`;
    case 'close_tab': return `${B.closeTab} ${args.tab ?? ''}`;
    case 'wait': return `${T.wait} ${args.seconds ?? 1}s`;
    case 'list_apps': return B.apps;
    case 'show': return tool === 'app' ? B.showApp : B.showBrowser;
    case 'hide': return B.hideBrowser;
    default: return args.action ?? '';
  }
}

/** Label for the foreground "computer" tool. */
function label(args) {
  const at = typeof args.x === 'number' ? ` (${Math.round(args.x)}, ${Math.round(args.y)})` : '';
  const text = typeof args.text === 'string' ? args.text : '';
  switch (args.action) {
    case 'screenshot': return T.screenshot;
    case 'click': return T.click + at;
    case 'double_click': return T.double + at;
    case 'right_click': return T.right + at;
    case 'move': return T.move + at;
    case 'drag': return `${T.drag}${at} → (${Math.round(args.to_x)}, ${Math.round(args.to_y)})`;
    case 'scroll': return ((args.dy ?? 0) > 0 ? T.scrollDown : T.scrollUp) + at;
    case 'type': return `${T.type} ${quote(clip(text, 48))}`;
    case 'key': return `${T.key} ${text}`;
    case 'open_app': return `${T.open} ${text}`;
    case 'wait': return `${T.wait} ${args.seconds ?? 1}s`;
    case 'cursor_position': return T.cursor;
    default: return args.action ?? '';
  }
}

const CursorGlyph = () => html`<svg viewBox="0 0 22 22" aria-hidden="true">
  <path d=${ARROW} fill="#0d0d0d" stroke="#fff" stroke-width="2" stroke-linejoin="round" paint-order="stroke"/>
</svg>`;

const ICONS = { computer: IconPointer, app: IconWindow, browser: IconGlobe };

// Codex-like motion in screenshot pixel space: a bowed path for long
// moves with the tip turning into the direction of travel, a tilted
// scoot for short ones, speed squash, then a short "thinking" wiggle.
const REST = -135; // the glyph's tip points up-left
const easeMove = (t) => (t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2);
const wrap = (a) => ((a + 540) % 360) - 180;
const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));

function glide(span, screen, from, to, onArrive) {
  const glyph = span.firstChild;
  const put = (p, rot, along, squash) => {
    span.style.left = `${(p.x / screen.width) * 100}%`;
    span.style.top = `${(p.y / screen.height) * 100}%`;
    glyph.style.transform = `rotate(${along}deg) scale(${squash}, 1) rotate(${-along}deg) rotate(${rot}deg)`;
  };
  let raf = 0;
  const t0 = performance.now();

  function wiggle() {
    const w0 = performance.now();
    const step = (now) => {
      const t = (now - w0) / 1000;
      if (t >= 1.41) { put(to, 0, 0, 1); return; }
      put(to, 12.5 * Math.sin((t / 0.66) * 2 * Math.PI) * Math.sin((Math.PI * t) / 1.41), 0, 1);
      raf = requestAnimationFrame(step);
    };
    return requestAnimationFrame(step);
  }

  if (!from) {
    // Appear: fade, grow from 40 % and un-blur, like the Codex cursor.
    const tick = (now) => {
      const t = clamp((now - t0) / 320, 0, 1);
      const e = 1 - Math.pow(1 - t, 3);
      put(to, 0, 0, 1);
      glyph.style.opacity = String(e);
      glyph.style.filter = `blur(${(1 - e) * 5}px)`;
      glyph.style.scale = String(0.4 + 0.6 * e);
      if (t < 1) raf = requestAnimationFrame(tick);
      else { glyph.style.filter = ''; glyph.style.scale = ''; onArrive?.(); raf = wiggle(); }
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }

  const dx = to.x - from.x, dy = to.y - from.y;
  const dist = Math.hypot(dx, dy) || 1;
  const nx = dx / dist, ny = dy / dist;
  const long = dist > 196;
  const bow = long ? Math.min(120, dist * 0.16) : 0;
  const ctrl = { x: (from.x + to.x) / 2 - ny * bow, y: (from.y + to.y) / 2 + nx * bow };
  const dur = clamp(260 + dist * 0.5, 320, 900);
  const at = (t) => {
    const u = 1 - t;
    return { x: u * u * from.x + 2 * u * t * ctrl.x + t * t * to.x, y: u * u * from.y + 2 * u * t * ctrl.y + t * t * to.y };
  };
  let prev = from, prevT = t0;

  const tick = (now) => {
    const t = clamp((now - t0) / dur, 0, 1);
    const p = at(easeMove(t));
    const dt = Math.max(1, now - prevT) / 1000;
    const speed = Math.hypot(p.x - prev.x, p.y - prev.y) / dt;
    const weight = Math.sin(Math.PI * t);
    let rot, along;
    if (long) {
      const tx = 2 * (1 - t) * (ctrl.x - from.x) + 2 * t * (to.x - ctrl.x);
      const ty = 2 * (1 - t) * (ctrl.y - from.y) + 2 * t * (to.y - ctrl.y);
      along = (Math.atan2(ty, tx) * 180) / Math.PI;
      rot = wrap(along - REST) * weight;
    } else {
      along = (Math.atan2(ny, nx) * 180) / Math.PI;
      rot = clamp(nx * 0.75 - ny * 0.62, -1, 1) * 70 * weight;
    }
    const squash = long ? clamp(1 - speed / 5500, 0.65, 1) : 1 - 0.15 * weight;
    put(p, rot, along, squash);
    prev = p; prevT = now;
    if (t < 1) raf = requestAnimationFrame(tick);
    else { put(to, 0, 0, 1); onArrive?.(); raf = wiggle(); }
  };
  put(from, 0, 0, 1);
  raf = requestAnimationFrame(tick);
  return () => cancelAnimationFrame(raf);
}

function Shot({ url, screen, from, to, click, pending, still }) {
  const [open, setOpen] = useState(false);
  const [pressed, setPressed] = useState(0);
  const span = useRef(null);
  useEffect(() => {
    if (!to || !span.current) return undefined;
    const reduce = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches;
    if (still || reduce) {
      span.current.style.left = `${(to.x / screen.width) * 100}%`;
      span.current.style.top = `${(to.y / screen.height) * 100}%`;
      return undefined;
    }
    return glide(span.current, screen, from, to, () => { if (click) setPressed((n) => n + 1); });
  }, [to?.x, to?.y, from?.x, from?.y, screen.width, screen.height, still]);
  return html`<div class="cx-shot" data-open=${open || undefined} data-pending=${pending || undefined}
      onClick=${() => setOpen((v) => !v)}>
    <img src=${url} alt="" draggable="false" />
    ${to ? html`<span class="cx-cursor" ref=${span}>
      <span class="cx-glyph"><${CursorGlyph} /></span>
      ${pressed ? html`<span class="cx-ripple" key=${pressed} />` : null}
    </span>` : null}
  </div>`;
}

/** Only steps that just happened animate; replayed history sits still. */
const FRESH_MS = 20_000;

export function CodexRow({ item }) {
  const tool = item.name;
  const { args, result } = item;
  const meta = result?.meta ?? null;
  const image = result?.content?.find?.((c) => c.type === 'image')?.attachment ?? null;
  const url = useImage(image);
  const prev = item.prevShot;
  const prevUrl = useImage(prev?.image);

  const target = typeof args.x === 'number' ? { x: args.x, y: args.y } : null;
  const end = args.action === 'drag' && typeof args.to_x === 'number' ? { x: args.to_x, y: args.to_y } : target;
  const screen = meta?.screen ?? prev?.screen ?? null;
  // Background tools aim at elements; the host reports where it clicked.
  const aimed = tool === 'computer' ? (meta?.cursor ?? end) : (meta?.cursor ?? target);

  const running = !result;
  const failed = !!result?.isError;
  const errorText = failed
    ? (result.error?.reason || result.content?.find?.((c) => c.type === 'text')?.text || T.failed)
    : null;
  const place = meta?.place || (tool === 'app' ? args.app || prev?.place : '') || '';
  const title = tool === 'computer' ? T.title : tool === 'browser' ? 'Browser' : (place || 'App');
  const action = tool === 'computer' ? label(args) : bgLabel(tool, args, meta?.target);
  const summary = tool === 'browser' && place && !['open', 'new_tab'].includes(args.action) ? `${action} · ${place}` : action;
  const clicks = /click|select|type|action/.test(args.action ?? '');
  const fresh = !result || typeof result.time !== 'number' || Date.now() - result.time < FRESH_MS;

  let shot = null;
  if (url && screen) {
    // Finished step: glide from where the previous step left the cursor
    // (only within the same app or site; another place starts fresh).
    const from = prev && (tool === 'computer' || prev.place === meta?.place) ? prev.cursor : null;
    shot = html`<${Shot} key="done" url=${url} screen=${screen} from=${from} to=${aimed} click=${clicks && !!aimed} still=${!fresh} />`;
  } else if (running && end && prev && prevUrl) {
    // Running step: replay the motion on the last frame we have.
    shot = html`<${Shot} key="live" url=${prevUrl} screen=${prev.screen} from=${prev.cursor} to=${end} click=${clicks} pending />`;
  }

  const Icon = ICONS[tool];
  return html`<div class="cx-card" data-tool=${tool} data-state=${running ? 'running' : failed ? 'error' : 'done'}>
    <div class="row cx-row">
      <span class="lead"><${Icon} size=${14} /></span>
      <${Shimmer} active=${running}><span class="row-title">${title}</span></${Shimmer}>
      ${summary ? html`<span class="sep" />` : null}
      <span class=${cx('row-summary', failed && 'is-error')} title=${errorText ?? summary}>${failed ? errorText : summary}</span>
    </div>
    ${shot}
  </div>`;
}
