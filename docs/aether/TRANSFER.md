# Folder transfer between devices

Move, copy or sync a project folder from one Aether instance to another
(this Mac, a VPS, …). The instances cannot reach each other: the VPS only
expose SSH, the Mac sits behind NAT. The client that can see both instances
(the macOS app with its SSH tunnels today, the iOS app later) therefore
streams the data: it exports from the source and imports into the target.
Every instance only needs the small, token-protected HTTP API below.

## Server API (every `dsh-codex web` instance)

All routes require the usual start token or a paired client token and sit
under `/api/transfer`. They are available on the loopback and the mobile
listener. Paths are absolute. "Allowed roots" are `$HOME`, every saved
workspace (`workspaces.json`) and the parent folder of every saved workspace;
`/` and paths outside the allowed roots are refused with 400. Symlinks are
never followed when deciding whether a path is allowed (canonicalize first).

| Route | Body / query | Result |
|---|---|---|
| `GET /api/transfer/roots` | – | `{"home": "/root", "roots": ["/root", "/srv/deepseek-workspaces", …], "defaultParent": "/root/workspaces"}` — `defaultParent` is the parent of the most recently used workspace, else `home`. |
| `POST /api/transfer/manifest` | `{"path": "/abs/folder", "excludes": ["node_modules", …]}` | `{"root": "/abs/folder", "name": "folder", "count": 123, "totalBytes": 456, "excluded": 7, "files": [{"path": "src/main.rs", "kind": "file", "size": 10, "mtime": 1700000000123, "mode": 420}, {"path": "link", "kind": "symlink", "target": "../x", "mtime": …}, {"path": "empty", "kind": "dir", "mtime": …}]}`. Paths are relative to `root`, `/`-separated, sorted. `mtime` is Unix milliseconds. Only empty directories appear as `dir` entries. |
| `POST /api/transfer/export` | `{"path": "/abs/folder", "excludes": […], "files": ["src/main.rs", …]}` (`files` optional: subset of manifest paths) | Streaming body `application/x-tar+zstd`, entries relative to the folder, mtimes/modes/symlinks preserved, no absolute paths or `..`. Header `X-Transfer-Count` with the entry count when known. |
| `POST /api/transfer/import?path=/abs/target&mode=merge\|fresh` | Body `application/x-tar+zstd` (streamed) | `{"written": 120, "bytes": 456, "skipped": 0}`. `path` is the destination folder itself; its parent must exist inside an allowed root. `fresh` fails with 409 if the folder exists and is not empty; `merge` overwrites files present in the archive and keeps the rest. Entries with absolute paths, `..`, or that would write through an existing symlink are rejected (400, nothing further is written). mtimes and modes are restored; symlinks are created as symlinks. |
| `POST /api/transfer/delete` | `{"path": "/abs/folder", "files": ["old.txt", "gone/"]}` | `{"deleted": 2}`. Removes those relative entries (files, symlinks, empty or whole directories listed with a trailing `/`) under the folder; refuses anything outside it. Used by sync with "delete extra files". |
| `POST /api/transfer/remove` | `{"path": "/abs/folder"}` | `{"removed": true}`. Deletes the whole folder after a verified move and drops it from `workspaces.json` when it was a saved workspace. Only inside the allowed roots, never a root itself. |

Default excludes (client-side constant, sent explicitly): `node_modules`,
`target`, `.venv`, `venv`, `__pycache__`, `DerivedData`, `.build`, `.cache`,
`.DS_Store`. `.git` is included. Excludes match a path component anywhere in
the relative path. Hidden files are included.

After an import the client calls the existing `POST /api/workspaces {"path"}`
on the target so the folder shows up as a project there.

## Client flow (macOS app)

"Ordner übertragen …" (menu Ablage, and from the web UI's project menu via
the bridge method `dshxDesktop.transferFolder({path})`, offered only when
`capabilities.transferFolder` is true):

1. Source: instance (default: current) and folder (its saved workspaces,
   or a typed path; `pickFolder` on the local instance). Target: another
   instance, destination parent (default `defaultParent` from
   `/api/transfer/roots`), folder name (default: source name).
2. Mode: **Kopieren**, **Verschieben** (copy, verify, then `remove` on the
   source), **Synchronisieren** (source → target: files that are missing or
   differ in size or mtime (>2 s) are exported and imported with `merge`;
   optional "Zusätzliche Dateien am Ziel löschen" uses `delete`). Excludes
   editable, defaults preselected.
3. "Vorschau" fetches both manifests and shows files, bytes and, for sync,
   the number of new/changed/extra entries. "Starten" spools the export to a
   0600 temp file in the app's cache, uploads it to the import route with
   byte progress, then re-fetches the target manifest and verifies every
   transferred entry by size and mtime (±2 s). Only then a move removes the
   source. The temp file is deleted afterwards, also on failure.
4. Result: counts, bytes, duration; "Zum Ziel wechseln" switches instances.
   Errors keep the sheet open with the failing step named and a retry.

Both instances must be connected; the app connects them on demand. A transfer
never runs two at a time. The iOS app can use the same API through its
device list later; nothing in the API is Mac-specific.
