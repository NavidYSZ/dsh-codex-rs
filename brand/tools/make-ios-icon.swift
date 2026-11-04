// Renders the Aether iOS app icon (1024 px, square corners: iOS masks them)
// in the light, dark and tinted appearances, and the launch-screen mark at
// @1x/@2x/@3x, pixel-exact with CoreGraphics. Geometry is the 32-unit mark
// of brand/aether-mark.svg; colours are brand/colors.json.
//
//   xcrun swiftc -O brand/tools/make-ios-icon.swift -o /tmp/make-ios-icon
//   /tmp/make-ios-icon brand/ios            # writes AppIcon.appiconset/*.png and LaunchMark*.png
import CoreGraphics
import Foundation
import ImageIO

let outDir = URL(fileURLWithPath: CommandLine.arguments.dropFirst().first ?? "brand/ios")
let iconSet = outDir.appendingPathComponent("AppIcon.appiconset")
try? FileManager.default.createDirectory(at: iconSet, withIntermediateDirectories: true)

let srgb = CGColorSpace(name: CGColorSpace.sRGB)!
func rgb(_ hex: UInt32, _ alpha: CGFloat = 1) -> CGColor {
    CGColor(colorSpace: srgb, components: [
        CGFloat((hex >> 16) & 0xff) / 255, CGFloat((hex >> 8) & 0xff) / 255, CGFloat(hex & 0xff) / 255, alpha])!
}

// Mark geometry in the 32-unit box of aether-mark.svg (y down).
let ringCenter = CGPoint(x: 16, y: 16)
let ringRadius: CGFloat = 10.4      // centre line of the ring
let ringWidth: CGFloat = 3          // stroke
let beadRadius: CGFloat = 4
let beadCenter = CGPoint(x: 16 + 10.4 * cos(.pi / 4), y: 16 - 10.4 * sin(.pi / 4))   // 45°, on the ring

struct Look {
    var background: [CGColor]       // top → bottom
    var ink: CGColor                // ring and bead
    var glow: CGColor?              // inside the ring, fades to clear at the inner edge
    var halo: CGColor?              // around the bead
}

/// Draws the mark into `ctx` (top-left origin), its 32-unit box scaled to `box` pixels centred in `size`.
func drawMark(_ ctx: CGContext, size: CGFloat, box: CGFloat, look: Look) {
    let unit = box / 32
    let origin = (size - box) / 2
    func p(_ pt: CGPoint) -> CGPoint { CGPoint(x: origin + pt.x * unit, y: origin + pt.y * unit) }
    let c = p(ringCenter), b = p(beadCenter)
    if let glow = look.glow {
        let g = CGGradient(colorsSpace: srgb, colors: [glow, rgb(0x000000, 0)] as CFArray, locations: [0, 1])!
        ctx.drawRadialGradient(g, startCenter: c, startRadius: 0, endCenter: c,
                               endRadius: (ringRadius - ringWidth / 2) * unit, options: [])
    }
    if let halo = look.halo {
        let g = CGGradient(colorsSpace: srgb, colors: [halo, rgb(0x000000, 0)] as CFArray, locations: [0, 1])!
        ctx.drawRadialGradient(g, startCenter: b, startRadius: beadRadius * unit, endCenter: b,
                               endRadius: beadRadius * 2.6 * unit, options: [])
    }
    ctx.setStrokeColor(look.ink)
    ctx.setLineWidth(ringWidth * unit)
    ctx.addArc(center: c, radius: ringRadius * unit, startAngle: 0, endAngle: 2 * .pi, clockwise: false)
    ctx.strokePath()
    ctx.setFillColor(look.ink)
    ctx.addArc(center: b, radius: beadRadius * unit, startAngle: 0, endAngle: 2 * .pi, clockwise: false)
    ctx.fillPath()
}

func context(_ size: Int, opaque: Bool) -> CGContext {
    let info = opaque ? CGImageAlphaInfo.noneSkipLast.rawValue : CGImageAlphaInfo.premultipliedLast.rawValue
    let ctx = CGContext(data: nil, width: size, height: size, bitsPerComponent: 8, bytesPerRow: 0,
                        space: srgb, bitmapInfo: info)!
    ctx.setAllowsAntialiasing(true)
    ctx.setShouldAntialias(true)
    ctx.interpolationQuality = .high
    // SVG coordinates: origin top-left, y down.
    ctx.translateBy(x: 0, y: CGFloat(size))
    ctx.scaleBy(x: 1, y: -1)
    return ctx
}

func write(_ ctx: CGContext, to url: URL) {
    let image = ctx.makeImage()!
    let dest = CGImageDestinationCreateWithURL(url as CFURL, "public.png" as CFString, 1, nil)!
    CGImageDestinationAddImage(dest, image, nil)
    guard CGImageDestinationFinalize(dest) else { fatalError("could not write \(url.path)") }
    print("wrote \(url.path) (\(image.width)×\(image.height))")
}

/// App icon: full-bleed background gradient and the mark on a 760 px box (ring ≈ 55 % of the width).
func icon(_ name: String, _ look: Look) {
    let size = 1024
    let ctx = context(size, opaque: true)
    let g = CGGradient(colorsSpace: srgb, colors: look.background as CFArray, locations: [0, 1])!
    ctx.drawLinearGradient(g, start: CGPoint(x: 512, y: 0), end: CGPoint(x: 512, y: 1024), options: [])
    drawMark(ctx, size: 1024, box: 760, look: look)
    write(ctx, to: iconSet.appendingPathComponent(name))
}

// colors.json: accent #3E62F0 / #8AA2FF, bg dark #0E0F14, text #121420 / #F2F3F8.
icon("AppIcon.png", Look(background: [rgb(0x5F80FA), rgb(0x3454E4)], ink: rgb(0xFFFFFF),
                         glow: rgb(0xFFFFFF, 0.22), halo: rgb(0xFFFFFF, 0.28)))
icon("AppIcon-dark.png", Look(background: [rgb(0x1B2040), rgb(0x0E0F14)], ink: rgb(0x8AA2FF),
                              glow: rgb(0x5B7CFA, 0.42), halo: rgb(0x8AA2FF, 0.30)))
// Tinted: grayscale; iOS maps luminance to the user's tint on its own dark gradient.
icon("AppIcon-tinted.png", Look(background: [rgb(0x1C1C1E), rgb(0x000000)], ink: rgb(0xFFFFFF),
                                glow: rgb(0xFFFFFF, 0.20), halo: rgb(0xFFFFFF, 0.24)))

let contents = """
{
  "images" : [
    { "filename" : "AppIcon.png", "idiom" : "universal", "platform" : "ios", "size" : "1024x1024" },
    { "appearances" : [ { "appearance" : "luminosity", "value" : "dark" } ],
      "filename" : "AppIcon-dark.png", "idiom" : "universal", "platform" : "ios", "size" : "1024x1024" },
    { "appearances" : [ { "appearance" : "luminosity", "value" : "tinted" } ],
      "filename" : "AppIcon-tinted.png", "idiom" : "universal", "platform" : "ios", "size" : "1024x1024" }
  ],
  "info" : { "author" : "xcode", "version" : 1 }
}

"""
try! contents.write(to: iconSet.appendingPathComponent("Contents.json"), atomically: true, encoding: .utf8)

// Launch-screen mark: 120 pt box, transparent, ink for the light appearance and light ink for dark.
for (suffix, ink) in [("", 0x121420 as UInt32), ("-dark", 0xF2F3F8)] {
    for scale in 1...3 {
        let size = 120 * scale
        let ctx = context(size, opaque: false)
        drawMark(ctx, size: CGFloat(size), box: CGFloat(size), look: Look(background: [], ink: rgb(ink), glow: nil, halo: nil))
        let name = scale == 1 ? "LaunchMark\(suffix).png" : "LaunchMark\(suffix)@\(scale)x.png"
        write(ctx, to: outDir.appendingPathComponent(name))
    }
}
