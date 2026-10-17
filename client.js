// Browser half of @yousufzai/dsh-codex: Codex-style tokens and layout for the
// DSH web client, plus the transcript row for the "computer" tool.
// Written directly in DSH's browser module format; no build step.
window.__ModuleLoader__.load({
  id: '@yousufzai/dsh-codex',
  factory: (require) => {
    const React = require('react');
    const { createElement: h, useEffect, useRef, useState } = React;
    const { TextShimmer } = require('@deepseek-ai/dsh-client-ui-primitives');

    const SOURCE = '@yousufzai/dsh-codex';
    const ACCENT = '#339cff';

    // ---- palette ---------------------------------------------------------
    // Values follow Codex 26.1007 (seeds: surface #fff/#181818, ink #1a1c1f,
    // accent #339cff) and Codex's dark user bubble.
    // Each entry is a { light, dark } pair for ctx.theme.overrideTokens.
    const same = (v) => ({ light: v, dark: v });
    const pair = (light, dark) => ({ light, dark });

    // DSH's grey ramp is slightly blue; Codex greys are neutral. Every alias
    // built on these steps follows automatically.
    const RAMP = {
      '00': '#ffffff', '50': '#f9f9f9', '75': '#f3f3f3', '100': '#ededed', '150': '#e8e8e8',
      '200': '#dfdfdf', '300': '#cdcdcd', '400': '#afafaf', '500': '#9f9f9f', '600': '#8f8f8f',
      '700': '#5d5d5d', '750': '#414141', '800': '#363636', '850': '#2d2d2d', '875': '#232323',
      '900': '#1c1c1c', '950': '#181818', '1000': '#1a1c1f',
    };
    const BLUE = {
      '50': '#e5f2ff', '100': '#e5f2ff', '200': '#cce6ff', '300': '#99cdff', '400': '#83c3ff',
      '450': '#339cff', '500': '#339cff', '600': '#0169cc', '700-delete': '#0d4f8f', '800': '#0d273f', '900': '#0a1f33',
    };

    const TOKENS = {
      ...Object.fromEntries(Object.entries(RAMP).map(([k, v]) => [`--dsw-static-neutral-bluish-${k}`, same(v)])),
      '--dsw-static-neutral-bluish-60': pair('#f6f6f6', '#f9f9f9'),
      ...Object.fromEntries(Object.entries(BLUE).map(([k, v]) => [`--dsw-static-deepseek-${k}`, same(v)])),

      // surfaces
      '--dsw-alias-bg-base': pair('#ffffff', '#181818'),
      '--dsw-alias-bg-layer-1': pair('#ffffff', '#232323'),
      '--dsw-alias-bg-layer-2': pair('#ffffff', '#2d2d2d'),
      '--dsw-alias-bg-layer-3': pair('#ffffff', '#363636'),
      '--dsw-alias-bg-overlay': pair('#ededed', '#414141'),
      '--dsw-alias-bg-module-platform': pair('#f3f3f3', '#2d2d2d'),
      '--dsw-alias-bg-multi-select': pair('#f3f3f3', '#232323'),
      '--dsw-alias-bg-document-selection': pair('#339cff4d', '#83c3ff4d'),

      // text
      '--dsw-alias-label-primary': pair('#1a1c1f', '#ffffff'),
      '--dsw-alias-label-primary-dimmed': pair('#1a1c1f', '#ededed'),
      '--dsw-alias-label-primary-bluish': pair('#1a1c1f', '#ffffff'),
      '--dsw-alias-label-secondary': pair('#1a1c1fb1', '#ffffffb5'),
      '--dsw-alias-label-tertiary': pair('#1a1c1f7e', '#ffffff7f'),
      '--dsw-alias-label-caption': pair('#1a1c1f59', '#ffffff59'),
      '--dsw-alias-label-dimmed': pair('#1a1c1f26', '#ffffff26'),
      '--dsw-alias-label-primary-foreground': pair('#ffffff', '#1a1c1f'),

      // hairlines
      '--dsw-alias-border-l1': pair('#1a1c1f0d', '#ffffff0b'),
      '--dsw-alias-border-l2': pair('#1a1c1f14', '#ffffff15'),
      '--dsw-alias-border-l3': pair('#1a1c1f1e', '#ffffff28'),
      '--dsw-alias-border-l4': pair('#1a1c1f29', '#ffffff33'),

      // ink actions, blue accent
      '--dsw-alias-brand-primary': pair('#1a1c1f', '#ffffff'),
      '--dsw-alias-brand-primary-invert': pair('#1a1c1f', '#ffffff'),
      '--dsw-alias-brand-text': pair('#1a1c1f', '#ffffff'),
      '--dsw-alias-link': pair('#339cff', '#83c3ff'),
      '--dsw-alias-state-business-primary': pair('#339cff', '#83c3ff'),
      '--dsw-alias-state-business-tertiary': pair('#e5f2ff', '#0d273f'),
      '--dsw-alias-button-primary-fill': pair('#0d0d0d', '#ffffff'),
      '--dsw-alias-button-primary-hover': pair('#393939', '#dfdfdf'),
      '--dsw-alias-button-info-fill': pair('#339cff', '#339cff'),
      '--dsw-alias-button-info-hover': pair('#1f8fff', '#5cb0ff'),

      // rows and hovers: translucent ink, never tinted
      '--dsw-alias-interactive-bg-hover': pair('#1a1c1f0e', '#ffffff14'),
      '--dsw-alias-interactive-bg-active': pair('#1a1c1f14', '#ffffff1f'),
      '--dsw-alias-interactive-bg-hover-solid': pair('#f3f3f3', '#2d2d2d'),

      // sidebar
      '--dsw-specific-sidebar-fill': pair('#fafafa', '#1a1a1a'),
      '--dsw-specific-sidebar-nav-item-hover': pair('#1a1c1f0e', '#ffffff14'),
      '--dsw-specific-sidebar-nav-item-active': pair('#1a1c1f14', '#ffffff1a'),
      '--dsw-specific-sidebar-nav-item-active-accent': pair('#1a1c1f14', '#ffffff1a'),

      // composer, menus, tooltips
      '--dsw-specific-input-major': pair('#ffffff', '#363636'),
      '--dsw-specific-selector': pair('#f3f3f3', '#414141'),
      '--dsw-specific-menu': pair('#fffffff2', '#2d2d2df2'),
      '--dsw-menu-surface-fill': pair('#fffffff2', '#2d2d2df2'),
      '--dsw-alias-menu-group-header-fill': pair('#fffffff2', '#2d2d2df2'),
      '--dsw-alias-tooltip-bg': pair('#1a1c1f', '#2d2d2d'),

      // user bubble (dark ink like the user's Codex window)
      '--dsw-specific-bubble': pair('#141414', '#323232'),
      '--dsw-specific-bubble-highlight': pair('#2d2d2d', '#444444'),
      '--dshcx-bubble-label': same('#ffffff'),

      // markdown and code
      '--dsw-alias-markdown-code-block': pair('#f3f3f3', '#2a2a2a'),
      '--dsw-alias-markdown-code-block-banner': pair('#f3f3f3', '#2a2a2a'),
      '--dsw-alias-markdown-inline-code': pair('#1a1c1f17', '#dfdfdf1c'),
      '--dsw-alias-markdown-tag': pair('#1a1c1f0e', '#ffffff14'),
      '--dsw-alias-turn-trigger-bg': pair('#f3f3f3', '#ffffff0f'),

      // diffs and states
      '--dsw-alias-code-diff-added': pair('#00a24026', '#40c9773b'),
      '--dsw-alias-code-diff-deleted': pair('#ba262326', '#fa423e3b'),
      '--dsw-alias-file-diff-added-bg': pair('#e0f3e7', '#1d3324'),
      '--dsw-alias-file-diff-added-marker': pair('#00a240', '#40c977'),
      '--dsw-alias-file-diff-deleted-bg': pair('#f7e2e1', '#3b1d1c'),
      '--dsw-alias-file-diff-deleted-marker': pair('#ba2623', '#fa423e'),
      '--dsw-alias-state-success-primary': pair('#00a240', '#40c977'),
      '--dsw-alias-state-warn-label': pair('#e25507', '#ff8549'),
      '--dsw-alias-state-error-primary': pair('#e02e2a', '#ff6764'),

      '--dsw-alias-scrollbar-bg-l1': pair('#1a1c1f33', '#ffffff33'),
      '--dsw-alias-scrollbar-hover-l1': pair('#1a1c1f4d', '#ffffff4d'),

      '--dshcx-accent': same(ACCENT),
      '--dshcx-warn': pair('#e25507', '#ff8549'),
      '--dshcx-tray': pair('#f4f4f4', '#232323'),
    };

    // ---- layout ----------------------------------------------------------
    const CSS = `
body { -webkit-font-smoothing: antialiased; }
::selection { background: var(--dsw-alias-bg-document-selection); }

/* Codex reads at 768px; the user's own drag width still wins. */
body [data-conversation-content] {
  --dsh-chat-content-width: var(--dsh-chat-user-width, min(768px, calc(100% - 32px)));
}

/* Sidebar: flat list, 10px rows, hairline divider, flat "New chat". */
body [class*="_sidebarCol"] { box-shadow: inset -0.5px 0 0 var(--dsw-alias-border-l2); }
body [class*="_sessionRow"], body [class*="_projectRow"] { border-radius: 10px; }
/* (!important: DSH tints this button per platform with higher specificity.) */
body button[class*="_newSession"] {
  background: transparent !important; border-color: transparent !important; box-shadow: none; font-weight: 450;
}
body button[class*="_newSession"]:is(:hover, :focus-visible) { background: var(--dsw-specific-sidebar-nav-item-hover) !important; }
body [class*="_newSessionContent"]:not([class*="_collapsed"] [class*="_newSessionContent"]) { justify-content: flex-start; }
.dshcx-brand { font: 600 17px/24px -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; letter-spacing: -0.01em; color: var(--dsw-alias-label-primary); }

/* User message: ink bubble, 22px squircle, max 70% wide. */
body [data-chat-flow-kind="user"] [class*="_bubble"] {
  color: var(--dshcx-bubble-label); border-radius: 22px; corner-shape: superellipse(1.5);
  padding: 10px 16px; max-width: 70%;
}
body [data-chat-flow-kind="user"] [class*="_bubble"] a { color: inherit; text-decoration: underline; }
body [data-chat-flow-kind="user"] [class*="_bubble"] code {
  background: color-mix(in srgb, var(--dshcx-bubble-label) 14%, transparent); color: inherit;
}

/* Composer: no border, Codex elevation, 22px radius. */
body [data-composer-card] {
  position: relative; z-index: 1; border: 0; border-radius: 22px;
  box-shadow: 0 0 0 1px #0000000a, 0 2px 8px 0 #0000000a, 0 4px 80px 8px #00000006;
}
body[data-ds-dark-theme] [data-composer-card] { box-shadow: inset 0 0 1px 0 #ffffff33, 0 8px 24px -10px #00000080; }
body [data-composer-placeholder] { opacity: .5; }
/* Send: solid ink circle (white in dark). */
body [data-composer-card] button[class*="_primary"] {
  background: var(--dsw-alias-button-primary-fill); color: var(--dsw-alias-label-primary-foreground);
  width: 28px; height: 28px; transform: none;
}
body [data-composer-card] button[class*="_primary"]:hover { background: var(--dsw-alias-button-primary-hover); }
body [data-composer-card] button[class*="_add"] { background: transparent; color: var(--dsw-alias-label-primary); }
body [data-composer-card] button[class*="_add"] svg { width: 16px; height: 16px; }
body [data-composer-card] button[class*="_add"]:hover { background: var(--dsw-alias-interactive-bg-hover); }
/* Footer chips: model name in ink, effort and chevrons quiet; full access warns in orange without a chevron. */
body [data-slot="conversation.input.model"] [class*="_triggerLabel"] { color: var(--dsw-alias-label-primary); }
body [data-slot="conversation.input.model"] [class*="_triggerEffort"] { color: var(--dsw-alias-label-tertiary); }
body [data-composer-card] [class*="_chevron"] { color: var(--dsw-alias-label-tertiary); }
body [data-composer-card] button[aria-label*="Full access" i] { color: var(--dshcx-warn); }
body [data-composer-card] button[aria-label*="Full access" i] [class*="_chevron"] { display: none; }
/* Session stats under the composer stay out of the way until hovered. */
body [data-composer-seat] [class*="_dock"]:not([data-slot]) { opacity: .7; transition: opacity .2s; }
body [data-composer-seat]:hover [class*="_dock"]:not([data-slot]) { opacity: 1; }

/* New session: headline in the middle, composer at the bottom (Codex home). */
body [data-phase="hero"] [data-conversation-scroll] { justify-content: flex-start; }
body [data-phase="hero"] [data-composer-seat] { flex: 1 1 auto; min-height: 100%; }
body [class*="_composerHero"] { flex: 1 1 auto; gap: 0; padding-bottom: 16px; }
body [class*="_composerHero"] > [class*="_root"]:first-child { margin: auto 0; padding-bottom: 32px; }
body [class*="_composerHero"] [class*="_headline"] { justify-content: center; }
body [class*="_composerHero"] [class*="_titleGroup"] { display: none; }
body [class*="_composerHero"] [class*="_fishHitbox"] { width: auto; height: auto; }
.dshcx-hero { display: flex; flex-direction: column; align-items: center; gap: 28px; text-align: center; }
.dshcx-hero-mark { display: inline-flex; filter: grayscale(1); opacity: .38; }
.dshcx-hero-title {
  font: 400 24px/32px -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; letter-spacing: -0.012em;
  color: var(--dsw-alias-label-primary); text-wrap: balance;
}
.dshcx-hero-ws {
  font: inherit; color: inherit; background: none; border: 0; padding: 0; cursor: pointer;
  text-decoration: underline dotted var(--dsw-alias-label-caption); text-underline-offset: 5px; text-decoration-thickness: 1.5px;
}
.dshcx-hero-ws:hover { text-decoration-color: var(--dsw-alias-label-tertiary); }
/* Workspace and mode become the grey tray tucked behind the composer's top edge. */
body [class*="_composerHero"] [class*="_heroWorkspaceRow"] {
  position: relative; margin: 0 calc(var(--dsh-composer-side-clearance, 16px) + 14px) -22px;
  padding: 6px 8px 28px; gap: 12px;
  background: var(--dshcx-tray); border-radius: 22px 22px 0 0;
}
body [class*="_heroWorkspaceRow"] button { color: var(--dsw-alias-label-primary); background: transparent; font-weight: 430; }
body [class*="_heroWorkspaceRow"] button:hover { background: var(--dsw-alias-interactive-bg-hover); }
body [class*="_heroWorkspaceRow"] [class*="_chevron"] { display: none; }

/* Inline code like Codex: small mono pill. */
body [data-conversation-content] :not(pre) > code { border-radius: 6px; padding: 1px 6px; font-size: .92em; }

/* ---- computer tool row ---- */
.dshcx-card { display: flex; flex-direction: column; }
.dshcx-row {
  height: calc(24px + var(--dsh-content-font-delta, 0px)); min-width: 0; display: flex; align-items: center;
  color: var(--dsw-alias-label-tertiary); transition: color .15s; cursor: pointer; overflow: hidden;
}
.dshcx-row:hover { color: var(--dsw-alias-label-primary); }
.dshcx-leading { width: 16px; height: 16px; margin-right: 6px; display: inline-flex; align-items: center; justify-content: center; flex: none; }
.dshcx-title, .dshcx-summary { font-size: var(--dsh-content-font-size-secondary, 13px); line-height: 24px; }
.dshcx-title { flex: none; }
.dshcx-summary { flex: auto; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.dshcx-sep { width: 2px; height: 2px; margin: 0 8px; border-radius: 1px; background: var(--dsw-alias-label-caption); flex: none; }
.dshcx-error { color: var(--dsw-alias-state-error-primary); }
.dshcx-shot {
  position: relative; align-self: flex-start; margin: 4px 0 6px 22px; width: min(360px, 100%);
  border-radius: 12px; corner-shape: superellipse(1.5); overflow: hidden;
  box-shadow: 0 0 0 .5px var(--dsw-alias-border-l3), 0 4px 16px #0000000d;
  background: var(--dsw-alias-bg-module-platform); cursor: zoom-in;
  transition: width .3s cubic-bezier(.19,1,.22,1);
}
.dshcx-shot[data-open] { width: min(100%, 900px); cursor: zoom-out; }
.dshcx-shot img { display: block; width: 100%; height: auto; }
.dshcx-shot[data-pending] img { filter: saturate(.7) brightness(.98); }
.dshcx-cursor { position: absolute; width: 0; height: 0; pointer-events: none; }
.dshcx-glyph { position: absolute; left: -2px; top: -2px; width: 22px; height: 22px; transform-origin: 2px 2px; will-change: transform; }
.dshcx-glyph svg {
  display: block; width: 22px; height: 22px; overflow: visible;
  filter: drop-shadow(0 1px 1.5px #00000040)
          drop-shadow(0 0 6px color-mix(in srgb, var(--dshcx-accent) 90%, transparent))
          drop-shadow(0 0 15px color-mix(in srgb, var(--dshcx-accent) 48%, transparent));
}
.dshcx-ripple {
  position: absolute; left: -13px; top: -13px; width: 26px; height: 26px; border-radius: 50%;
  border: 1.5px solid var(--dshcx-accent); background: color-mix(in srgb, var(--dshcx-accent) 16%, transparent);
  animation: dshcx-ripple .5s cubic-bezier(.19,1,.22,1) both;
}
@keyframes dshcx-ripple { from { transform: scale(.2); opacity: .95; } to { transform: scale(1.15); opacity: 0; } }
@media (prefers-reduced-motion: reduce) { .dshcx-ripple { animation: none; opacity: 0; } }
`;

    // ---- computer tool row -----------------------------------------------
    const de = (navigator.language || '').toLowerCase().startsWith('de');
    const T = de
      ? { title: 'Computer', screenshot: 'Bildschirm ansehen', click: 'Klick', double: 'Doppelklick', right: 'Rechtsklick',
          move: 'Zeiger bewegen', drag: 'Ziehen', scrollDown: 'Nach unten scrollen', scrollUp: 'Nach oben scrollen',
          type: 'Tippt', key: 'Taste', open: 'Öffnet', wait: 'Wartet', cursor: 'Zeigerposition', failed: 'Fehlgeschlagen' }
      : { title: 'Computer', screenshot: 'Look at screen', click: 'Click', double: 'Double-click', right: 'Right-click',
          move: 'Move pointer', drag: 'Drag', scrollDown: 'Scroll down', scrollUp: 'Scroll up',
          type: 'Type', key: 'Press', open: 'Open', wait: 'Wait', cursor: 'Pointer position', failed: 'Failed' };

    const clip = (v, n) => (v.length > n ? v.slice(0, n) + '…' : v);
    const quote = (v) => (de ? `„${v}“` : `“${v}”`);

    // Background tools: app windows and the headless browser.
    const B = de
      ? { open: 'Öffnet', state: 'Seite ansehen', windowState: 'Fenster ansehen', read: 'Seite lesen', back: 'Zurück',
          forward: 'Vor', reload: 'Neu laden', tabs: 'Tabs ansehen', newTab: 'Neuer Tab', tab: 'Tab', closeTab: 'Tab schließen',
          select: 'Auswahl', setValue: 'Trägt ein', menu: 'Menü', apps: 'Apps auflisten', showApp: 'Nach vorn holen',
          showBrowser: 'Browser zeigen', hideBrowser: 'Zurück in den Hintergrund', scroll: 'Scrollen' }
      : { open: 'Open', state: 'Look at page', windowState: 'Look at window', read: 'Read page', back: 'Back',
          forward: 'Forward', reload: 'Reload', tabs: 'Tabs', newTab: 'New tab', tab: 'Tab', closeTab: 'Close tab',
          select: 'Select', setValue: 'Fill in', menu: 'Menu', apps: 'List apps', showApp: 'Bring to front',
          showBrowser: 'Show browser', hideBrowser: 'Back to background', scroll: 'Scroll' };

    function bgLabel(tool, args, target) {
      const at = target ? ` ${quote(clip(target, 48))}` : typeof args.index === 'number' ? ` [${args.index}]`
        : typeof args.x === 'number' ? ` (${Math.round(args.x)}, ${Math.round(args.y)})` : '';
      const text = typeof args.text === 'string' ? args.text : '';
      const url = String(args.url ?? '').replace(/^https?:\/\//, '').replace(/\/$/, '');
      switch (args.action) {
        case 'open': return `${B.open} ${clip(url, 60)}`;
        case 'state': return tool === 'app' ? B.windowState : B.state;
        case 'click': return ((args.count ?? 1) > 1 ? T.double : T.click) + at;
        case 'type': return `${T.type} ${quote(clip(text, 48))}${at}`;
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
        case 'type': return `${T.type} „${text.length > 48 ? text.slice(0, 48) + '…' : text}“`;
        case 'key': return `${T.key} ${text}`;
        case 'open_app': return `${T.open} ${text}`;
        case 'wait': return `${T.wait} ${args.seconds ?? 1}s`;
        case 'cursor_position': return T.cursor;
        default: return args.action ?? '';
      }
    }

    function parseArgs(raw) {
      if (typeof raw !== 'string') return {};
      try { return JSON.parse(raw) ?? {}; } catch { return {}; }
    }

    // The latest finished screenshot per tool, so a running step can show the
    // cursor travelling across the previous frame while the real one moves.
    const lastShots = {};

    // Rounded arrowhead, tip at (2,2) pointing up-left, concave notch at the
    // back; black with a white rim and an accent glow (drawn for this plugin).
    const ARROW = 'M2 2 L7.3 18.4 Q7.9 19.9 8.9 18.8 L11.1 11.1 L18.8 8.9 Q19.9 7.9 18.4 7.3 Z';
    const CursorGlyph = () => h('svg', { viewBox: '0 0 22 22', 'aria-hidden': true },
      h('path', { d: ARROW, fill: '#0d0d0d', stroke: '#fff', strokeWidth: 2, strokeLinejoin: 'round', paintOrder: 'stroke' }));

    const PointerIcon = () => h('svg', { width: 14, height: 14, viewBox: '0 0 22 22', 'aria-hidden': true },
      h('path', { d: ARROW, fill: 'none', stroke: 'currentColor', strokeWidth: 1.8, strokeLinejoin: 'round' }));
    const stroke = { fill: 'none', stroke: 'currentColor', strokeWidth: 1.3, strokeLinecap: 'round', strokeLinejoin: 'round' };
    const GlobeIcon = () => h('svg', { width: 14, height: 14, viewBox: '0 0 16 16', 'aria-hidden': true },
      h('circle', { cx: 8, cy: 8, r: 6.25, ...stroke }),
      h('path', { d: 'M1.9 8h12.2M8 1.75c1.7 1.8 2.5 3.9 2.5 6.25S9.7 12.45 8 14.25C6.3 12.45 5.5 10.35 5.5 8S6.3 3.55 8 1.75Z', ...stroke }));
    const WindowIcon = () => h('svg', { width: 14, height: 14, viewBox: '0 0 16 16', 'aria-hidden': true },
      h('rect', { x: 1.75, y: 2.75, width: 12.5, height: 10.5, rx: 2.25, ...stroke }),
      h('path', { d: 'M1.9 5.9h12.2', ...stroke }),
      h('circle', { cx: 4.1, cy: 4.35, r: 0.55, fill: 'currentColor' }),
      h('circle', { cx: 5.9, cy: 4.35, r: 0.55, fill: 'currentColor' }));
    const ICONS = { computer: PointerIcon, app: WindowIcon, browser: GlobeIcon };

    // Codex-like motion in screenshot pixel space: a bowed path for long
    // moves with the tip turning into the direction of travel, a tilted
    // scoot for short ones, speed squash, then a short "thinking" wiggle.
    const REST = -135;  // the glyph's tip points up-left
    const easeMove = (t) => (t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2);
    const wrap = (a) => ((a + 540) % 360) - 180;
    const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));

    function glide(span, screen, from, to, onArrive) {
      const glyph = span.firstChild;
      const put = (p, rot, along, squash) => {
        span.style.left = `${(p.x / screen.width) * 100}%`;
        span.style.top = `${(p.y / screen.height) * 100}%`;
        glyph.style.transform =
          `rotate(${along}deg) scale(${squash}, 1) rotate(${-along}deg) rotate(${rot}deg)`;
      };
      let raf = 0;
      const t0 = performance.now();

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
      return h('div', {
        className: 'dshcx-shot', 'data-open': open || undefined, 'data-pending': pending || undefined,
        onClick: () => setOpen((v) => !v),
      },
      h('img', { src: url, alt: '', draggable: false }),
      to ? h('span', { className: 'dshcx-cursor', ref: span },
        h('span', { className: 'dshcx-glyph' }, h(CursorGlyph)),
        pressed ? h('span', { className: 'dshcx-ripple', key: pressed }) : null) : null);
    }

    const ToolRow = (tool) => function DshCodexToolRow(props) {
      const { phase, block, loadImage } = props;
      const result = phase === 'result' ? block : null;
      const args = parseArgs(result ? result.call?.argsRaw : block?.argsRaw);
      const meta = result?.meta ?? null;
      const image = result?.content?.find?.((c) => c.type === 'image')?.attachment;
      const [url, setUrl] = useState(null);
      const startShot = useRef(lastShots[tool] ?? null);

      useEffect(() => {
        if (!image || !loadImage) return undefined;
        let live = true;
        Promise.resolve(loadImage(image)).then((u) => { if (live) setUrl(u); }).catch(() => {});
        return () => { live = false; };
      }, [image?.attachmentId]);

      const target = typeof args.x === 'number' ? { x: args.x, y: args.y } : null;
      const end = args.action === 'drag' && typeof args.to_x === 'number' ? { x: args.to_x, y: args.to_y } : target;
      const screen = meta?.screen ?? startShot.current?.screen ?? null;
      // Background tools aim at elements; the host reports where it clicked.
      const aimed = tool === 'computer' ? (meta?.cursor ?? end) : (meta?.cursor ?? target);

      useEffect(() => {
        if (url && meta?.screen) lastShots[tool] = { url, screen: meta.screen, cursor: aimed, place: meta.place };
      }, [url]);

      const running = phase !== 'result';
      const failed = !!result?.isError;
      const errorText = failed
        ? (result.error?.reason || result.content?.find?.((c) => c.type === 'text')?.text || T.failed)
        : null;
      const place = meta?.place || (tool === 'app' ? args.app || startShot.current?.place : '') || '';
      const title = tool === 'computer' ? T.title : tool === 'browser' ? 'Browser' : (place || 'App');
      const action = tool === 'computer' ? label(args) : bgLabel(tool, args, meta?.target);
      const summary = phase === 'preparing' ? ''
        : tool === 'browser' && place && !['open', 'new_tab'].includes(args.action) ? `${action} · ${place}` : action;
      const clicks = /click|select|type|action/.test(args.action ?? '');
      // Only animate steps that just happened; replayed history sits still.
      const fresh = !result || typeof result.time !== 'number' || result.time < 1e12 || Date.now() - result.time < 20_000;

      let shot = null;
      if (url && screen) {
        // Finished step: glide from where the previous step left the cursor
        // (only within the same app or site; another place starts fresh).
        const prev = startShot.current;
        const from = prev && (tool === 'computer' || prev.place === meta?.place) ? prev.cursor : null;
        shot = h(Shot, { key: 'done', url, screen, from, to: aimed, click: clicks && !!aimed, still: !fresh });
      } else if (running && target && startShot.current) {
        // Running step: replay the motion on the last frame we have.
        const prev = startShot.current;
        shot = h(Shot, { key: 'live', url: prev.url, screen: prev.screen, from: prev.cursor, to: end, click: clicks, pending: true });
      }

      return h('div', { className: 'dshcx-card', 'data-tool': tool, 'data-state': running ? 'running' : failed ? 'error' : 'done' },
        h('div', { className: 'dshcx-row' },
          h('span', { className: 'dshcx-leading' }, h(ICONS[tool])),
          h(TextShimmer, { active: running }, h('span', { className: 'dshcx-title' }, title)),
          summary ? h('span', { className: 'dshcx-sep' }) : null,
          h('span', { className: 'dshcx-summary' + (failed ? ' dshcx-error' : ''), title: errorText ?? summary },
            failed ? errorText : summary)),
        shot);
    };

    // ---- brand -----------------------------------------------------------
    // Codex shows its name as plain semibold text, no logo or badge.
    const NoMark = () => null;
    const BrandName = () => h('span', { className: 'dshcx-brand' }, 'DeepSeek');

    // New-session headline like Codex: a quiet mark above one question that
    // names the workspace. The name opens DSH's own workspace picker.
    const { FishLogo } = require('@deepseek-ai/dsh-client-ui-primitives');
    const HERO = de
      ? ['Woran sollen wir in ', ' arbeiten?', 'Woran sollen wir arbeiten?']
      : ['What should we work on in ', '?', 'What should we work on?'];

    function HeroHeadline() {
      const ref = useRef(null);
      const [ws, setWs] = useState('');
      useEffect(() => {
        const hero = ref.current?.closest('[class*="_composerHero"]');
        if (!hero) return undefined;
        const read = () => setWs(hero.querySelector('[class*="_workspaceLabel"]')?.textContent?.trim() ?? '');
        read();
        const observer = new MutationObserver(read);
        observer.observe(hero, { subtree: true, childList: true, characterData: true });
        return () => observer.disconnect();
      }, []);
      const pick = () => ref.current?.closest('[class*="_composerHero"]')
        ?.querySelector('[class*="_heroWorkspaceRow"] button')?.click();
      return h('span', { className: 'dshcx-hero', ref },
        FishLogo ? h('span', { className: 'dshcx-hero-mark' }, h(FishLogo, { size: 36 })) : null,
        h('span', { className: 'dshcx-hero-title' },
          ws ? [HERO[0], h('button', { key: 'ws', type: 'button', className: 'dshcx-hero-ws', onClick: pick }, ws), HERO[1]] : HERO[2]));
    }

    // ---- wiring ----------------------------------------------------------
    function apply(ctx) {
      ctx.effect(() => ctx.theme.overrideTokens(SOURCE, TOKENS), 'dsh-codex: tokens');

      ctx.effect(() => {
        const style = document.createElement('style');
        style.dataset.plugin = SOURCE;
        style.textContent = CSS;
        document.head.appendChild(style);
        return () => style.remove();
      }, 'dsh-codex: layout');

      // Lower priority renders first, so these shadow the official brand
      // without disabling it.
      const BRAND_PRIORITY = -10;
      ctx.slots.inject('sidebar.brand.mark', () =>
        ctx.slots.register({ name: 'sidebar.brand.mark', priority: BRAND_PRIORITY }, NoMark));
      ctx.slots.inject('sidebar.brand.name', () =>
        ctx.slots.register({ name: 'sidebar.brand.name', priority: BRAND_PRIORITY }, BrandName));
      ctx.slots.inject('conversation.hero.brand.mark', () =>
        ctx.slots.register({ name: 'conversation.hero.brand.mark', priority: BRAND_PRIORITY }, HeroHeadline));

      ctx.slots.inject('tool.call.toolview', function* () {
        for (const tool of ['computer', 'app', 'browser']) {
          yield ctx.slots.register({ name: 'tool.call.toolview', key: tool }, ToolRow(tool));
        }
      });
    }

    // Dev preview of the tool rows outside a live session:
    // localStorage['dsh-codex:preview'] = '1', then reload.
    try {
      if (localStorage.getItem('dsh-codex:preview') === '1') {
        window.__dshCodexPreview = { React, ToolRow, createRoot: require('react-dom/client').createRoot };
      }
    } catch { /* storage blocked */ }

    return { name: SOURCE, inject: ['slots', 'theme'], apply };
  },
});
