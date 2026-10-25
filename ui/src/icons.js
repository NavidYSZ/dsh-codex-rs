// Line icons, 16 px grid, drawn for this UI (no icon font, no network).
import { html } from './html.js';

const S = { fill: 'none', stroke: 'currentColor', 'stroke-width': 1.3, 'stroke-linecap': 'round', 'stroke-linejoin': 'round' };
const icon = (body, size = 16) => (p = {}) => html`<svg class=${p.class ?? 'icon'} width=${p.size ?? size} height=${p.size ?? size} viewBox="0 0 16 16" aria-hidden="true">${body}</svg>`;

export const IconTerminal = icon(html`<rect x="1.75" y="2.75" width="12.5" height="10.5" rx="2.25" ...${S}/><path d="M4.75 6.25 6.75 8l-2 1.75M8.5 10h2.75" ...${S}/>`);
export const IconFile = icon(html`<path d="M4 1.75h5l3.25 3.25v8a1.25 1.25 0 0 1-1.25 1.25H4A1.25 1.25 0 0 1 2.75 13V3A1.25 1.25 0 0 1 4 1.75Z" ...${S}/><path d="M8.75 1.9V5.25h3.35" ...${S}/>`);
export const IconPencil = icon(html`<path d="M10.6 2.6a1.6 1.6 0 0 1 2.3 2.3l-7.6 7.6-3.05.75.75-3.05 7.6-7.6Z" ...${S}/><path d="M9.6 3.6l2.8 2.8" ...${S}/>`);
export const IconSearch = icon(html`<circle cx="7" cy="7" r="4.5" ...${S}/><path d="m10.4 10.4 3.35 3.35" ...${S}/>`);
export const IconList = icon(html`<path d="M6.25 4h7M6.25 8h7M6.25 12h7" ...${S}/><path d="m2.25 4 .9.9 1.6-1.8M2.25 8l.9.9 1.6-1.8" ...${S}/><circle cx="3.4" cy="12" r=".9" ...${S}/>`);
export const IconGlobe = icon(html`<circle cx="8" cy="8" r="6.25" ...${S}/><path d="M1.9 8h12.2M8 1.75c1.7 1.8 2.5 3.9 2.5 6.25S9.7 12.45 8 14.25C6.3 12.45 5.5 10.35 5.5 8S6.3 3.55 8 1.75Z" ...${S}/>`);
export const IconWindow = icon(html`<rect x="1.75" y="2.75" width="12.5" height="10.5" rx="2.25" ...${S}/><path d="M1.9 5.9h12.2" ...${S}/><circle cx="4.1" cy="4.35" r=".55" fill="currentColor"/><circle cx="5.9" cy="4.35" r=".55" fill="currentColor"/>`);
export const IconImage = icon(html`<rect x="1.75" y="2.75" width="12.5" height="10.5" rx="2.25" ...${S}/><circle cx="5.5" cy="6.25" r="1.1" ...${S}/><path d="m2.25 11.5 3.4-3.1 2.6 2.3 2.2-1.9 3.3 2.9" ...${S}/>`);
export const IconTool = icon(html`<path d="M9.9 2.2a3.3 3.3 0 0 0-3.6 4.6L2.4 10.7a1.4 1.4 0 0 0 2 2l3.9-3.9a3.3 3.3 0 0 0 4.6-3.6l-2 2-1.9-.5-.5-1.9 2-2Z" ...${S}/>`);
export const IconJob = icon(html`<circle cx="8" cy="8" r="6.25" ...${S}/><path d="M8 4.5V8l2.25 1.5" ...${S}/>`);

// Cursor arrow from client.js: tip at (2,2) pointing up-left, concave notch at the back.
export const ARROW = 'M2 2 L7.3 18.4 Q7.9 19.9 8.9 18.8 L11.1 11.1 L18.8 8.9 Q19.9 7.9 18.4 7.3 Z';
export const IconPointer = (p = {}) => html`<svg class=${p.class ?? 'icon'} width=${p.size ?? 16} height=${p.size ?? 16} viewBox="0 0 22 22" aria-hidden="true"><path d=${ARROW} fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round"/></svg>`;

export const IconPlus = icon(html`<path d="M8 3v10M3 8h10" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>`);
export const IconArrowUp = icon(html`<path d="M8 13V3.5M3.75 7.5 8 3.25l4.25 4.25" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"/>`);
export const IconStop = icon(html`<rect x="4.5" y="4.5" width="7" height="7" rx="1.5" fill="currentColor"/>`);
export const IconChevronDown = icon(html`<path d="m4.5 6.25 3.5 3.5 3.5-3.5" ...${S}/>`);
export const IconChevronRight = icon(html`<path d="m6.25 4.5 3.5 3.5-3.5 3.5" ...${S}/>`);
export const IconChevronLeft = icon(html`<path d="M9.75 4.5 6.25 8l3.5 3.5" ...${S}/>`);
export const IconGear = icon(html`<circle cx="8" cy="8" r="2.1" ...${S}/><path d="M8 1.75v1.6M8 12.65v1.6M14.25 8h-1.6M3.35 8h-1.6M12.42 3.58l-1.13 1.13M4.71 11.29l-1.13 1.13M12.42 12.42l-1.13-1.13M4.71 4.71 3.58 3.58" ...${S}/><circle cx="8" cy="8" r="4.6" ...${S}/>`);
export const IconCompose = icon(html`<path d="M7.25 2.75H4A1.25 1.25 0 0 0 2.75 4v8A1.25 1.25 0 0 0 4 13.25h8A1.25 1.25 0 0 0 13.25 12V8.75" ...${S}/><path d="M11.6 2.4a1.4 1.4 0 0 1 2 2L8.25 9.75l-2.4.6.6-2.4L11.6 2.4Z" ...${S}/>`);
export const IconFolder = icon(html`<path d="M1.75 4.5A1.25 1.25 0 0 1 3 3.25h3l1.5 1.5H13a1.25 1.25 0 0 1 1.25 1.25v5.5A1.25 1.25 0 0 1 13 12.75H3a1.25 1.25 0 0 1-1.25-1.25v-7Z" ...${S}/>`);
export const IconSidebar = icon(html`<rect x="1.75" y="2.75" width="12.5" height="10.5" rx="2.25" ...${S}/><path d="M6.25 2.9v10.2" ...${S}/>`);
export const IconTrash = icon(html`<path d="M2.75 4.25h10.5M6.25 4V2.75h3.5V4M4.25 4.25l.6 8.6A1.25 1.25 0 0 0 6.1 14h3.8a1.25 1.25 0 0 0 1.25-1.15l.6-8.6" ...${S}/>`);
export const IconCheck = icon(html`<path d="m3.5 8.25 2.9 2.9 6.1-6.3" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>`);
export const IconX = icon(html`<path d="m4.25 4.25 7.5 7.5M11.75 4.25l-7.5 7.5" ...${S}/>`);
export const IconAlert = icon(html`<circle cx="8" cy="8" r="6.25" ...${S}/><path d="M8 4.75v3.75" ...${S}/><circle cx="8" cy="11" r=".75" fill="currentColor"/>`);
export const IconShield = icon(html`<path d="M8 1.75 13 3.6v4.1c0 3-2.1 5.3-5 6.55-2.9-1.25-5-3.55-5-6.55V3.6L8 1.75Z" ...${S}/>`);
export const IconLock = icon(html`<rect x="3" y="7" width="10" height="7" rx="1.75" ...${S}/><path d="M5.25 7V5a2.75 2.75 0 0 1 5.5 0v2" ...${S}/>`);
export const IconArrowDown = icon(html`<path d="M8 3v9.5M3.75 8.5 8 12.75l4.25-4.25" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>`);
export const IconBell = icon(html`<path d="M4.1 6.6a3.9 3.9 0 0 1 7.8 0c0 2.5.75 3.85 1.45 4.65H2.65c.7-.8 1.45-2.15 1.45-4.65Z" ...${S}/><path d="M6.5 13.4a1.6 1.6 0 0 0 3 0" ...${S}/>`);
export const IconPin = icon(html`<path d="M9.9 1.9 14.1 6.1l-1.45.55-2.5 2.5-.3 2.9-1.05 1.05L2.9 7.2l1.05-1.05 2.9-.3 2.5-2.5L9.9 1.9Z" ...${S}/><path d="m5.6 10.4-3.35 3.35" ...${S}/>`);
export const IconArchive = icon(html`<rect x="1.75" y="2.75" width="12.5" height="3" rx="1" ...${S}/><path d="M2.9 5.9v6.35a1 1 0 0 0 1 1h8.2a1 1 0 0 0 1-1V5.9M6.4 8.5h3.2" ...${S}/>`);
export const IconMore = icon(html`<circle cx="3.5" cy="8" r="1.1" fill="currentColor"/><circle cx="8" cy="8" r="1.1" fill="currentColor"/><circle cx="12.5" cy="8" r="1.1" fill="currentColor"/>`);
export const IconChat = icon(html`<path d="M2.25 7.6c0-2.85 2.55-4.85 5.75-4.85s5.75 2 5.75 4.85S11.2 12.45 8 12.45c-.75 0-1.45-.1-2.1-.3L3 13.25l.8-2.35A4.45 4.45 0 0 1 2.25 7.6Z" ...${S}/>`);
export const IconFolderPlus = icon(html`<path d="M1.75 4.5A1.25 1.25 0 0 1 3 3.25h3l1.5 1.5H13a1.25 1.25 0 0 1 1.25 1.25v5.5A1.25 1.25 0 0 1 13 12.75H3a1.25 1.25 0 0 1-1.25-1.25v-7Z" ...${S}/><path d="M8 7v3.5M6.25 8.75h3.5" ...${S}/>`);
export const IconPanelRight = icon(html`<rect x="1.75" y="2.75" width="12.5" height="10.5" rx="2.25" ...${S}/><path d="M9.75 2.9v10.2" ...${S}/>`);
export const IconUnread = icon(html`<path d="M13.25 7.2v4.55a1.5 1.5 0 0 1-1.5 1.5h-7.5a1.5 1.5 0 0 1-1.5-1.5v-7.5a1.5 1.5 0 0 1 1.5-1.5H8.8" ...${S}/><circle cx="12.25" cy="3.75" r="1.75" fill="currentColor"/>`);
export const IconCopy = icon(html`<rect x="5.25" y="5.25" width="8.5" height="8.5" rx="1.75" ...${S}/><path d="M10.75 5.25V3.5A1.25 1.25 0 0 0 9.5 2.25h-6A1.25 1.25 0 0 0 2.25 3.5v6a1.25 1.25 0 0 0 1.25 1.25h1.75" ...${S}/>`);
export const IconSun = icon(html`<circle cx="8" cy="8" r="2.75" ...${S}/><path d="M8 1.75v1.5M8 12.75v1.5M14.25 8h-1.5M3.25 8h-1.5M12.4 3.6l-1.05 1.05M4.65 11.35 3.6 12.4M12.4 12.4l-1.05-1.05M4.65 4.65 3.6 3.6" ...${S}/>`);
export const IconRefresh = icon(html`<path d="M13.25 8a5.25 5.25 0 1 1-1.54-3.71" ...${S}/><path d="M13.4 2.6v3h-3" ...${S}/>`);
export const IconExternal = icon(html`<path d="M9.25 2.75h4v4M13.1 2.9 7.75 8.25" ...${S}/><path d="M11.75 9.5v2.75a1 1 0 0 1-1 1h-7a1 1 0 0 1-1-1v-7a1 1 0 0 1 1-1H6.5" ...${S}/>`);
export const IconDiff = icon(html`<path d="M4.5 2.25v6.5M1.25 5.5h6.5M8.25 12.5h6.5" ...${S}/><path d="M11.5 2.5 9.75 13.5" ...${S}/>`);
export const IconPlug = icon(html`<path d="M5.75 1.75v3M10.25 1.75v3M3.75 4.75h8.5v2.5a4.25 4.25 0 0 1-8.5 0v-2.5ZM8 11.5v2.75" ...${S}/>`);
export const IconKey = icon(html`<circle cx="5.5" cy="10.5" r="2.75" ...${S}/><path d="m7.5 8.5 5.75-5.75M11 5l1.75 1.75M9.5 6.5l1.25 1.25" ...${S}/>`);

/**
 * Neutral product glyph (a prompt in a rounded square). The DeepSeek whale is
 * DeepSeek's brand material and is not used outside DSH.
 */
export const Mark = ({ size = 36 }) => html`<svg class="mark" width=${size} height=${size} viewBox="0 0 36 36" aria-hidden="true">
  <rect x="2" y="2" width="32" height="32" rx="10" fill="currentColor"/>
  <path d="M11 13.5 16 18l-5 4.5M19.5 23h6" fill="none" stroke="var(--bg)" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round"/>
</svg>`;
export const IconInfo = icon(html`<circle cx="8" cy="8" r="6.25" ...${S}/><path d="M8 7.25v3.75" ...${S}/><circle cx="8" cy="5" r=".8" fill="currentColor"/>`);
export const IconChart = icon(html`<path d="M2.25 13.75h11.5" ...${S}/><rect x="3.25" y="8" width="2.5" height="4" rx=".6" ...${S}/><rect x="6.75" y="3.75" width="2.5" height="8.25" rx=".6" ...${S}/><rect x="10.25" y="6" width="2.5" height="6" rx=".6" ...${S}/>`);
export const IconCube = icon(html`<path d="M8 1.9 13.6 5v6L8 14.1 2.4 11V5L8 1.9Z" ...${S}/><path d="M2.6 5.1 8 8.1l5.4-3M8 8.1v5.8" ...${S}/>`);
export const IconContext = icon(html`<path d="M2.75 4.25h10.5M2.75 8h10.5M2.75 11.75h6" ...${S}/><path d="m11 10.25 1.75 1.5L11 13.25" ...${S}/>`);
