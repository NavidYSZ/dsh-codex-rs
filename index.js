// Host half of @yousufzai/dsh-codex. The Codex look lives in client.js; here
// we register the macOS tools: "computer" (foreground, takes over the screen),
// "app" (one app in the background) and "browser" (background Chrome).
import * as computer from './lib/computer-tool.js';
import * as apps from './lib/app-tool.js';
import * as browserTool from './lib/browser-tool.js';
import { Browser, findChrome } from './lib/browser.js';
import { execFileSync } from 'node:child_process';

export const name = 'dsh-codex';
export const inject = ['tools'];

const mac = process.platform === 'darwin';

/** "de" or "en" from the environment, else macOS's first preferred language. */
function systemLocale() {
  const env = process.env.LC_ALL || process.env.LC_MESSAGES || process.env.LANG;
  let lang = env && !/^(C|POSIX)\b/.test(env) ? env : '';
  if (!lang && mac) {
    try {
      lang = execFileSync('defaults', ['read', '-g', 'AppleLanguages'], { encoding: 'utf8', timeout: 2000 })
        .match(/[A-Za-z]{2,3}/)?.[0] ?? '';
    } catch { /* no preference set */ }
  }
  return lang.toLowerCase().startsWith('de') ? 'de' : 'en';
}

const STRINGS = {
  de: { banner: 'DeepSeek verwendet deinen Computer', escHint: 'Esc zum Abbrechen',
    done: 'Fertig', stopped: 'Gestoppt', stop: 'Stoppen', open: 'Anzeigen' },
  en: { banner: 'DeepSeek is using your computer', escHint: 'Esc to cancel',
    done: 'Done', stopped: 'Stopped', stop: 'Stop', open: 'Show' },
};

const DEFAULTS = {
  computerUse: {
    enabled: mac,
    accent: '#339cff',
    label: 'DeepSeek',
    showLabel: false,
    showGlow: false,
    showActivity: true,
    clickPulse: true,
    maxWidth: 1280,
    quality: 0.8,
  },
  apps: { enabled: mac },
  browser: { enabled: true, chrome: undefined, width: 1280, height: 800, idleMinutes: 15, quality: 0.7 },
  mini: { doneAfter: 6, hideAfter: 24 },
};

const IMAGE_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['attachmentId', 'mediaType', 'bytes', 'width', 'height'],
  properties: {
    attachmentId: { type: 'string' },
    mediaType: { type: 'string', enum: ['image/png', 'image/jpeg', 'image/webp', 'image/gif'] },
    bytes: { type: 'integer' },
    width: { type: 'integer' },
    height: { type: 'integer' },
    name: { type: 'string' },
    originalDimensions: {
      type: 'object',
      additionalProperties: false,
      required: ['width', 'height'],
      properties: { width: { type: 'integer' }, height: { type: 'integer' } },
    },
  },
};

const POINT = {
  type: 'object',
  additionalProperties: false,
  required: ['x', 'y'],
  properties: { x: { type: 'integer' }, y: { type: 'integer' } },
};

const OUTPUT_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['summary'],
  properties: {
    summary: { type: 'string' },
    image: IMAGE_SCHEMA,
    screen: {
      type: 'object',
      additionalProperties: false,
      required: ['width', 'height'],
      properties: { width: { type: 'integer' }, height: { type: 'integer' } },
    },
    cursor: POINT,
    place: { type: 'string' },   // app name or site for the transcript row
    target: { type: 'string' },  // name of the element acted on
  },
};

async function imageCapable(ctx, exec) {
  const routed = exec.agent?.session.requestHeader()?.config;
  const provider = routed?.provider ?? exec.agent?.options.provider;
  const model = routed?.model ?? exec.agent?.options.model;
  const llm = ctx.get('llm');
  if (provider === undefined || model === undefined || llm === undefined) return false;
  try {
    const info = await llm.resolveModelInfo(provider, model, exec.signal);
    return Array.isArray(info.inputModalities) && info.inputModalities.includes('image');
  } catch {
    return false;
  }
}

/** Stores a screenshot as a session attachment when the model can see images. */
async function attach(ctx, exec, value, image, textOnlyHint) {
  value.screen = { width: image.width, height: image.height };
  const attachments = ctx.get('attachments');
  if (!attachments || !(await imageCapable(ctx, exec))) {
    if (textOnlyHint) value.summary += textOnlyHint;
    return;
  }
  const ref = await attachments.saveImage({
    data: new Uint8Array(Buffer.from(image.data, 'base64')),
    mediaType: image.mime,
    name: 'screen.jpg',
  });
  value.image = {
    attachmentId: String(ref.attachmentId),
    mediaType: ref.mediaType,
    bytes: ref.bytes,
    width: ref.width,
    height: ref.height,
    ...(ref.name === undefined ? {} : { name: ref.name }),
    ...(ref.originalDimensions === undefined ? {} : { originalDimensions: { ...ref.originalDimensions } }),
  };
  if (ref.width !== image.width) {
    value.summary += ` (The attached image is scaled to ${ref.width}×${ref.height}; keep using coordinates of the ${image.width}×${image.height} screenshot.)`;
  }
}

const round = (p) => (p && typeof p.x === 'number' ? { x: Math.round(p.x), y: Math.round(p.y) } : null);

/** One tool with the shared output shape and transcript metadata. */
function registerTool(ctx, { tool, description, parameters, describe, execute, textOnlyHint }) {
  ctx.tools.register({
    name: tool,
    description,
    parameters,
    timeoutMs: 120_000,
    output: {
      schema: OUTPUT_SCHEMA,
      render(_args, value) {
        const blocks = [{ type: 'text', text: value.summary }];
        if (value.image) blocks.push({ type: 'image', attachment: { ...value.image } });
        return blocks;
      },
      // Replayed to the client renderer as `meta` on the tool result.
      presentationMeta(args, value) {
        return {
          tool,
          action: args.action,
          label: describe(args),
          x: typeof args.x === 'number' ? args.x : null,
          y: typeof args.y === 'number' ? args.y : null,
          toX: typeof args.to_x === 'number' ? args.to_x : null,
          toY: typeof args.to_y === 'number' ? args.to_y : null,
          screen: value.screen ?? null,
          cursor: value.cursor ?? null,
          place: value.place ?? null,
          target: value.target ?? null,
        };
      },
    },
    presentCall(args) {
      return { card: 'generic', title: describe(args), kind: 'other' };
    },
    async execute(args, exec) {
      const result = await execute(args, exec);
      const value = { summary: result.text };
      const cursor = round(result.cursor);
      if (cursor) value.cursor = cursor;
      if (result.place) value.place = String(result.place).slice(0, 120);
      if (result.target) value.target = String(result.target).slice(0, 120);
      if (result.image) await attach(ctx, exec, value, result.image, textOnlyHint);
      return value;
    },
  });
}

function registerComputer(ctx, settings, mini) {
  let configured = false;
  registerTool(ctx, {
    tool: computer.TOOL_NAME,
    description: computer.DESCRIPTION,
    parameters: computer.INPUT_SCHEMA,
    describe: computer.describe,
    textOnlyHint: ' (The current model cannot see images; switch to an image-capable model for computer use.)',
    async execute(args, exec) {
      if (!configured) {
        await computer.configure({
          accent: settings.accent, label: settings.label, banner: settings.banner, escHint: settings.escHint,
          showLabel: settings.showLabel, showGlow: settings.showGlow, showActivity: settings.showActivity,
          clickPulse: settings.clickPulse, maxWidth: settings.maxWidth, mini,
          ...(Array.isArray(settings.blockedApps) ? { blockedApps: settings.blockedApps } : {}),
        });
        configured = true;
      }
      const result = await computer.run(args, {
        maxWidth: settings.maxWidth, quality: settings.quality, locale: settings.locale, signal: exec.signal,
      });
      return result;
    },
  });
}

/** The helper keeps its own settings; send them once per host lifetime. */
function helperSetup(settings, mini) {
  let done = null;
  return () => (done ??= computer.configure({
    accent: settings.accent, maxWidth: settings.maxWidth, mini,
    ...(Array.isArray(settings.blockedApps) ? { blockedApps: settings.blockedApps } : {}),
  }).catch(() => { done = null; }));
}

function registerApps(ctx, settings, setup) {
  registerTool(ctx, {
    tool: apps.TOOL_NAME,
    description: apps.DESCRIPTION,
    parameters: apps.INPUT_SCHEMA,
    describe: apps.describe,
    async execute(args) {
      await setup();
      const result = await apps.run(args, { locale: settings.locale, maxWidth: settings.maxWidth, quality: settings.quality });
      return { ...result, place: result.app };
    },
  });
}

function registerBrowser(ctx, settings, cu, setup) {
  const browser = new Browser({
    chrome: settings.chrome, width: settings.width, height: settings.height, idleMinutes: settings.idleMinutes,
    locale: cu.locale, mini: mac && settings.mini !== false,
  });
  if (!browser.available) return null;
  registerTool(ctx, {
    tool: browserTool.TOOL_NAME,
    description: browserTool.DESCRIPTION,
    parameters: browserTool.INPUT_SCHEMA,
    describe: browserTool.describe,
    async execute(args) {
      if (mac) await setup();
      const result = await browserTool.run(browser, args, { locale: cu.locale, quality: settings.quality });
      let place = '';
      try { place = result.url ? new URL(result.url).host : ''; } catch { /* about:blank */ }
      return { ...result, place };
    },
  });
  return browser;
}

export function apply(ctx, config = {}) {
  const locale = config.computerUse?.locale ?? systemLocale();
  const { banner, escHint, ...miniStrings } = STRINGS[locale] ?? STRINGS.en;
  const cu = { ...DEFAULTS.computerUse, locale, banner, escHint, ...(config.computerUse ?? {}) };
  const appSettings = { ...DEFAULTS.apps, ...(config.apps ?? {}) };
  const browserSettings = { ...DEFAULTS.browser, ...(config.browser ?? {}) };
  const mini = { ...DEFAULTS.mini, ...miniStrings, ...(config.mini ?? {}) };
  const setup = helperSetup(cu, mini);

  // Screenshots need the durable attachment store, exactly like read_image.
  ctx.inject(['attachments'], (imageCtx) => {
    if (cu.enabled) registerComputer(imageCtx, cu, mini);
    if (appSettings.enabled && mac) registerApps(imageCtx, cu, setup);
    if (browserSettings.enabled && findChrome(browserSettings.chrome)) {
      const browser = registerBrowser(imageCtx, browserSettings, cu, setup);
      if (browser) imageCtx.effect?.(() => () => { browser.close(); }, 'dsh-codex: browser');
    }
  });
}
