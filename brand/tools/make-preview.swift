// Renders brand/preview.png: the Aether brand at a glance (icon at 1024/180/60 px,
// mark, wordmark, lockup, palette, a mocked iPhone card). CoreGraphics + CoreText,
// system font; the wordmark uses the SF variable weight axis at 650.
//
//   xcrun swiftc -O brand/tools/make-preview.swift -o /tmp/make-preview
//   /tmp/make-preview brand           # reads brand/colors.json and brand/ios/*, writes brand/preview.png
import AppKit
import CoreText
import Foundation
import ImageIO

let brandDir = URL(fileURLWithPath: CommandLine.arguments.dropFirst().first ?? "brand")
let scale: CGFloat = 2
let W: CGFloat = 1440, H: CGFloat = 980

// MARK: colours
let srgb = CGColorSpace(name: CGColorSpace.sRGB)!
func hex(_ s: String, _ alpha: CGFloat = 1) -> CGColor {
    var v: UInt64 = 0
    Scanner(string: String(s.dropFirst())).scanHexInt64(&v)
    return CGColor(colorSpace: srgb, components: [CGFloat((v >> 16) & 0xff) / 255, CGFloat((v >> 8) & 0xff) / 255, CGFloat(v & 0xff) / 255, alpha])!
}
let tokensJSON = try! JSONSerialization.jsonObject(with: Data(contentsOf: brandDir.appendingPathComponent("colors.json"))) as! [String: Any]
let light = tokensJSON["light"] as! [String: String]
let dark = tokensJSON["dark"] as! [String: String]
let order = ["accent", "accentSoft", "online", "warning", "danger", "bg", "surface", "surfaceRaised", "border", "text", "textMuted"]

func luminance(_ s: String) -> CGFloat {
    let c = hex(s).components!
    func lin(_ v: CGFloat) -> CGFloat { v <= 0.04045 ? v / 12.92 : pow((v + 0.055) / 1.055, 2.4) }
    return 0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])
}
func contrast(_ a: String, _ b: String) -> CGFloat {
    let la = luminance(a), lb = luminance(b)
    return (max(la, lb) + 0.05) / (min(la, lb) + 0.05)
}

// MARK: context
let ctx = CGContext(data: nil, width: Int(W * scale), height: Int(H * scale), bitsPerComponent: 8, bytesPerRow: 0,
                    space: srgb, bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)!
ctx.setAllowsAntialiasing(true); ctx.setShouldAntialias(true); ctx.setShouldSmoothFonts(true)
ctx.interpolationQuality = .high
ctx.scaleBy(x: scale, y: scale)
ctx.translateBy(x: 0, y: H); ctx.scaleBy(x: 1, y: -1)   // y down, points

// MARK: text
func systemFont(_ size: CGFloat, weight: CGFloat, tracking: CGFloat = 0) -> CTFont {
    // weight on the CSS scale (400…700); SF's variable 'wght' axis makes 650 exact.
    let base = CTFontCreateUIFontForLanguage(.system, size, nil)!
    let wght = 0x77676874
    let desc = CTFontDescriptorCreateWithAttributes([kCTFontVariationAttribute: [NSNumber(value: wght): weight]] as CFDictionary)
    return CTFontCreateCopyWithAttributes(base, size, nil, desc)
}
@discardableResult
func text(_ s: String, x: CGFloat, y: CGFloat, size: CGFloat, weight: CGFloat = 400, color: CGColor, tracking: CGFloat = 0, align: String = "left", mono: Bool = false) -> CGFloat {
    let font: CTFont = mono ? (NSFont.monospacedSystemFont(ofSize: size, weight: .regular) as CTFont) : systemFont(size, weight: weight)
    let attrs: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: color, .kern: tracking * size]
    let line = CTLineCreateWithAttributedString(NSAttributedString(string: s, attributes: attrs))
    let width = CGFloat(CTLineGetTypographicBounds(line, nil, nil, nil)) - tracking * size
    var px = x
    if align == "center" { px = x - width / 2 } else if align == "right" { px = x - width }
    ctx.saveGState()
    ctx.textMatrix = CGAffineTransform(scaleX: 1, y: -1)
    ctx.textPosition = CGPoint(x: px, y: y)
    CTLineDraw(line, ctx)
    ctx.restoreGState()
    return width
}

// MARK: shapes
func roundedRect(_ r: CGRect, _ radius: CGFloat) -> CGPath { CGPath(roundedRect: r, cornerWidth: radius, cornerHeight: radius, transform: nil) }
func fill(_ path: CGPath, _ color: CGColor) { ctx.addPath(path); ctx.setFillColor(color); ctx.fillPath() }
func stroke(_ path: CGPath, _ color: CGColor, _ width: CGFloat) { ctx.addPath(path); ctx.setStrokeColor(color); ctx.setLineWidth(width); ctx.strokePath() }
func circle(_ c: CGPoint, _ r: CGFloat) -> CGPath { CGPath(ellipseIn: CGRect(x: c.x - r, y: c.y - r, width: 2 * r, height: 2 * r), transform: nil) }

/// The mark of aether-mark.svg: 32-unit box scaled to `box` at `origin` (top-left).
func mark(at origin: CGPoint, box: CGFloat, color: CGColor) {
    let u = box / 32
    let c = CGPoint(x: origin.x + 16 * u, y: origin.y + 16 * u)
    let b = CGPoint(x: origin.x + (16 + 10.4 * cos(.pi / 4)) * u, y: origin.y + (16 - 10.4 * sin(.pi / 4)) * u)
    stroke(circle(c, 10.4 * u), color, 3 * u)
    fill(circle(b, 4 * u), color)
}

func image(_ url: URL) -> CGImage {
    let src = CGImageSourceCreateWithURL(url as CFURL, nil)!
    return CGImageSourceCreateImageAtIndex(src, 0, nil)!
}
/// Draws `img` into `rect` (top-left coordinates) clipped to the iOS icon shape (continuous corners approximated).
func icon(_ img: CGImage, in rect: CGRect, radius: CGFloat? = nil) {
    ctx.saveGState()
    ctx.addPath(roundedRect(rect, radius ?? rect.width * 0.2237))
    ctx.clip()
    ctx.translateBy(x: rect.minX, y: rect.maxY); ctx.scaleBy(x: 1, y: -1)
    ctx.draw(img, in: CGRect(x: 0, y: 0, width: rect.width, height: rect.height))
    ctx.restoreGState()
}

// MARK: sheet
let ink = hex(light["text"]!), muted = hex(light["textMuted"]!), paper = hex(light["bg"]!)
let dInk = hex(dark["text"]!), dMuted = hex(dark["textMuted"]!)
ctx.setFillColor(paper); ctx.fill(CGRect(x: 0, y: 0, width: W, height: H))

// Grid: left column 56…520, middle column 576…1040, right column 1084…1384.
let L: CGFloat = 56, M: CGFloat = 576, MW: CGFloat = 464, R: CGFloat = 1084

// Header: lockup and a caption.
mark(at: CGPoint(x: L, y: 44), box: 56, color: ink)
text("Aether", x: L + 56 + 14, y: 44 + 28 + 14.5, size: 41, weight: 650, color: ink, tracking: -0.035)
text("Brand sheet · 26.10.2026 · system font (SF), wordmark weight 650, tracking −0.035 em · tokens: brand/colors.json", x: L, y: 128, size: 12.5, color: muted)
text("Aether (dsh-codex) · your phone, the agent on your Mac and your servers", x: W - L, y: 128, size: 12.5, color: muted, align: "right")

// Left column: app icon at three sizes, plus dark and tinted.
let iconDir = brandDir.appendingPathComponent("ios/AppIcon.appiconset")
let iconLight = image(iconDir.appendingPathComponent("AppIcon.png"))
let iconDark = image(iconDir.appendingPathComponent("AppIcon-dark.png"))
let iconTinted = image(iconDir.appendingPathComponent("AppIcon-tinted.png"))
text("App icon", x: L, y: 176, size: 13, weight: 600, color: ink)
icon(iconLight, in: CGRect(x: L, y: 192, width: 256, height: 256))
text("1024 px, shown at 25 %", x: L, y: 470, size: 11.5, color: muted)
icon(iconLight, in: CGRect(x: 336, y: 192, width: 90, height: 90))
text("180 px (1:1)", x: 336, y: 300, size: 11.5, color: muted)
icon(iconLight, in: CGRect(x: 336, y: 318, width: 30, height: 30))
text("60 px (1:1)", x: 336, y: 366, size: 11.5, color: muted)
icon(iconDark, in: CGRect(x: 336, y: 392, width: 60, height: 60))
text("dark", x: 336, y: 470, size: 11.5, color: muted)
icon(iconTinted, in: CGRect(x: 410, y: 392, width: 60, height: 60))
text("tinted", x: 410, y: 470, size: 11.5, color: muted)
text("iOS masks the corners; the Mac icon is the same composition on the 824 pt body.", x: L, y: 496, size: 11.5, color: muted)

// Mark and construction.
text("Mark", x: L, y: 548, size: 13, weight: 600, color: ink)
mark(at: CGPoint(x: L, y: 566), box: 128, color: ink)
mark(at: CGPoint(x: 204, y: 566 + 128 - 48), box: 48, color: ink)
mark(at: CGPoint(x: 268, y: 566 + 128 - 32), box: 32, color: ink)
mark(at: CGPoint(x: 316, y: 566 + 128 - 16), box: 16, color: ink)
text("128 · 48 · 32 · 16 px", x: 204, y: 566 + 128 + 18, size: 11.5, color: muted)
let cBox = CGRect(x: 380, y: 556, width: 150, height: 150)
let cu = cBox.width / 32
let cc = CGPoint(x: cBox.minX + 16 * cu, y: cBox.minY + 16 * cu)
stroke(roundedRect(cBox, 0), hex(light["border"]!), 1)
for r in [11.9, 10.4, 8.9] as [CGFloat] { stroke(circle(cc, r * cu), hex(light["accent"]!, 0.35), 0.75) }
ctx.setLineDash(phase: 0, lengths: [3, 3])
stroke(CGPath(rect: CGRect(x: cc.x, y: cBox.minY, width: 0, height: cBox.height), transform: nil), hex(light["accent"]!, 0.35), 0.75)
stroke(CGPath(rect: CGRect(x: cBox.minX, y: cc.y, width: cBox.width, height: 0), transform: nil), hex(light["accent"]!, 0.35), 0.75)
let d = CGMutablePath(); d.move(to: cc); d.addLine(to: CGPoint(x: cBox.minX + (16 + 10.4 * cos(.pi / 4)) * cu, y: cBox.minY + (16 - 10.4 * sin(.pi / 4)) * cu))
stroke(d, hex(light["accent"]!, 0.6), 0.75)
ctx.setLineDash(phase: 0, lengths: [])
mark(at: cBox.origin, box: cBox.width, color: hex(light["text"]!, 0.9))
text("32-unit grid · ring r 10.4, stroke 3 · point r 4 on the ring at 45°", x: L, y: 728, size: 11.5, color: muted)
text("one path, nonzero fill · the glow appears only on the app icon", x: L, y: 745, size: 11.5, color: muted)

// Wordmark and lockup.
text("Wordmark and lockup", x: L, y: 786, size: 13, weight: 600, color: ink)
text("Aether", x: L, y: 786 + 20 + 62, size: 88, weight: 650, color: ink, tracking: -0.035)
mark(at: CGPoint(x: 380, y: 806), box: 44, color: ink)
text("Aether", x: 380 + 44 + 11, y: 806 + 22 + 11.4, size: 32.2, weight: 650, color: ink, tracking: -0.035)
mark(at: CGPoint(x: 380, y: 866), box: 24, color: ink)
text("Aether", x: 380 + 24 + 6, y: 866 + 12 + 6.2, size: 17.6, weight: 650, color: ink, tracking: -0.035)
text("mark = 1.02 cap heights, centred on the cap mid-line · gap 0.4 cap heights", x: L, y: 912, size: 11.5, color: muted)
text("UI copy: system font, regular/semibold; German informal “du”, no exclamation marks", x: L, y: 930, size: 11.5, color: muted)

// Middle column: palette in two rows per theme.
let rows = [["accent", "accentSoft", "online", "warning", "danger"], ["bg", "surface", "surfaceRaised", "border", "text", "textMuted"]]
let sw: CGFloat = 70, sh: CGFloat = 34, gap: CGFloat = 6, rowH: CGFloat = 62
func palette(_ t: [String: String], y0: CGFloat, labelColor: CGColor, hexColor: CGColor, lineColor: CGColor) {
    for (ri, row) in rows.enumerated() {
        for (i, name) in row.enumerated() {
            let x = M + CGFloat(i) * (sw + gap), y = y0 + CGFloat(ri) * rowH
            fill(roundedRect(CGRect(x: x, y: y, width: sw, height: sh), 7), hex(t[name]!))
            stroke(roundedRect(CGRect(x: x, y: y, width: sw, height: sh), 7), lineColor, 0.75)
            text(name, x: x, y: y + sh + 13, size: 9, weight: 500, color: labelColor)
            text(t[name]!, x: x, y: y + sh + 25, size: 8.5, color: hexColor, mono: true)
        }
    }
}
text("Palette", x: M, y: 176, size: 13, weight: 600, color: ink)
palette(light, y0: 192, labelColor: ink, hexColor: muted, lineColor: hex(light["border"]!))
let darkPanel = CGRect(x: M - 10, y: 318, width: MW + 20, height: 2 * rowH + 12)
fill(roundedRect(darkPanel, 10), hex(dark["bg"]!))
palette(dark, y0: 330, labelColor: dInk, hexColor: dMuted, lineColor: hex(dark["border"]!))

func fmt(_ v: CGFloat) -> String { String(format: "%.1f:1", v) }
func lowest(_ t: [String: String], _ fg: String) -> CGFloat { min(contrast(t[fg]!, t["bg"]!), contrast(t[fg]!, t["surface"]!), contrast(t[fg]!, t["surfaceRaised"]!)) }
let cy0: CGFloat = 486
text("Contrast (WCAG), lowest of bg / surface / surfaceRaised", x: M, y: cy0, size: 11.5, weight: 600, color: ink)
text("light   text \(fmt(lowest(light, "text")))  ·  textMuted \(fmt(lowest(light, "textMuted")))  ·  accent \(fmt(lowest(light, "accent")))  ·  white on accent \(fmt(contrast("#FFFFFF", light["accent"]!)))", x: M, y: cy0 + 18, size: 11, color: muted)
text("          danger \(fmt(lowest(light, "danger")))  ·  online \(fmt(lowest(light, "online")))  ·  warning \(fmt(lowest(light, "warning")))", x: M, y: cy0 + 34, size: 11, color: muted)
text("dark    text \(fmt(lowest(dark, "text")))  ·  textMuted \(fmt(lowest(dark, "textMuted")))  ·  accent \(fmt(lowest(dark, "accent")))  ·  bg on accent \(fmt(contrast(dark["bg"]!, dark["accent"]!)))", x: M, y: cy0 + 52, size: 11, color: muted)
text("          danger \(fmt(lowest(dark, "danger")))  ·  online \(fmt(lowest(dark, "online")))  ·  warning \(fmt(lowest(dark, "warning")))", x: M, y: cy0 + 68, size: 11, color: muted)
text("Text roles and accent ≥ 4.5:1 on every surface; status colours ≥ 3:1 (dots, chips);", x: M, y: cy0 + 88, size: 11, color: muted)
text("danger also as text. Text on accent: white in light, bg in dark.", x: M, y: cy0 + 104, size: 11, color: muted)

// Dark panel: lockup on dark, the accent as glow/link colour, voice.
let dm = CGRect(x: M, y: 612, width: MW, height: 108)
fill(roundedRect(dm, 12), hex(dark["bg"]!))
mark(at: CGPoint(x: dm.minX + 20, y: dm.minY + 20), box: 36, color: dInk)
text("Aether", x: dm.minX + 20 + 36 + 9, y: dm.minY + 20 + 18 + 9.3, size: 26.4, weight: 650, color: dInk, tracking: -0.035)
mark(at: CGPoint(x: dm.minX + 20, y: dm.minY + 72), box: 16, color: dInk)
mark(at: CGPoint(x: dm.minX + 44, y: dm.minY + 64), box: 24, color: hex(dark["accent"]!))
text("on dark: ink for the mark; accent for links, buttons and the icon's glow", x: dm.minX + 80, y: dm.minY + 82, size: 11, color: dMuted)
text("Voice: calm, precise, friendly. Short sentences, no exclamation marks.", x: dm.minX + 20, y: dm.minY + 100 - 2, size: 11, color: dMuted)

// Light card: a settings-like card in the brand tokens.
let lm = CGRect(x: M, y: 736, width: MW, height: 120)
fill(roundedRect(lm, 12), hex(light["surface"]!)); stroke(roundedRect(lm, 12), hex(light["border"]!), 1)
text("Für die Aether-App erreichbar", x: lm.minX + 16, y: lm.minY + 28, size: 13, weight: 600, color: ink)
text("Port 7443 · LAN, Tailscale · Fingerabdruck 9f86…0a08", x: lm.minX + 16, y: lm.minY + 46, size: 11.5, color: muted)
fill(roundedRect(CGRect(x: lm.maxX - 16 - 44, y: lm.minY + 16, width: 44, height: 26), 13), hex(light["accent"]!))
fill(circle(CGPoint(x: lm.maxX - 16 - 13, y: lm.minY + 29), 11), hex("#FFFFFF"))
fill(roundedRect(CGRect(x: lm.minX + 16, y: lm.minY + 64, width: 150, height: 34), 9), hex(light["accent"]!))
text("Link kopieren", x: lm.minX + 16 + 75, y: lm.minY + 64 + 22, size: 13, weight: 600, color: hex("#FFFFFF"), align: "center")
fill(roundedRect(CGRect(x: lm.minX + 176, y: lm.minY + 64, width: 130, height: 34), 9), hex(light["surfaceRaised"]!))
text("Neu erzeugen", x: lm.minX + 176 + 65, y: lm.minY + 64 + 22, size: 13, weight: 600, color: ink, align: "center")
text("Der Link enthält ein Geheimnis und gilt, bis du ihn widerrufst.", x: lm.minX + 16, y: lm.minY + 112, size: 10, color: muted)
text("web UI card in the brand tokens (the live web UI keeps its own neutral tokens)", x: M, y: 876, size: 11.5, color: muted)

// Right column: mocked iPhone, dark appearance, the Geräte screen.
let phone = CGRect(x: R, y: 176, width: 300, height: 640)
ctx.saveGState()
ctx.setShadow(offset: CGSize(width: 0, height: 10), blur: 30, color: hex("#000000", 0.18))
fill(roundedRect(phone, 46), hex("#101117"))
ctx.restoreGState()
stroke(roundedRect(phone.insetBy(dx: 0.5, dy: 0.5), 46), hex("#2A2E40"), 1)
ctx.saveGState()
ctx.addPath(roundedRect(phone.insetBy(dx: 10, dy: 10), 38)); ctx.clip()
fill(CGPath(rect: phone, transform: nil), hex(dark["bg"]!))
fill(roundedRect(CGRect(x: phone.midX - 46, y: phone.minY + 22, width: 92, height: 26), 13), hex("#000000"))
text("9:41", x: phone.minX + 42, y: phone.minY + 40, size: 13, weight: 600, color: dInk)
text("Geräte", x: phone.minX + 28, y: phone.minY + 106, size: 30, weight: 700, color: dInk, tracking: -0.02)
fill(circle(CGPoint(x: phone.maxX - 42, y: phone.minY + 96), 15), hex(dark["accentSoft"]!))
let plus = CGMutablePath(); plus.move(to: CGPoint(x: phone.maxX - 48, y: phone.minY + 96)); plus.addLine(to: CGPoint(x: phone.maxX - 36, y: phone.minY + 96)); plus.move(to: CGPoint(x: phone.maxX - 42, y: phone.minY + 90)); plus.addLine(to: CGPoint(x: phone.maxX - 42, y: phone.minY + 102))
stroke(plus, hex(dark["accent"]!), 2)
struct Device { let name: String; let status: String; let dot: String; let transport: String; let meta: String; let unread: String? }
let devices = [
    Device(name: "MacBook Pro", status: "Bereit", dot: "online", transport: "https · WLAN", meta: "2 laufen", unread: "1"),
    Device(name: "VPS 2", status: "Wartet auf Freigabe", dot: "warning", transport: "ssh", meta: "1 läuft", unread: nil),
    Device(name: "VPS 1", status: "Offline", dot: "border", transport: "https · Tailscale", meta: "vor 3 h", unread: nil),
]
var y = phone.minY + 128
for dev in devices {
    let card = CGRect(x: phone.minX + 20, y: y, width: phone.width - 40, height: 92)
    fill(roundedRect(card, 16), hex(dark["surface"]!))
    stroke(roundedRect(card, 16), hex(dark["border"]!), 1)
    fill(roundedRect(CGRect(x: card.minX + 14, y: card.minY + 16, width: 40, height: 40), 11), hex(dark["surfaceRaised"]!))
    mark(at: CGPoint(x: card.minX + 22, y: card.minY + 24), box: 24, color: hex(dark["accent"]!))
    text(dev.name, x: card.minX + 66, y: card.minY + 30, size: 15, weight: 600, color: dInk)
    if let n = dev.unread {
        fill(circle(CGPoint(x: card.maxX - 24, y: card.minY + 25), 9), hex(dark["accent"]!))
        text(n, x: card.maxX - 24, y: card.minY + 29, size: 11, weight: 600, color: hex(dark["bg"]!), align: "center")
    }
    fill(circle(CGPoint(x: card.minX + 71, y: card.minY + 47), 4), hex(dark[dev.dot]!))
    text(dev.status, x: card.minX + 80, y: card.minY + 51, size: 12, color: dMuted)
    let chipW = text(dev.transport, x: card.minX + 66 + 8, y: card.minY + 75, size: 10, weight: 500, color: hex(dark["accent"]!))
    fill(roundedRect(CGRect(x: card.minX + 66, y: card.minY + 62, width: chipW + 16, height: 18), 9), hex(dark["accentSoft"]!))
    text(dev.transport, x: card.minX + 66 + 8, y: card.minY + 75, size: 10, weight: 500, color: hex(dark["accent"]!))
    text(dev.meta, x: card.maxX - 14, y: card.minY + 75, size: 10, color: dMuted, align: "right")
    y += 104
}
let chat = CGRect(x: phone.minX + 20, y: y + 8, width: phone.width - 40, height: 118)
text("Zuletzt", x: chat.minX, y: chat.minY + 2, size: 12, weight: 600, color: dMuted)
fill(roundedRect(CGRect(x: chat.minX, y: chat.minY + 12, width: chat.width, height: 104), 16), hex(dark["surface"]!))
stroke(roundedRect(CGRect(x: chat.minX, y: chat.minY + 12, width: chat.width, height: 104), 16), hex(dark["border"]!), 1)
text("Add --top flag to tally", x: chat.minX + 14, y: chat.minY + 36, size: 13.5, weight: 600, color: dInk)
text("MacBook Pro · tally", x: chat.minX + 14, y: chat.minY + 54, size: 11, color: dMuted)
fill(roundedRect(CGRect(x: chat.minX + 14, y: chat.minY + 66, width: chat.width - 28, height: 36), 10), hex(dark["surfaceRaised"]!))
fill(circle(CGPoint(x: chat.minX + 30, y: chat.minY + 84), 4), hex(dark["online"]!))
text("Läuft · Edited tally.py · Ran pytest", x: chat.minX + 42, y: chat.minY + 88, size: 11.5, color: dInk)
let tabY = phone.maxY - 78
stroke(CGPath(rect: CGRect(x: phone.minX + 10, y: tabY, width: phone.width - 20, height: 0), transform: nil), hex(dark["border"]!), 1)
for (i, label) in ["Geräte", "Chats", "Einstellungen"].enumerated() {
    let x = phone.minX + 62 + CGFloat(i) * 88
    let on = i == 0
    fill(circle(CGPoint(x: x, y: tabY + 18), 7), on ? hex(dark["accent"]!) : hex(dark["textMuted"]!, 0.5))
    text(label, x: x, y: tabY + 41, size: 9.5, weight: 500, color: on ? hex(dark["accent"]!) : dMuted, align: "center")
}
fill(roundedRect(CGRect(x: phone.midX - 54, y: phone.maxY - 18, width: 108, height: 4), 2), hex(dark["text"]!, 0.9))
ctx.restoreGState()
text("iPhone · Geräte (mock, dark appearance)", x: phone.minX, y: phone.maxY + 24, size: 11.5, color: muted)
let lcard = CGRect(x: phone.minX, y: phone.maxY + 44, width: 300, height: 92)
fill(roundedRect(lcard, 16), hex(light["surface"]!)); stroke(roundedRect(lcard, 16), hex(light["border"]!), 1)
fill(roundedRect(CGRect(x: lcard.minX + 14, y: lcard.minY + 16, width: 40, height: 40), 11), hex(light["surfaceRaised"]!))
mark(at: CGPoint(x: lcard.minX + 22, y: lcard.minY + 24), box: 24, color: hex(light["accent"]!))
text("MacBook Pro", x: lcard.minX + 66, y: lcard.minY + 30, size: 15, weight: 600, color: ink)
fill(circle(CGPoint(x: lcard.maxX - 24, y: lcard.minY + 25), 9), hex(light["accent"]!))
text("1", x: lcard.maxX - 24, y: lcard.minY + 29, size: 11, weight: 600, color: hex("#FFFFFF"), align: "center")
fill(circle(CGPoint(x: lcard.minX + 71, y: lcard.minY + 47), 4), hex(light["online"]!))
text("Bereit", x: lcard.minX + 80, y: lcard.minY + 51, size: 12, color: muted)
let cw = text("https · WLAN", x: lcard.minX + 74, y: lcard.minY + 75, size: 10, weight: 500, color: hex(light["accent"]!))
fill(roundedRect(CGRect(x: lcard.minX + 66, y: lcard.minY + 62, width: cw + 16, height: 18), 9), hex(light["accentSoft"]!))
text("https · WLAN", x: lcard.minX + 74, y: lcard.minY + 75, size: 10, weight: 500, color: hex(light["accent"]!))
text("2 laufen", x: lcard.maxX - 14, y: lcard.minY + 75, size: 10, color: muted, align: "right")
text("the same card, light appearance", x: lcard.minX, y: lcard.maxY + 18, size: 11.5, color: muted)

// MARK: write
let img = ctx.makeImage()!
let outURL = brandDir.appendingPathComponent("preview.png")
let dest = CGImageDestinationCreateWithURL(outURL as CFURL, "public.png" as CFString, 1, nil)!
CGImageDestinationAddImage(dest, img, [kCGImagePropertyDPIWidth: 144, kCGImagePropertyDPIHeight: 144] as CFDictionary)
guard CGImageDestinationFinalize(dest) else { fatalError("write failed") }
print("wrote \(outURL.path) (\(img.width)×\(img.height))")
