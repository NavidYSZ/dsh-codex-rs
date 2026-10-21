// Live view of browser and computer work, like the plugin's 0.4 preview: a
// small floating picture while the panel is closed, a full "Live" tab in it.
// Browser frames arrive as JPEG; computer/app steps show their screenshot.
import { useEffect, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t } from '../i18n.js';
import { useStore } from '../store.js';
import { store, cancel, openPanel, hideLive } from '../state.js';
import { IconStop, IconX, IconPanelRight } from '../icons.js';
import { useImage } from './common.js';
import { liveLabel, CursorGlyph, ICONS } from './codex.js';

/** How long a finished step stays up in the floating picture. */
const LINGER_MS = 8000;

function phase(live) {
  if (live.ended) return 'ended';
  return live.active ? 'active' : 'idle';
}

/** Where the cursor sits, as fractions of the picture. */
function cursorOf(live) {
  if (live.cursor && typeof live.cursor.x === 'number') return live.cursor;
  const c = live.meta?.cursor;
  const screen = live.meta?.screen;
  if (c && screen?.width) return { x: c.x / screen.width, y: c.y / screen.height };
  return null;
}

function Picture({ live }) {
  const shot = useImage(live.image ? null : live.attachment);
  const src = live.image ? `data:image/jpeg;base64,${live.image}` : shot;
  const cursor = cursorOf(live);
  if (!src) return html`<div class="live-empty">${t.liveWaiting}</div>`;
  return html`<div class="live-pic">
    <img src=${src} alt="" draggable="false" />
    ${cursor ? html`<span class="live-cursor" style=${`left:${cursor.x * 100}%;top:${cursor.y * 100}%`}>
      <span class="cx-glyph"><${CursorGlyph} /></span>
    </span>` : null}
  </div>`;
}

function Caption({ live }) {
  const tool = live.tool ?? 'browser';
  const Icon = ICONS[tool] ?? ICONS.browser;
  const title = tool === 'computer' ? 'Computer' : tool === 'browser' ? 'Browser' : (live.meta?.place || live.args?.app || 'App');
  const place = live.place || (tool === 'browser' ? '' : live.meta?.place || '');
  const step = live.activity || liveLabel(tool, live.args, live.meta?.target);
  const p = phase(live);
  return html`<div class="live-caption">
    <span class=${cx('live-dot', `is-${p}`)} title=${p === 'active' ? t.liveActive : p === 'ended' ? t.liveEnded : t.liveIdle} />
    <span class="live-icon"><${Icon} size=${13} /></span>
    <span class="live-text">
      <span class="live-place">${title}${place && place !== title ? html` · ${place}` : null}</span>
      ${step ? html`<span class="live-step">${step}</span>` : null}
    </span>
  </div>`;
}

function StopButton() {
  const status = useStore(store, (s) => s.status);
  const running = status === 'running' || status === 'waiting-approval';
  if (!running) return null;
  return html`<button type="button" class="live-stop" onClick=${cancel} title=${t.stopAgent}><${IconStop} size=${12} />${t.stop}</button>`;
}

/** The panel's Live tab. */
export function LivePanel() {
  const live = useStore(store, (s) => s.live);
  if (!live) return html`<div class="panel-empty">${t.liveWaiting}</div>`;
  return html`<div class="live-panel">
    <div class="live-bar"><${Caption} live=${live} /><span class="spacer" /><${StopButton} /></div>
    <div class="live-stage"><${Picture} live=${live} /></div>
  </div>`;
}

/** Floating picture while the panel does not show the Live tab. */
export function LiveFloat() {
  const live = useStore(store, (s) => s.live);
  const hidden = useStore(store, (s) => s.liveHidden);
  const panel = useStore(store, (s) => s.panel);
  const [, tick] = useState(0);
  // Re-check once the last step has lingered long enough.
  useEffect(() => {
    if (!live || live.active) return undefined;
    const id = setTimeout(() => tick((n) => n + 1), LINGER_MS + 50);
    return () => clearTimeout(id);
  }, [live?.time, live?.active]);
  if (!live || live.ended) return null;
  if (panel.open && panel.tab === 'live') return null;
  if (hidden && hidden >= (live.time ?? 0) - 1) return null;
  if (!live.active && Date.now() - (live.time ?? 0) > LINGER_MS) return null;
  return html`<div class=${cx('live-float', `is-${phase(live)}`)} role="region" aria-label=${t.live}>
    <${Picture} live=${live} />
    <div class="live-float-top">
      <button type="button" class="live-fbtn" title=${t.showInPanel} aria-label=${t.showInPanel} onClick=${() => openPanel('live')}><${IconPanelRight} size=${13} /></button>
      <button type="button" class="live-fbtn" title=${t.hide} aria-label=${t.hide} onClick=${hideLive}><${IconX} size=${13} /></button>
    </div>
    <div class="live-float-bottom"><${Caption} live=${live} /><${StopButton} /></div>
  </div>`;
}

/** Tab entry for the panel while there is something live in this chat. */
export function useLiveTab() {
  const live = useStore(store, (s) => s.live);
  if (!live) return null;
  return {
    label: live.tool === 'computer' ? 'Computer' : live.tool === 'app' ? (live.meta?.place || 'App') : 'Browser',
    badge: live.active ? html`<span class="live-dot is-active" />` : null,
    body: html`<${LivePanel} />`,
  };
}
