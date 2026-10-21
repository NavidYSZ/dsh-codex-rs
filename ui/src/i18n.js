// German or English from navigator.language (?lang=de|en overrides for testing).
const forced = new URLSearchParams(location.search).get('lang');
export const lang = (forced || navigator.language || 'en').toLowerCase().startsWith('de') ? 'de' : 'en';
export const de = lang === 'de';
document.documentElement.lang = lang;

const EN = {
  brand: 'dsh-codex',
  newChat: 'New chat',
  settings: 'Settings',
  noSessions: 'No chats yet',
  deleteChat: 'Delete chat',
  confirmDelete: 'Delete this chat? This cannot be undone.',
  toggleSidebar: 'Toggle sidebar',
  heroIn: ['What should we work on in ', '?'],
  heroPlain: 'What should we work on?',
  pickWorkspace: 'Choose a workspace',
  addWorkspace: 'Add folder',
  addWorkspaceHint: 'Absolute path, e.g. /Users/you/code/app',
  add: 'Add',
  workspace: 'Workspace',
  placeholderHero: 'Ask anything',
  placeholderNoWorkspace: 'Choose a workspace first',
  placeholder: 'Ask for follow-up changes',
  placeholderNoKey: 'Add an API key in Settings to start',
  attach: 'Attach image',
  removeImage: 'Remove image',
  send: 'Send',
  stop: 'Stop',
  model: 'Model',
  effort: 'Reasoning',
  efforts: { off: 'Off', low: 'Low', medium: 'Medium', high: 'High', max: 'Max' },
  access: {
    'read-only': ['Read only', 'Reads files, asks before any change'],
    'workspace-write': ['Auto', 'Edits files in this folder, asks for more'],
    'danger-full-access': ['Full access', 'Edits any file and uses the network without asking'],
  },
  thinking: 'Thinking',
  thoughtFor: (s) => `Thought for ${s}`,
  thought: 'Thought',
  workedFor: (s) => `Worked for ${s}`,
  stopped: 'Stopped',
  turnFailed: 'This turn failed',
  maxTokens: 'Output token limit reached. Send “continue” to let the model resume.',
  interrupted: 'The agent exited before this turn finished.',
  retrying: (n, max, s, why) => `Retrying model request (${n}/${max}) in ${s}${why ? ` · ${why}` : ''}`,
  compacted: 'Context compacted',
  openSettings: 'Open settings',
  // tool rows
  ran: 'Ran', running: 'Running', exit: (c) => `Exit ${c}`,
  read: 'Read', reading: 'Reading', lines: (a, b) => `L${a}–${b}`,
  edited: 'Edited', editing: 'Editing', created: 'Created', creating: 'Creating', wrote: 'Wrote',
  searched: 'Searched', searching: 'Searching',
  files: (n) => `${n} ${n === 1 ? 'file' : 'files'}`,
  matches: (n, f) => `${n} ${n === 1 ? 'match' : 'matches'} in ${f} ${f === 1 ? 'file' : 'files'}`,
  noMatches: 'no matches',
  fetched: 'Fetched', fetching: 'Fetching',
  searchedWeb: 'Searched the web', searchingWeb: 'Searching the web',
  viewedImage: 'Viewed image',
  plan: 'Plan', updatedPlan: 'Updated plan', planDone: (d, n) => `${d} of ${n} done`,
  called: 'Called', calling: 'Calling',
  waitingApproval: 'Waiting for approval',
  denied: 'Denied',
  output: 'Output', noOutput: 'No output',
  showMore: (n) => `Show ${n} more lines`, showLess: 'Show less',
  copy: 'Copy', copied: 'Copied',
  failed: 'Failed',
  // approval
  approveTitle: {
    bash: 'Run this command with wider access?',
    edit: 'Allow this edit outside the sandbox?',
    write: 'Allow writing this file outside the sandbox?',
    other: 'Allow this action?',
  },
  allowOnce: 'Allow once', deny: 'Deny',
  // banners
  noKey: 'No DeepSeek API key yet. Add one in Settings to start.',
  reconnecting: 'Connection lost. Reconnecting…',
  offline: 'Can’t reach dsh-codex. Is the server still running?',
  unauthorized: 'This page’s access token is missing or expired. Open the link that dsh-codex printed.',
  fixture: 'Fixture',
  fixtureReply: 'Fixture mode: no agent is connected, so this message only shows how a turn looks.',
  // settings
  general: 'General', theme: 'Theme', themes: { system: 'System', light: 'Light', dark: 'Dark' },
  language: 'Language', languageValue: 'English (from your browser)',
  defaults: 'Defaults for new chats',
  sandbox: 'Access', approval: 'Approvals',
  approvals: { ask: 'Ask when an action needs more access', never: 'Never ask (deny instead)' },
  account: 'Provider', apiKey: 'DeepSeek API key', apiKeySet: 'Saved', apiKeyMissing: 'Not set yet',
  apiKeyHint: 'Kept on this computer, never shown again',
  apiKeyPlaceholder: 'sk-…', save: 'Save', saved: 'Saved', replace: 'Replace key', cancel: 'Cancel',
  about: 'About', version: 'Version', platform: 'Platform', tools: 'Mac tools',
  toolNames: { browser: 'Browser', app: 'Apps', computer: 'Computer' },
  on: 'on', off: 'off', back: 'Back',
  tokens: (n) => `${n} tokens`, cached: (p) => `${p}% cached`, context: (p) => `${p}% of context`,
  justNow: 'now',
  scrollDown: 'Scroll to bottom',
  // sidebar
  search: 'Search', pinned: 'Pinned', projects: 'Projects', chats: 'Chats', activity: 'Activity',
  activityFilters: { all: 'All', unread: 'Unread', active: 'Active', pinned: 'Pinned' },
  markAllRead: 'Mark all as read', nothingHere: 'Nothing here',
  noProject: 'No project', noProjectHint: 'A private folder for this chat',
  newChatIn: (n) => `New chat in ${n}`, newChatNoProject: 'New chat without a project',
  moreChats: (n) => `Show ${n} more`, fewerChats: 'Show less',
  rename: 'Rename', pin: 'Pin', unpin: 'Unpin', markUnread: 'Mark as unread', markRead: 'Mark as read',
  archive: 'Archive', unarchive: 'Restore', archived: 'Archived chats', noArchived: 'No archived chats',
  copyPath: 'Copy folder path', more: 'More', byProject: 'Back to projects', addProject: 'Add project',
  statusRunning: 'Running', statusWaiting: 'Waiting for you', statusUnread: 'Unread',
  // search palette
  paletteHint: 'Search chats and actions', actions: 'Actions', noResults: 'No results',
  toggleTheme: 'Switch light or dark', inProject: (n) => `in ${n}`,
  // desktop instances
  instances: 'Instances', addInstance: 'Add instance…', manageInstances: 'Manage instances…',
  // side panel
  changes: 'Changes', filesTab: 'Files', sidePanel: 'Side panel', closePanel: 'Close panel', closeTab: 'Close',
  filesChanged: (n) => `${n} ${n === 1 ? 'file' : 'files'} changed`, review: 'Review',
  preview: 'Preview', source: 'Source', reload: 'Reload', openNewTab: 'Open in new tab', openInEditor: 'Open in editor',
  noChanges: 'No files changed in this chat yet', notFound: 'File not found', tooLarge: 'Too large to preview',
  binary: 'Binary file, no preview', outsideFolder: 'Only files in this chat’s folder can be previewed',
  emptyFolder: 'Empty folder', firstRows: (n) => `First ${n} rows`, loading: 'Loading…', open: 'Open',
  // live view
  live: 'Live', liveActive: 'Working', liveIdle: 'Paused', liveEnded: 'Done', liveWaiting: 'Waiting for the first picture…',
  showInPanel: 'Show in panel', hide: 'Hide', stopAgent: 'Stop agent',
};

const DE = {
  ...EN,
  newChat: 'Neuer Chat',
  settings: 'Einstellungen',
  noSessions: 'Noch keine Chats',
  deleteChat: 'Chat löschen',
  confirmDelete: 'Diesen Chat löschen? Das lässt sich nicht rückgängig machen.',
  toggleSidebar: 'Seitenleiste ein/aus',
  heroIn: ['Woran sollen wir in ', ' arbeiten?'],
  heroPlain: 'Woran sollen wir arbeiten?',
  pickWorkspace: 'Arbeitsordner wählen',
  addWorkspace: 'Ordner hinzufügen',
  addWorkspaceHint: 'Absoluter Pfad, z. B. /Users/du/code/app',
  add: 'Hinzufügen',
  workspace: 'Arbeitsordner',
  placeholderHero: 'Frag etwas',
  placeholderNoWorkspace: 'Wähle zuerst einen Arbeitsordner',
  placeholder: 'Frag nach weiteren Änderungen',
  placeholderNoKey: 'Hinterlege in den Einstellungen einen API-Schlüssel',
  attach: 'Bild anhängen',
  removeImage: 'Bild entfernen',
  send: 'Senden',
  stop: 'Stoppen',
  model: 'Modell',
  effort: 'Nachdenken',
  efforts: { off: 'Aus', low: 'Niedrig', medium: 'Mittel', high: 'Hoch', max: 'Max' },
  access: {
    'read-only': ['Nur lesen', 'Liest Dateien, fragt vor jeder Änderung'],
    'workspace-write': ['Auto', 'Ändert Dateien in diesem Ordner, fragt für mehr'],
    'danger-full-access': ['Voller Zugriff', 'Ändert jede Datei und nutzt das Netz ohne Nachfrage'],
  },
  thinking: 'Denkt nach',
  thoughtFor: (s) => `${s} nachgedacht`,
  thought: 'Nachgedacht',
  workedFor: (s) => `${s} gearbeitet`,
  stopped: 'Gestoppt',
  turnFailed: 'Dieser Schritt ist fehlgeschlagen',
  maxTokens: 'Ausgabelimit erreicht. Schreib „weiter“, damit das Modell fortsetzt.',
  interrupted: 'Der Agent wurde beendet, bevor dieser Schritt fertig war.',
  retrying: (n, max, s, why) => `Neuer Versuch (${n}/${max}) in ${s}${why ? ` · ${why}` : ''}`,
  compacted: 'Kontext verdichtet',
  openSettings: 'Einstellungen öffnen',
  ran: 'Ausgeführt', running: 'Läuft', exit: (c) => `Exit ${c}`,
  read: 'Gelesen', reading: 'Liest',
  edited: 'Bearbeitet', editing: 'Bearbeitet', created: 'Erstellt', creating: 'Erstellt', wrote: 'Geschrieben',
  searched: 'Gesucht', searching: 'Sucht',
  files: (n) => `${n} ${n === 1 ? 'Datei' : 'Dateien'}`,
  matches: (n, f) => `${n} ${n === 1 ? 'Treffer' : 'Treffer'} in ${f} ${f === 1 ? 'Datei' : 'Dateien'}`,
  noMatches: 'keine Treffer',
  fetched: 'Abgerufen', fetching: 'Ruft ab',
  searchedWeb: 'Im Web gesucht', searchingWeb: 'Sucht im Web',
  viewedImage: 'Bild angesehen',
  plan: 'Plan', updatedPlan: 'Plan aktualisiert', planDone: (d, n) => `${d} von ${n} erledigt`,
  called: 'Aufgerufen', calling: 'Ruft auf',
  waitingApproval: 'Wartet auf Freigabe',
  denied: 'Abgelehnt',
  output: 'Ausgabe', noOutput: 'Keine Ausgabe',
  showMore: (n) => `${n} weitere Zeilen`, showLess: 'Weniger',
  copy: 'Kopieren', copied: 'Kopiert',
  failed: 'Fehlgeschlagen',
  approveTitle: {
    bash: 'Diesen Befehl mit mehr Rechten ausführen?',
    edit: 'Diese Änderung außerhalb der Sandbox erlauben?',
    write: 'Diese Datei außerhalb der Sandbox schreiben?',
    other: 'Diese Aktion erlauben?',
  },
  allowOnce: 'Einmal erlauben', deny: 'Ablehnen',
  noKey: 'Noch kein DeepSeek-API-Schlüssel. Hinterlege ihn in den Einstellungen.',
  reconnecting: 'Verbindung verloren. Verbinde neu …',
  offline: 'dsh-codex ist nicht erreichbar. Läuft der Server noch?',
  unauthorized: 'Das Zugriffstoken dieser Seite fehlt oder ist abgelaufen. Öffne den Link, den dsh-codex ausgegeben hat.',
  fixture: 'Fixture',
  fixtureReply: 'Fixture-Modus: Es ist kein Agent verbunden, diese Nachricht zeigt nur, wie ein Schritt aussieht.',
  general: 'Allgemein', theme: 'Erscheinungsbild', themes: { system: 'System', light: 'Hell', dark: 'Dunkel' },
  language: 'Sprache', languageValue: 'Deutsch (aus dem Browser)',
  defaults: 'Standard für neue Chats',
  sandbox: 'Zugriff', approval: 'Freigaben',
  approvals: { ask: 'Fragen, wenn eine Aktion mehr Rechte braucht', never: 'Nie fragen (stattdessen ablehnen)' },
  account: 'Anbieter', apiKey: 'DeepSeek-API-Schlüssel', apiKeySet: 'Gespeichert', apiKeyMissing: 'Noch nicht hinterlegt',
  apiKeyHint: 'Bleibt auf diesem Computer und wird nie wieder angezeigt',
  save: 'Speichern', saved: 'Gespeichert', replace: 'Schlüssel ersetzen', cancel: 'Abbrechen',
  about: 'Über', version: 'Version', platform: 'Plattform', tools: 'Mac-Werkzeuge',
  toolNames: { browser: 'Browser', app: 'Apps', computer: 'Computer' },
  on: 'an', off: 'aus', back: 'Zurück',
  tokens: (n) => `${n} Tokens`, cached: (p) => `${p} % aus dem Cache`, context: (p) => `${p} % vom Kontext`,
  justNow: 'jetzt',
  scrollDown: 'Nach unten',
  search: 'Suchen', pinned: 'Angepinnt', projects: 'Projekte', chats: 'Chats', activity: 'Aktivität',
  activityFilters: { all: 'Alle', unread: 'Ungelesen', active: 'Aktiv', pinned: 'Angepinnt' },
  markAllRead: 'Alle als gelesen markieren', nothingHere: 'Hier ist nichts',
  noProject: 'Ohne Projekt', noProjectHint: 'Eigener Ordner für diesen Chat',
  newChatIn: (n) => `Neuer Chat in ${n}`, newChatNoProject: 'Neuer Chat ohne Projekt',
  moreChats: (n) => `${n} weitere anzeigen`, fewerChats: 'Weniger anzeigen',
  rename: 'Umbenennen', pin: 'Anpinnen', unpin: 'Lösen', markUnread: 'Als ungelesen markieren', markRead: 'Als gelesen markieren',
  archive: 'Archivieren', unarchive: 'Wiederherstellen', archived: 'Archivierte Chats', noArchived: 'Keine archivierten Chats',
  copyPath: 'Ordnerpfad kopieren', more: 'Mehr', byProject: 'Zurück zu Projekten', addProject: 'Projekt hinzufügen',
  statusRunning: 'Läuft', statusWaiting: 'Wartet auf dich', statusUnread: 'Ungelesen',
  paletteHint: 'Chats und Aktionen suchen', actions: 'Aktionen', noResults: 'Keine Treffer',
  toggleTheme: 'Hell oder dunkel', inProject: (n) => `in ${n}`,
  instances: 'Instanzen', addInstance: 'Instanz hinzufügen …', manageInstances: 'Instanzen verwalten …',
  changes: 'Änderungen', filesTab: 'Dateien', sidePanel: 'Seitenpanel', closePanel: 'Panel schließen', closeTab: 'Schließen',
  filesChanged: (n) => `${n} ${n === 1 ? 'Datei' : 'Dateien'} geändert`, review: 'Ansehen',
  preview: 'Vorschau', source: 'Quelle', reload: 'Neu laden', openNewTab: 'In neuem Tab öffnen', openInEditor: 'Im Editor öffnen',
  noChanges: 'In diesem Chat wurden noch keine Dateien geändert', notFound: 'Datei nicht gefunden', tooLarge: 'Zu groß für die Vorschau',
  binary: 'Binärdatei, keine Vorschau', outsideFolder: 'Vorschau nur für Dateien im Ordner dieses Chats',
  emptyFolder: 'Leerer Ordner', firstRows: (n) => `Erste ${n} Zeilen`, loading: 'Lädt …', open: 'Öffnen',
  live: 'Live', liveActive: 'Arbeitet', liveIdle: 'Pausiert', liveEnded: 'Fertig', liveWaiting: 'Warte auf das erste Bild …',
  showInPanel: 'Im Panel zeigen', hide: 'Ausblenden', stopAgent: 'Agent stoppen',
};

export const t = de ? DE : EN;

/** "4s", "1m 12s", "2h 3m". */
export function duration(ms) {
  const s = Math.max(1, Math.round(ms / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${s % 60}s`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}

/** Short relative age for the sidebar: "now", "5m", "3h", "2d", then a date. */
export function age(ms, now = Date.now()) {
  const s = Math.max(0, (now - ms) / 1000);
  if (s < 60) return t.justNow;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  if (s < 86400 * 7) return `${Math.floor(s / 86400)}d`;
  return new Date(ms).toLocaleDateString(lang, { month: 'short', day: 'numeric' });
}

/** 1234 → "1.2k". */
export function compact(n) {
  if (n < 1000) return String(n);
  if (n < 1e6) return `${(n / 1000).toFixed(n < 10000 ? 1 : 0)}k`;
  return `${(n / 1e6).toFixed(1)}M`;
}

export const quote = (v) => (de ? `„${v}“` : `“${v}”`);
export const clip = (v, n) => (v.length > n ? v.slice(0, n) + '…' : v);
