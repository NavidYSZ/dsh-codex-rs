#!/usr/bin/env swift
// Placeholder app icon until the brand package lands (SPEC §6: brand/ios/AppIcon.appiconset replaces the files
// in Aether/Resources/Assets.xcassets/AppIcon.appiconset by a plain copy).
//
// Renders the provisional Aether mark with CoreGraphics: a soft accent gradient, a ring of "upper air" and a
// glowing core. Three 1024 px PNGs: light, dark and the tinted (grayscale) variant for iOS 18.
//
//   swift ios/Tools/make-icon.swift ios/Aether/Resources/Assets.xcassets/AppIcon.appiconset
import AppKit
import CoreGraphics
import Foundation

let out = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "."
let size = 1024

func rgb(_ hex: UInt32) -> CGColor {
    CGColor(srgbRed: CGFloat((hex >> 16) & 0xFF) / 255, green: CGFloat((hex >> 8) & 0xFF) / 255, blue: CGFloat(hex & 0xFF) / 255, alpha: 1)
}

enum Variant { case light, dark, tinted }

func render(_ variant: Variant) -> CGImage {
    let space = CGColorSpace(name: CGColorSpace.sRGB)!
    let ctx = CGContext(data: nil, width: size, height: size, bitsPerComponent: 8, bytesPerRow: 0, space: space,
                        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    let rect = CGRect(x: 0, y: 0, width: size, height: size)
    let s = CGFloat(size)

    // Background: the accent gradient (light), a deep night sky (dark) or a neutral gray ramp (tinted).
    let top: CGColor, bottom: CGColor
    switch variant {
    case .light: top = rgb(0x6F8CFF); bottom = rgb(0x3E5BD8)
    case .dark: top = rgb(0x1A1F3C); bottom = rgb(0x0E0F14)
    case .tinted: top = rgb(0x6E6E6E); bottom = rgb(0x2A2A2A)
    }
    let gradient = CGGradient(colorsSpace: space, colors: [top, bottom] as CFArray, locations: [0, 1])!
    ctx.drawLinearGradient(gradient, start: CGPoint(x: 0, y: s), end: CGPoint(x: s * 0.35, y: 0),
                           options: [.drawsBeforeStartLocation, .drawsAfterEndLocation])

    let center = CGPoint(x: s / 2, y: s / 2)
    let glow: CGColor = variant == .tinted ? rgb(0xFFFFFF) : (variant == .dark ? rgb(0x8AA2FF) : rgb(0xFFFFFF))

    // Halo: the glow behind the mark.
    for i in 0..<6 {
        let r = s * (0.22 + CGFloat(i) * 0.05)
        ctx.setFillColor(glow.copy(alpha: 0.06)!)
        ctx.fillEllipse(in: CGRect(x: center.x - r, y: center.y - r, width: 2 * r, height: 2 * r))
    }

    // The ring: an open arc, the "upper air" that carries things to every device.
    ctx.setLineCap(.round)
    ctx.setStrokeColor(glow)
    ctx.setLineWidth(s * 0.075)
    let r = s * 0.30
    ctx.addArc(center: center, radius: r, startAngle: .pi * 0.62, endAngle: .pi * 2.38, clockwise: false)
    ctx.strokePath()

    // The core.
    let core = s * 0.105
    ctx.setFillColor(glow)
    ctx.fillEllipse(in: CGRect(x: center.x - core, y: center.y - core, width: 2 * core, height: 2 * core))

    // A small satellite in the ring's opening: what travels over the aether.
    let dot = s * 0.034
    let angle = CGFloat.pi * 0.5
    let p = CGPoint(x: center.x + cos(angle) * r, y: center.y + sin(angle) * r)
    ctx.fillEllipse(in: CGRect(x: p.x - dot, y: p.y - dot, width: 2 * dot, height: 2 * dot))

    _ = rect
    return ctx.makeImage()!
}

func write(_ image: CGImage, to path: String) throws {
    let url = URL(fileURLWithPath: path)
    let rep = NSBitmapImageRep(cgImage: image)
    guard let data = rep.representation(using: .png, properties: [:]) else { throw NSError(domain: "icon", code: 1) }
    try data.write(to: url)
    print("wrote \(path)")
}

try FileManager.default.createDirectory(atPath: out, withIntermediateDirectories: true)
try write(render(.light), to: "\(out)/AppIcon-1024.png")
try write(render(.dark), to: "\(out)/AppIcon-1024-dark.png")
try write(render(.tinted), to: "\(out)/AppIcon-1024-tinted.png")
