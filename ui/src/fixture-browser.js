// ?browser=disabled|missing|failure exercises the Browser Use settings pane.
// Like the real process, fixture settings save without replacing its active browser.
import { ApiError } from './api.js';
import { absoluteBrowserPath, BROWSER_CHECKS, browserConfig, browserConfigKey } from './browser-settings.js';

const CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const clone = (value) => JSON.parse(JSON.stringify(value));

export function createBrowserFixture(info, variant = '') {
  info.settings = { ...info.settings, browser: browserConfig(info.settings?.browser) };
  if (variant === 'disabled') info.settings.browser.enabled = false;
  const initial = clone(info.settings.browser);
  let detectedChrome = variant === 'missing' ? null : CHROME;
  const resolve = (config) => config.chrome || detectedChrome;
  const active = {
    enabled: initial.enabled, available: initial.enabled && !!resolve(initial),
    chrome: resolve(initial), profile: '/Users/alex/.dsh-codex/browser-profile', running: false,
  };
  info.tools = { ...info.tools, browser: active.available };
  return {
    // Allows DOM regressions to model installation/removal without changing settings.
    setDetectedChrome(chrome) { detectedChrome = chrome; },
    patch(value) {
      if (!value || typeof value !== 'object' || Array.isArray(value)
          || (value.enabled !== undefined && typeof value.enabled !== 'boolean')
          || (value.chrome !== undefined && value.chrome !== null && typeof value.chrome !== 'string')) {
        throw new ApiError(400, 'invalid browser settings');
      }
      const next = browserConfig({ ...info.settings.browser, ...value });
      if (!absoluteBrowserPath(next.chrome) || next.chrome?.startsWith('/missing')) {
        throw new ApiError(400, 'Chrome must be an existing executable at an absolute path.');
      }
      info.settings.browser = next;
    },
    status() {
      const configured = browserConfig(info.settings.browser);
      return clone({ configured, active, resolvedChrome: resolve(configured), restartRequired: browserConfigKey(initial) !== browserConfigKey(configured) });
    },
    async test() {
      const configured = browserConfig(info.settings.browser);
      const chrome = resolve(configured);
      await new Promise((resolve) => setTimeout(resolve, 350));
      const ok = configured.enabled && !!chrome && variant !== 'failure';
      return {
        ok, chrome, isolated: true, checkedAt: Date.now(), durationMs: 350,
        checks: BROWSER_CHECKS.map((name, index) => ({ name, ok: ok || (configured.enabled && !!chrome && index < 2) })),
        ...(!ok ? { error: !configured.enabled ? 'The background browser is disabled.' : !chrome ? 'No Chrome executable found.' : 'Typing could not be verified (fixture).' } : {}),
      };
    },
  };
}
