// Renders the app icon (the ui/favicon.svg prompt mark on the macOS icon grid)
// into an .iconset directory for iconutil.
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
    ctx.setFillColor(NSColor(srgbRed: 0.10, green: 0.11, blue: 0.12, alpha: 1).cgColor)
    ctx.fillPath()
    ctx.restoreGState()

    ctx.saveGState()
    ctx.addPath(shape)
    ctx.clip()
    let gradient = CGGradient(
        colorsSpace: CGColorSpaceCreateDeviceRGB(),
        colors: [NSColor(srgbRed: 0.20, green: 0.21, blue: 0.23, alpha: 1).cgColor,
                 NSColor(srgbRed: 0.08, green: 0.085, blue: 0.095, alpha: 1).cgColor] as CFArray,
        locations: [0, 1])!
    ctx.drawLinearGradient(gradient, start: CGPoint(x: 512, y: 924), end: CGPoint(x: 512, y: 100), options: [])
    // Hairline highlight along the top edge.
    ctx.addPath(CGPath(roundedRect: body.insetBy(dx: 3, dy: 3), cornerWidth: 183, cornerHeight: 183, transform: nil))
    ctx.setStrokeColor(NSColor.white.withAlphaComponent(0.10).cgColor)
    ctx.setLineWidth(6)
    ctx.strokePath()
    ctx.restoreGState()

    // favicon.svg: viewBox 36, rect 2…34. Map its 32 units onto the 824 pt body (y flipped).
    let unit: CGFloat = 824 / 32
    func p(_ x: CGFloat, _ y: CGFloat) -> CGPoint { CGPoint(x: 100 + (x - 2) * unit, y: 924 - (y - 2) * unit) }
    ctx.setStrokeColor(NSColor.white.cgColor)
    ctx.setLineWidth(2.6 * unit)
    ctx.setLineCap(.round)
    ctx.setLineJoin(.round)
    ctx.move(to: p(11, 13.5))
    ctx.addLine(to: p(16, 18))
    ctx.addLine(to: p(11, 22.5))
    ctx.move(to: p(19.5, 23))
    ctx.addLine(to: p(25.5, 23))
    ctx.strokePath()

    NSGraphicsContext.restoreGraphicsState()
    return rep.representation(using: .png, properties: [:])!
}

for size in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let name = scale == 1 ? "icon_\(size)x\(size).png" : "icon_\(size)x\(size)@2x.png"
        try! render(size * scale).write(to: out.appendingPathComponent(name))
    }
}
