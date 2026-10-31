// Browser settings and diagnostic facts shared by the pane and fixtures.
export const BROWSER_CHECKS = ['launch', 'open', 'type', 'click', 'read'];

export function browserConfig(value) {
  return { enabled: value?.enabled !== false, chrome: value?.chrome?.trim() || null };
}

export const browserConfigKey = (value) => JSON.stringify(browserConfig(value));
// Automatic detection can change while the saved `chrome: null` stays the same.
export const browserDiagnosticKey = (value, resolvedChrome) => JSON.stringify({ ...browserConfig(value), resolvedChrome: resolvedChrome ?? null });
export const absoluteBrowserPath = (value) => !value || (value.startsWith('/') && !value.includes('\0'));

/** A partial or unrelated successful response must never turn the test green. */
export function browserProbePassed(result) {
  return result?.ok === true && result.isolated === true
    && BROWSER_CHECKS.every((name) => result.checks?.some((check) => check.name === name && check.ok === true));
}

export function browserTools(server) {
  return (server.tools ?? []).filter((tool) => /browser|playwright|puppeteer|chromium|chrome|(?:^|_)cdp(?:_|$)/i.test(tool.rawName ?? tool.name ?? ''));
}

export function browserServer(server) {
  return /hunt[-_]?proxy/i.test(server.name ?? '') || browserTools(server).length > 0;
}

/** Discovery alone is not availability: disabled tools and servers stay unavailable. */
export function availableBrowserTools(server) {
  if (server.enabled === false || server.status !== 'connected') return [];
  const disabled = new Set(server.disabledTools ?? []);
  return browserTools(server).filter((tool) => !disabled.has(tool.rawName) && !disabled.has(tool.name));
}
