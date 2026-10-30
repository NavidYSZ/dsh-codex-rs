// Renders the macOS app icon (the Aether mark on the macOS icon grid) into an
// .iconset directory for iconutil. Same composition as the iOS default icon
// (brand/tools/make-ios-icon.swift): the 32-unit mark of brand/aether-mark.svg
// on the accent gradient, with the soft glow inside the ring.
//
//   make-icon <out.iconset>
import AppKit

let out = URL(fileURLWithPath: CommandLine.arguments.dropFirst().first ?? "AppIcon.iconset")
try? FileManager.default.createDirectory(at: out, withIntermediateDirectories: true)

func render(_ pixels: Int) -> Data {
    let rep = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: pixels, pixelsHigh: pixels, bitsPerSample: 8,
        samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
        bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    let ctx = NSGraphicsContext.current!.cgContext
    let s = CGFloat(pixels) / 1024
    ctx.scaleBy(x: s, y: s)

    // 824 pt body with the standard 100 pt margin and a soft shadow.
    let body = CGRect(x: 100, y: 100, width: 824, height: 824)
    let shape = CGPath(roundedRect: body, cornerWidth: 186, cornerHeight: 186, transform: nil)
    ctx.saveGState()
    ctx.setShadow(offset: CGSize(width: 0, height: -10), blur: 24, color: NSColor.black.withAlphaComponent(0.35).cgColor)
    ctx.addPath(shape)
    ctx.setFillColor(NSColor(srgbRed: 0.26, green: 0.38, blue: 0.94, alpha: 1).cgColor)
    ctx.fillPath()
    ctx.restoreGState()

    ctx.saveGState()
    ctx.addPath(shape)
    ctx.clip()
    // Accent gradient, top #5F80FA → bottom #3454E4 (brand/colors.json accent #3E62F0 in the middle).
    let gradient = CGGradient(
        colorsSpace: CGColorSpaceCreateDeviceRGB(),
        colors: [NSColor(srgbRed: 0x5F / 255, green: 0x80 / 255, blue: 0xFA / 255, alpha: 1).cgColor,
                 NSColor(srgbRed: 0x34 / 255, green: 0x54 / 255, blue: 0xE4 / 255, alpha: 1).cgColor] as CFArray,
        locations: [0, 1])!
    ctx.drawLinearGradient(gradient, start: CGPoint(x: 512, y: 924), end: CGPoint(x: 512, y: 100), options: [])
    // Hairline highlight along the top edge.
    ctx.addPath(CGPath(roundedRect: body.insetBy(dx: 3, dy: 3), cornerWidth: 183, cornerHeight: 183, transform: nil))
    ctx.setStrokeColor(NSColor.white.withAlphaComponent(0.14).cgColor)
    ctx.setLineWidth(6)
    ctx.strokePath()
    ctx.restoreGState()

    // The mark: aether-mark.svg's 32-unit box (y down) on a 612 pt box centred in the body
    // (the same 760/1024 proportion as the iOS icon): ring r 10.4, stroke 3, bead r 4 at 45°.
    let box: CGFloat = 824 * 760 / 1024
    let unit = box / 32
    let origin = 100 + (824 - box) / 2
    func p(_ x: CGFloat, _ y: CGFloat) -> CGPoint { CGPoint(x: origin + x * unit, y: 924 - (origin - 100) - y * unit) }
    let center = p(16, 16)
    let bead = p(16 + 10.4 * cos(.pi / 4), 16 - 10.4 * sin(.pi / 4))
    let white = NSColor.white
    let glow = CGGradient(colorsSpace: CGColorSpaceCreateDeviceRGB(),
                          colors: [white.withAlphaComponent(0.22).cgColor, white.withAlphaComponent(0).cgColor] as CFArray,
                          locations: [0, 1])!
    ctx.drawRadialGradient(glow, startCenter: center, startRadius: 0, endCenter: center, endRadius: (10.4 - 1.5) * unit, options: [])
    let halo = CGGradient(colorsSpace: CGColorSpaceCreateDeviceRGB(),
                          colors: [white.withAlphaComponent(0.28).cgColor, white.withAlphaComponent(0).cgColor] as CFArray,
                          locations: [0, 1])!
    ctx.drawRadialGradient(halo, startCenter: bead, startRadius: 4 * unit, endCenter: bead, endRadius: 4 * 2.6 * unit, options: [])
    ctx.setStrokeColor(white.cgColor)
    ctx.setLineWidth(3 * unit)
    ctx.addArc(center: center, radius: 10.4 * unit, startAngle: 0, endAngle: 2 * .pi, clockwise: false)
    ctx.strokePath()
    ctx.setFillColor(white.cgColor)
    ctx.addArc(center: bead, radius: 4 * unit, startAngle: 0, endAngle: 2 * .pi, clockwise: false)
    ctx.fillPath()

    NSGraphicsContext.restoreGraphicsState()
    return rep.representation(using: .png, properties: [:])!
}

for size in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let name = scale == 1 ? "icon_\(size)x\(size).png" : "icon_\(size)x\(size)@2x.png"
        try! render(size * scale).write(to: out.appendingPathComponent(name))
    }
}
