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
listener. Paths are absolute (`~/…` is expanded with the server's home;
`..` components are refused). "Allowed roots" are `$HOME`, every project
folder of `GET /api/workspaces` (`workspaces.json` plus the folders of
existing chats, which the UI lists as projects too) and the parent folder of
each; `/` and paths outside the allowed roots are refused with 400. The
canonical path decides, so a symlink never leads out of the roots.

| Route | Body / query | Result |
|---|---|---|
| `GET /api/transfer/roots` | – | `{"home": "/root", "roots": ["/root", "/srv/deepseek-workspaces", …], "defaultParent": "/root/workspaces"}` — `defaultParent` is the parent of the most recently used workspace, else `home`. |
| `POST /api/transfer/manifest` | `{"path": "/abs/folder", "excludes": ["node_modules", …]}` | `{"root": "/abs/folder", "name": "folder", "count": 123, "totalBytes": 456, "excluded": 7, "skipped": 0, "files": [{"path": "src/main.rs", "kind": "file", "size": 10, "mtime": 1700000000123, "mode": 420}, {"path": "link", "kind": "symlink", "target": "../x", "mtime": …}, {"path": "empty", "kind": "dir", "mtime": …, "mode": 493}]}`. `root` is canonical. Paths are relative to `root`, `/`-separated, sorted. `mtime` is Unix milliseconds. Only empty directories appear as `dir` entries; a folder whose entries are all excluded counts as empty. `skipped` counts sockets, devices, FIFOs and names that are not UTF-8 (never transferred). The walk never follows symlinks. |
| `POST /api/transfer/export` | `{"path": "/abs/folder", "excludes": […], "files": ["src/main.rs", …]}` (`files` optional: subset of manifest paths) | Streaming body `application/x-tar+zstd`: exactly the manifest entries (files, symlinks, empty folders) relative to the folder, GNU headers, mtimes in whole seconds, modes and symlinks preserved, no absolute paths or `..`. Headers `X-Transfer-Count` (entries) and `X-Transfer-Bytes` (uncompressed file bytes). A read error mid-stream aborts the body, so the import fails instead of seeing a short archive. |
| `POST /api/transfer/import?path=/abs/target&mode=merge\|fresh` | Body `application/x-tar+zstd` (streamed) | `{"path": "/canonical/target", "written": 120, "bytes": 456, "skipped": 0}`. `path` is the destination folder itself; its parent must exist inside an allowed root, the folder must not be a symlink and is created when missing. `mode` defaults to `fresh`, which fails with 409 if the folder exists and is not empty; `merge` overwrites entries present in the archive and keeps the rest. Entries with absolute paths, `..`, or a symlink or file on the way to them are rejected (400 `rejected`, nothing further is written; the answer reports `written` and `bytes` so far). A file or symlink is written next to its place and renamed over it, so an existing symlink at that place is replaced, never written through. A folder where the archive has a file (or the reverse) is rejected too. mtimes and modes (without setuid/setgid) are restored; symlinks are created as symlinks; other entry types are counted in `skipped`. A truncated upload fails (400 `failed`). |
| `POST /api/transfer/delete` | `{"path": "/abs/folder", "files": ["old.txt", "gone/"]}` | `{"deleted": 2}`. Removes those relative entries (files, symlinks, empty or whole directories listed with a trailing `/`) under the folder; missing ones are skipped. Nothing is deleted when an entry is invalid (`..`, absolute, empty) or lies behind a symlink or file. A bare name of a non-empty folder fails. Used by sync with "delete extra files". |
| `POST /api/transfer/remove` | `{"path": "/abs/folder"}` | `{"removed": true, "path": "/canonical/folder"}`. Deletes the whole folder after a verified move and drops it from `workspaces.json` when it was a saved workspace. Only strictly inside an allowed root (a saved project lies inside its parent); never `$HOME`, a project parent or a folder above one of them; a path that is itself a symlink is refused. Symlinks inside are removed, not followed. |

Errors are `{"error": "…", "code": "…"}`: `invalid` (400), `outside-roots`
(400), `not-found` (404), `not-empty` (409), `rejected` and `failed` (400,
with `written`/`bytes` for an import), `unsupported` (404, the backend offers
no transfer).

Default excludes (client-side constant, sent explicitly): `node_modules`,
`target`, `.venv`, `venv`, `__pycache__`, `DerivedData`, `.build`, `.cache`,
`.DS_Store`. `.git` is included. Excludes match a path component anywhere in
the relative path; an exclude with a `/` matches that relative path and
everything below it. Hidden files are included.

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
   optional "Zusätzliche Dateien am Ziel löschen" uses `delete`; a folder
   the source lacks entirely goes as one `name/`). Excludes editable,
   defaults preselected.
3. "Vorschau" fetches both manifests and shows files, bytes and, for sync,
   the number of new/changed/extra entries. "Starten" spools the export to a
   0600 temp file in the app's cache, uploads it to the import route with
   byte progress, then re-fetches the target manifest and verifies every
   transferred entry by size and mtime (±2 s). Only then a move removes the
   source. The temp file is deleted afterwards, also on failure.
4. Result: counts, bytes, duration; "Zum Ziel wechseln" switches instances.
   Errors keep the sheet open with the failing step named and a retry.

Both instances must be connected; the app connects them on demand. A transfer
never runs two at a time. `capabilities.transferFolder` is true once the app
has at least two instances; `transferFolder` answers `false` when another
sheet is open in the window. Move asks for confirmation first. The iOS app can use the same API through its
device list later; nothing in the API is Mac-specific.
