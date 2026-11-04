# Aether – brand

Status: brand package P4, 26.10.2026. Contract: [docs/aether/SPEC.md](../docs/aether/SPEC.md) §1 and §6.
Sheet: [preview.png](preview.png). Tokens: [colors.json](colors.json). Explorations and the
reasons for the chosen mark: [explorations/README.md](explorations/README.md).

## 1. Name and story

**Aether** is the product: the iOS app, the accounts hub, the web UI's title, mark and
favicon, the macOS app's name and icon. Earlier attempts shipped as Zeus and Oxygen;
Aether joins both lineages. In Greek myth Aether is the bright upper air the gods breathe
(Zeus); in classical physics it is the fifth element, the substance of the heavens in
which the celestial spheres turn (the element after Oxygen's four); and in everyday
German, things reach every device „über den Äther“. That is what the product does: your
phone talks to the agent on your Mac and on your servers, over the air, wherever you are.

Spelling: *Aether*, capital A, Latin spelling in both languages (never „Äther“, never
„AETHER“, never *aether* outside identifiers such as `aether://`, `aether-hub`,
`de.yousufzai.aether`). In German compounds use a hyphen: *Aether-App*. In English: *Aether
app*. Where the technical name matters, docs say *Aether (dsh-codex)*.

Runner-up names, kept in the SPEC in case the user prefers one: Talos, Proteus.

## 2. Voice

Calm, precise, friendly. The product keeps you company while work runs; it never shouts.

- **German uses the informal „du“** in every UI string, push notification and mail.
  English uses a neutral *you*.
- **No exclamation marks.** Not in buttons, toasts, errors, or marketing.
- **Sentences, not codes.** An error says what happened, and what to do next, in one or
  two short sentences. The technical detail (status, code) may follow in a smaller line.
- **Say what happens next.** „Der Link gilt, bis du ihn widerrufst.“ beats „Link erstellt“.
- **Name things the way the user does.** The user's machines are *Geräte / devices*
  (the Mac, VPS 1, VPS 2); a phone is a *client* only in code; a *Chat* is a session;
  a *Projekt* is a folder; an approval is a *Freigabe*.
- **Numbers and dates in the reader's locale.** German: `1.234`, `14,2 %`, `vor 3 h`.

| Do | Don't |
|---|---|
| „Verbunden über SSH.“ | „Erfolgreich verbunden!“ |
| „VPS 2 ist gerade nicht erreichbar. Prüfe die Verbindung und versuche es erneut.“ | „Fehler 503“ |
| „Der Link enthält ein Geheimnis und gilt, bis du ihn widerrufst.“ | „Achtung: Dieser Link ist geheim!!“ |
| “Waiting for your approval.” | “Approval required!” |
| „Gerät hinzufügen“ | „Jetzt Gerät hinzufügen und loslegen“ |

## 3. The mark

A ring and a point on it: the sphere of the upper air and the thing that moves in it.
Read it as the agent reaching your phone, or your phone reaching the Mac, over the aether.
It is one shape in one colour; the glow is optional and lives only on the app icon.

### Construction

32-unit grid, `viewBox="0 0 32 32"`:

| Element | Geometry |
|---|---|
| Ring | centre (16, 16), radius 10.4 on the centre line, stroke 3 → outer radius 11.9, inner radius 8.9 |
| Point | radius 4, centre on the ring at 45°: (23.354, 8.646) |
| Clear space | the 4.1-unit margin of the box on every side; never less than one ring stroke |
| Minimum size | 16 px (favicon); 29 px (iOS settings icon) has room to spare |

The file is a single `<path>` with `fill-rule="nonzero"` (outer circle, inner circle in
the opposite direction, point), so any renderer can tint it as one shape; the favicon and
the web sidebar use the same path. The ring must stay closed, the point must stay on the
ring at 45°, and there is never a second point.

| File | Use |
|---|---|
| [aether-mark.svg](aether-mark.svg) | adaptive: ink `#121420` on light, `#F2F3F8` on dark via `prefers-color-scheme` |
| [aether-mark-light.svg](aether-mark-light.svg), [aether-mark-dark.svg](aether-mark-dark.svg) | fixed colours for places without CSS media queries (Xcode asset catalogs: light + dark appearance) |
| [aether-mark-template.svg](aether-mark-template.svg) | `currentColor`; in SwiftUI `.renderingMode(.template)` |
| [ui/favicon.svg](../ui/favicon.svg) | the mark in the web UI's ink (`#1a1c1f` / `#fff`) |

### Colour of the mark

In UI chrome the mark is **text-coloured** (`text` on light, `text` on dark), like the
favicon and the sidebar. The accent is for the app icon, the glow, links, buttons and
selection. A small accent-coloured mark is allowed as a device glyph inside a tinted
tile (see the iPhone card on the sheet), never as the brand slot.

### The glow (app icon only)

Radial gradient inside the ring, from the centre to the inner edge, plus a small halo
around the point (radius 1 → 2.6 point radii). Light icon: white at 22 % → 0, halo
white 28 % → 0. Dark icon: `#5B7CFA` at 42 % → 0, halo `#8AA2FF` 30 % → 0. Tinted icon:
white 20 % / 24 %. The glow never animates and never appears in UI chrome.

### App icons

- iOS: [ios/AppIcon.appiconset](ios/AppIcon.appiconset) (1024 px, square corners; iOS
  masks them): `AppIcon.png` (light: gradient `#5F80FA` → `#3454E4`, white mark),
  `AppIcon-dark.png` (`#1B2040` → `#0E0F14`, mark `#8AA2FF`), `AppIcon-tinted.png`
  (grayscale on `#1C1C1E` → `#000000`; iOS maps luminance to the user's tint). The mark's
  32-unit box is 760 px, so the ring spans 55 % of the icon.
- Launch screen: [ios/LaunchMark.png](ios/LaunchMark.png) @1x/@2x/@3x (120 pt box, ink
  `#121420`, transparent) and `LaunchMark-dark*.png` (`#F2F3F8`) for the dark appearance.
- macOS: `desktop/Tools/make-icon.swift` draws the same composition on the 824 pt body
  with the standard 100 pt margin, shadow and hairline highlight, into an `.iconset`
  for `iconutil`.

Regenerate (pixel-exact, CoreGraphics; `magick` is only for previews):

```sh
xcrun swiftc -O brand/tools/make-ios-icon.swift -o /tmp/make-ios-icon && /tmp/make-ios-icon brand/ios
xcrun swiftc -O desktop/Tools/make-icon.swift -o /tmp/make-icon && /tmp/make-icon /tmp/AppIcon.iconset && iconutil -c icns /tmp/AppIcon.iconset -o /tmp/AppIcon.icns
xcrun swiftc -O brand/tools/make-preview.swift -o /tmp/make-preview && /tmp/make-preview brand
python3 brand/tools/contrast.py
```

## 4. Wordmark and lockup

The wordmark is the word *Aether* in the **system font at weight 650 with tracking
−0.035 em** (SF Pro on Apple platforms, where the variable `wght` axis makes 650 exact;
the web UI's stack elsewhere, where it falls back to 600 or 700). No outlines, no
external fonts: [aether-wordmark.svg](aether-wordmark.svg) (and `-light`/`-dark`) carries
live `<text>`, and the reference rendering is on [preview.png](preview.png). Do not use
the SVG wordmark in an Xcode asset catalog (it needs the font at render time); draw the
text with the system font instead:

```swift
// UIKit / AppKit: the system font at weight 650 through the variation axis
let base = UIFont.systemFont(ofSize: 22, weight: .semibold)
let desc = base.fontDescriptor.addingAttributes([
    kCTFontVariationAttribute as UIFontDescriptor.AttributeName: [2003265652: 650]   // 'wght'
])
let wordmarkFont = UIFont(descriptor: desc, size: 22)
// SwiftUI without the axis: .font(.system(size: 22, weight: .semibold)).kerning(-0.77)
```

Lockup ([aether-lockup.svg](aether-lockup.svg), `-light`, `-dark`): the ring's outer
diameter is 1.02 cap heights, its centre sits on the cap mid-line, the gap between ring
and *A* is 0.4 cap heights. At text sizes below 20 px the mark may sit next to the
product name in the UI's own text style instead (the web sidebar: 18 px mark, 7 px gap,
`.brand` at 600 / −0.01 em).

## 5. Colour

Token names are fixed by the SPEC; the values below are the refined ones in
[colors.json](colors.json). The iOS app and the hub read the file; the web UI keeps its
own Codex-derived neutrals in `ui/styles/tokens.css` (mapping below).

| Token | Light | Dark | Role |
|---|---|---|---|
| `accent` | `#3E62F0` | `#8AA2FF` | buttons, links, selection, the icon's glow |
| `accentSoft` | `#E8EDFF` | `#1D2240` | tinted backgrounds, chips |
| `online` | `#0F8A68` | `#5EEAD4` | status dot: ready / running |
| `warning` | `#B97400` | `#F5B544` | status dot: waiting for approval |
| `danger` | `#C43D3D` | `#FF7B7B` | errors, destructive actions (also as text) |
| `bg` | `#FAFAFC` | `#0E0F14` | window background |
| `surface` | `#FFFFFF` | `#161822` | cards, sheets |
| `surfaceRaised` | `#F2F3F8` | `#1E2130` | rows, inputs, tiles |
| `border` | `#E3E5EE` | `#2A2E40` | hairlines |
| `text` | `#121420` | `#F2F3F8` | primary text |
| `textMuted` | `#646980` | `#9AA0B8` | secondary text |

Changes against the provisional values and why: `accent` light `#5B7CFA` → `#3E62F0`
(3.7:1 → 4.5:1 on `surfaceRaised`, so links and white-on-accent buttons meet AA);
`textMuted` light `#6B7085` → `#646980` (4.4:1 → 4.9:1 on `surfaceRaised`); `online`
light `#15A37A` → `#0F8A68` and `warning` light `#D98B00` → `#B97400` (status dots below
3:1 on the raised surface); `danger` light `#D64545` → `#C43D3D` (so error text passes
4.5:1 everywhere). The dark values stay.

Contrast (WCAG 2.x, `python3 brand/tools/contrast.py`), the lowest of `bg` / `surface` /
`surfaceRaised` unless noted:

| Pair | Light | Dark | Minimum |
|---|---|---|---|
| `text` on surfaces | 16.5:1 | 14.4:1 | 4.5:1 |
| `textMuted` on surfaces | 4.9:1 | 6.2:1 | 4.5:1 |
| `accent` on surfaces (links) | 4.5:1 | 6.6:1 | 4.5:1 |
| text on `accent` (buttons): white in light, `bg` in dark | 5.0:1 | 7.9:1 | 4.5:1 |
| `text` on `accentSoft` | 15.7:1 | 14.0:1 | 4.5:1 |
| `danger` on surfaces (as text) | 4.6:1 | 6.4:1 | 4.5:1 |
| `online` on surfaces (dots, icons) | 3.9:1 | 10.8:1 | 3:1 |
| `warning` on surfaces (dots, icons) | 3.4:1 | 8.8:1 | 3:1 |

Rules:

- **Text on accent** is white in light mode and `bg` in dark mode (white on `#8AA2FF` is
  only 2.4:1).
- **Status colours carry dots, chips and icons, not sentences.** The word next to a dot is
  `text` or `textMuted`. `danger` is the exception and may colour error text.
- **`accentSoft` is the chip and tinted-tile background**; chip text is `accent`
  (4.3:1 light: fine for the 10–11 pt medium chip labels, not for body text) or `text`.
- **Hairlines are decorative.** An input is defined by its `surfaceRaised` fill, not by
  `border` alone; a focused control gets a 2 px `accent` ring.
- **Large areas stay neutral.** The accent fills buttons, chips and the app icon, never a
  screen; the glow is the only gradient in the brand.

Mapping to the web UI (`ui/styles/tokens.css`, unchanged in this round): `bg` → `--bg`,
`surface` → `--bg-1`, `surfaceRaised` → `--bg-module`, `border` → `--line-3`, `text` →
`--ink`, `textMuted` → `--ink-3`, `accent` → `--accent` / `--link`, `online` →
`--success`, `warning` → `--warn`, `danger` → `--error`.

## 6. Typography

System fonts only: SF Pro (Text/Display as the system chooses) on Apple platforms, the
UI's stack (`-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue",
Arial, sans-serif`) on the web, `ui-monospace, "SF Mono", Menlo, …` for code. Nothing is
downloaded.

| Role | iOS (Dynamic Type) | Web (existing) |
|---|---|---|
| Screen title | `.largeTitle` bold, tracking −0.02 em („Geräte“) | hero 21–24 px / 28–32 px |
| Card title / device name | `.headline` (semibold) | 15 px / 22 px, 600 |
| Body, chat text | `.body` | 14 px / 20 px (the UI's base) |
| Meta, timestamps, chips | `.subheadline` / `.footnote`, `.monospacedDigit()` for counts | 12–13 px, `--ink-3` |
| Code, diffs, paths | `.body` monospaced design | `--mono` 13 px |
| Wordmark | weight 650, −0.035 em (§4) | weight 650, −0.035 em; sidebar: `.brand` |

Tight tracking belongs to the wordmark and large titles only; body and small text keep
the font's own spacing. Line length in chat stays at the UI's 768 px column.

## 7. Spacing, shape, motion

- **4 pt grid.** iOS: 16 pt screen margins, 12–14 pt inside cards, 8 pt between related
  rows, 24 pt between groups. Web: the UI's existing values.
- **Radii:** 6 pt small controls, 9 pt chips (pill at 18 pt height), 12 pt cards (iOS
  16 pt), 16 pt sheets, 38–46 pt only on the mocked phone frame.
- **Elevation:** hairline + fill, not shadows. One soft shadow is allowed under floating
  layers (menus, the live-view image).
- **Motion:** 160–220 ms ease-out, no overshoot. While a device connects, the status dot
  may pulse (1.2 s ease-in-out), as the web UI's `is-starting` dot does; everything else
  is still. `prefers-reduced-motion` / Reduce Motion switches the pulse off. The mark
  and the glow never animate.

## 8. Iconography

- **Apple platforms: SF Symbols**, regular weight at body size, `.medium` inside buttons
  and chips, monochrome rendering (no multicolour, no hierarchical). Vocabulary:
  `laptopcomputer` (Mac), `server.rack` (VPS), `cloud` (through the hub), `iphone`
  (this phone), `qrcode.viewfinder` (scan), `link` (paste a link), `keyboard`
  (enter by hand), `checkmark.shield` (approval), `arrow.up` (send), `stop.fill` (stop),
  `folder` (project), `gearshape` (settings).
- **Web: the UI's own 16 px line icons** (`ui/src/icons.js`: stroke 1.3, round caps and
  joins), nothing else; no icon fonts, no network icons.
- One set per surface; never mix. The mark is a brand element, not an icon for a
  function: it does not stand for „connect“, „devices“ or „status“.
- Status is shown by a dot plus a word, never by the mark's point.

## 9. Do and don't

Do

- Use the mark as one colour, text-coloured in UI chrome, white on the accent icon.
- Keep the clear space (the box margin) and never go below 16 px.
- Use the lockup for welcome and about screens; the mark alone for favicons, tiles,
  the sidebar.
- Write short, calm sentences in „du“; say what happens next.
- Keep screens neutral: colour means status or action.

Don't

- Don't rotate, tilt, mirror or open the ring, move the point, or add a second point.
- Don't put letters in the mark, and don't outline the ring thinner than 3/32 of its box.
- Don't use the glow outside the app icon, and don't add gradients elsewhere.
- Don't fill screens, cards or the sidebar with the accent.
- Don't use exclamation marks, „Äther“, or the old prompt glyph (`>_`).
- Don't ship the SVG wordmark where the system font is not available at render time.

## 10. Rename plan

User-facing surfaces carry Aether now: the web UI title, favicon and `t.brand`, the Mac
app's `CFBundleName` / `CFBundleDisplayName` and icon, the iOS bundle id
`de.yousufzai.aether`, the URL scheme `aether://`, the hub binary `aether-hub`, the Bonjour
type `_aether._tcp`, the certificate CN „Aether on <name>“ and the server's log line
`aether: listening …`. Docs say *Aether (dsh-codex)* where the binary matters.

These keep their old names in this round, on purpose:

| Keeps its name | Why |
|---|---|
| Binary `dsh-codex` (`dsh-codex web`, `dsh-codex pair`), `/usr/local/bin/dsh-codex` on the VPS | referenced by the systemd unit, the Mac app's `tokenCommand`, the OracleVPS runbooks and every doc |
| Config `~/.dsh-codex`, `DSHX_HOME` (`/var/lib/dsh-codex` on the VPS), `config.toml` `[codex.*]` | user data and credentials live there; a move needs a migration |
| Crates `dshx-*`, the `dshx` prefixes in code | internal; a rename churns the whole workspace for no user-visible gain |
| macOS bundle id `de.yousufzai.dsh-codex`, executable `dsh-codex-desktop` | a new bundle id is a new app to macOS: permissions, Keychain items, window state and Login Items are lost |
| systemd unit `deepseek-harness`, service user `deepseek` on VPS 1 and 2 | documented in the OracleVPS folder (`state.md`, `log.md`) and used by the SSH token command |

Proposed order, each step its own release with a changelog entry, and nothing on the
VPS without the user's go:

1. **Docs and help** say *Aether (dsh-codex)*; the CLI's `--help` header says „Aether“.
2. **Command alias:** install `aether` next to `dsh-codex` (symlink or an `argv[0]`
   check); `aether-hub` already carries the new name.
3. **Config path:** read `AETHER_HOME`, then `DSHX_HOME`, then `~/.aether` if it exists,
   else `~/.dsh-codex`; offer `dsh-codex migrate` that moves the directory and leaves a
   symlink. Credentials (`credentials.toml`, `mobile/`) move with it; nothing is copied.
4. **VPS units:** add `aether.service` with `Alias=deepseek-harness.service`, same user,
   paths and token command; update the pairing `--unit` default and the OracleVPS
   `state.md` / `log.md` per its `Agents.md`; keep the `deepseek` user.
5. **macOS bundle id** `de.yousufzai.aether` only together with a Keychain and
   preferences migration and a release note (users re-grant permissions once); the
   executable name can change in the same build.
6. **Crates** last, or never.

Risks: the SSH `tokenCommand` in pairing payloads and `instances.json` names the unit;
pinned links and QR codes stay valid because they carry a server id, not a name; the
Mac app's `CFBundleIdentifier` is in the launch-at-login registration.

## 11. Files

| Path | What |
|---|---|
| `brand/BRAND.md` | this document |
| `brand/colors.json` | tokens, `{"light": {…}, "dark": {…}}`, the SPEC's names |
| `brand/aether-mark*.svg`, `aether-wordmark*.svg`, `aether-lockup*.svg` | mark, wordmark, lockup; adaptive, light, dark, template |
| `brand/explorations/` | the three directions and the decision |
| `brand/ios/AppIcon.appiconset/` | iOS 18 icon set: light, dark, tinted + `Contents.json` |
| `brand/ios/LaunchMark*.png` | launch-screen mark @1x/@2x/@3x, light and dark |
| `brand/tools/make-ios-icon.swift` | renders the iOS icons and launch marks |
| `brand/tools/make-preview.swift` | renders `brand/preview.png` |
| `brand/tools/contrast.py` | WCAG table for `colors.json` |
| `brand/preview.png` | the sheet: icon at 1024/180/60 px, mark, wordmark, palette, iPhone card |
| `brand/screenshots/` | the web UI with the new mark, light and dark |
| `desktop/Tools/make-icon.swift`, `desktop/Info.plist` | Mac icon and display name |
| `ui/favicon.svg`, `ui/index.html`, `ui/src/i18n.js` (`brand`), `ui/src/components/instances.js` | web UI |

## 12. Open points

- `ui/src/icons.js` still exports the old prompt-glyph `Mark` used on the start page
  (`app.js` hero) and the boot screen; not in this package's scope. Suggested swap: the
  same path as `instances.js`'s `MARK` with `fill="currentColor"`, no `rect`.
- `ui/styles/tokens.css` keeps the Codex-derived neutrals; adopting `colors.json` on the
  web is a separate decision (§5 has the mapping).
- The wordmark SVGs render with whatever system font the viewer has; on Windows/Linux
  the shapes differ (Segoe UI / Roboto). The PNG on the sheet is the reference.
- Apple's current HIG page could not be fetched for the exact wording on dark/tinted
  icon backgrounds; the dark and tinted icons ship with opaque backgrounds, which is
  safe either way (iOS draws its own gradient only under transparent ones).
