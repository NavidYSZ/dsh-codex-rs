import AppKit
import QuartzCore

/// Visual settings the DSH plugin may override with the `configure` command.
struct OverlayStyle {
    var accent = NSColor(srgbRed: 0x33 / 255.0, green: 0x9c / 255.0, blue: 0xff / 255.0, alpha: 1)
    var label = "DeepSeek"
    var banner = "DeepSeek verwendet deinen Computer"
    var escHint = "Esc zum Abbrechen"
    /// Codex shows no name tag and no screen glow; both stay available.
    var showLabel = false
    var showGlow = false
    var showActivity = true
    var clickPulse = true
    var idleSeconds: Double = 12
}

extension NSColor {
    convenience init?(hex: String) {
        var s = hex.trimmingCharacters(in: .whitespaces)
        if s.hasPrefix("#") { s.removeFirst() }
        guard s.count == 6, let v = UInt32(s, radix: 16) else { return nil }
        self.init(srgbRed: CGFloat((v >> 16) & 0xFF) / 255, green: CGFloat((v >> 8) & 0xFF) / 255,
                  blue: CGFloat(v & 0xFF) / 255, alpha: 1)
    }
}

/// A transparent, click-through window per display. It joins all Spaces,
/// floats above full-screen apps and is excluded from our own screenshots.
final class OverlayWindow: NSWindow {
    let displayID: CGDirectDisplayID
    let root = CALayer()

    init(screen: NSScreen, displayID: CGDirectDisplayID) {
        self.displayID = displayID
        super.init(contentRect: screen.frame, styleMask: .borderless, backing: .buffered, defer: false)
        isOpaque = false
        backgroundColor = .clear
        hasShadow = false
        ignoresMouseEvents = true
        level = .screenSaver
        collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary, .ignoresCycle]
        sharingType = .none
        isReleasedWhenClosed = false
        let view = NSView(frame: NSRect(origin: .zero, size: screen.frame.size))
        view.wantsLayer = true
        view.layer = root
        root.frame = view.bounds
        contentView = view
        setFrame(screen.frame, display: false)
    }

    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}

/// The agent cursor: a rounded black arrowhead with a white rim and an
/// accent glow, moved with Codex-like motion (bowed path for long moves,
/// tip turning into the direction of travel, speed squash, settle wiggle).
@MainActor
final class Overlay {
    var style = OverlayStyle() { didSet { restyle() } }

    private var windows: [CGDirectDisplayID: OverlayWindow] = [:]
    private var glowLayers: [CGDirectDisplayID: CALayer] = [:]

    private let cursor = CALayer()          // positioned at the tip
    private let glyph = CALayer()           // rotates/squashes around the tip
    private let glowFar = CAShapeLayer()
    private let glowNear = CAShapeLayer()
    private let arrow = CAShapeLayer()
    private let tag = CALayer()
    private let tagText = CATextLayer()
    private let activity = CALayer()
    private let activityText = CATextLayer()

    private var banner: NSVisualEffectView?
    private var bannerDot: CALayer?
    private var bannerLabel: NSTextField?
    private var bannerHint: NSTextField?
    private var idleWork: DispatchWorkItem?

    private var timer: Timer?
    private var finishMotion: (() -> Void)?
    private var generation = 0

    /// Global CoreGraphics point (y down) of the cursor tip.
    private(set) var position: CGPoint?
    private(set) var active = false
    private var cursorDisplay: CGDirectDisplayID?

    // Arrowhead in a 22×22 box, tip at (2,2), y down; matches client.js.
    static let box: CGFloat = 22
    private static let tipOffset = CGPoint(x: 2, y: 2)
    private static let restAngle: CGFloat = -135
    /// The 22pt outline drawn at ~23pt like the Codex cursor asset.
    private static let glyphScale: CGFloat = 1.25

    init() {
        buildCursor()
        rebuildWindows()
        NotificationCenter.default.addObserver(forName: NSApplication.didChangeScreenParametersNotification,
                                               object: nil, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.rebuildWindows() }
        }
    }

    // MARK: windows

    private func rebuildWindows() {
        for w in windows.values { w.orderOut(nil) }
        windows = [:]
        glowLayers = [:]
        banner?.removeFromSuperview()
        for screen in NSScreen.screens {
            guard let id = screen.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? CGDirectDisplayID else { continue }
            let w = OverlayWindow(screen: screen, displayID: id)
            windows[id] = w
            let glow = makeGlow(size: screen.frame.size)
            glow.opacity = active && style.showGlow ? 1 : 0
            w.root.addSublayer(glow)
            glowLayers[id] = glow
            w.orderFrontRegardless()
        }
        cursor.removeFromSuperlayer()
        cursorDisplay = nil
        if let p = position { place(p) }
        if active { showBanner() }
    }

    private func screen(for id: CGDirectDisplayID) -> NSScreen? {
        NSScreen.screens.first { ($0.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? CGDirectDisplayID) == id }
    }

    /// Global CG point (y down) → layer point in that display's overlay (y up).
    private func local(_ p: CGPoint, in id: CGDirectDisplayID) -> CGPoint {
        let b = CGDisplayBounds(id)
        return CGPoint(x: p.x - b.minX, y: b.height - (p.y - b.minY))
    }

    // MARK: cursor construction

    /// Same outline as ARROW in client.js; y-up layer space unless `yDown`.
    static func arrowPath(yDown: Bool = false) -> CGPath {
        let b = box
        func p(_ x: CGFloat, _ y: CGFloat) -> CGPoint { CGPoint(x: x, y: yDown ? y : b - y) }
        let path = CGMutablePath()
        path.move(to: p(2, 2))
        path.addLine(to: p(7.3, 18.4))
        path.addQuadCurve(to: p(8.9, 18.8), control: p(7.9, 19.9))
        path.addLine(to: p(11.1, 11.1))
        path.addLine(to: p(18.8, 8.9))
        path.addQuadCurve(to: p(18.4, 7.3), control: p(19.9, 7.9))
        path.closeSubpath()
        return path
    }

    private func buildCursor() {
        let b = Overlay.box
        let scale = NSScreen.main?.backingScaleFactor ?? 2
        cursor.bounds = .zero
        cursor.opacity = 0

        glyph.bounds = CGRect(x: 0, y: 0, width: b, height: b)
        glyph.anchorPoint = CGPoint(x: Overlay.tipOffset.x / b, y: (b - Overlay.tipOffset.y) / b)
        glyph.position = .zero
        cursor.addSublayer(glyph)

        let path = Overlay.arrowPath()
        for (layer, radius, opacity) in [(glowFar, CGFloat(15), Float(0.48)), (glowNear, CGFloat(6), Float(0.9))] {
            layer.path = path
            layer.shadowPath = path
            layer.shadowRadius = radius
            layer.shadowOpacity = opacity
            layer.shadowOffset = .zero
            layer.frame = glyph.bounds
            glyph.addSublayer(layer)
        }
        arrow.path = path
        arrow.frame = glyph.bounds
        arrow.fillColor = NSColor(white: 0.05, alpha: 1).cgColor
        arrow.strokeColor = NSColor.white.cgColor
        arrow.lineWidth = 2
        arrow.lineJoin = .round
        arrow.shadowColor = NSColor.black.cgColor
        arrow.shadowOpacity = 0.25
        arrow.shadowRadius = 1.5
        arrow.shadowOffset = CGSize(width: 0, height: -1)
        arrow.contentsScale = scale
        glyph.addSublayer(arrow)

        // Optional name tag (off by default, Codex has none).
        tag.cornerRadius = 9
        tag.borderColor = NSColor.white.withAlphaComponent(0.9).cgColor
        tag.borderWidth = 1
        text(tagText, size: 11.5, weight: .semibold)
        tag.addSublayer(tagText)
        cursor.addSublayer(tag)

        // Activity pill next to the cursor, like Codex's avatar overlay.
        activity.cornerRadius = 8
        activity.backgroundColor = NSColor(srgbRed: 0.1, green: 0.11, blue: 0.12, alpha: 0.88).cgColor
        activity.borderColor = NSColor.white.withAlphaComponent(0.12).cgColor
        activity.borderWidth = 0.5
        activity.shadowColor = NSColor.black.cgColor
        activity.shadowOpacity = 0.2
        activity.shadowRadius = 6
        activity.shadowOffset = CGSize(width: 0, height: -2)
        activity.opacity = 0
        text(activityText, size: 12, weight: .medium)
        activity.addSublayer(activityText)
        cursor.addSublayer(activity)
        restyle()
    }

    private func text(_ t: CATextLayer, size: CGFloat, weight: NSFont.Weight) {
        t.font = NSFont.systemFont(ofSize: size, weight: weight)
        t.fontSize = size
        t.foregroundColor = NSColor.white.cgColor
        t.alignmentMode = .center
        t.contentsScale = NSScreen.main?.backingScaleFactor ?? 2
        t.truncationMode = .end
    }

    private func restyle() {
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        glowFar.fillColor = style.accent.cgColor
        glowFar.shadowColor = style.accent.cgColor
        glowNear.fillColor = style.accent.cgColor
        glowNear.shadowColor = style.accent.cgColor
        tag.backgroundColor = style.accent.cgColor
        tagText.string = style.label
        let font = NSFont.systemFont(ofSize: 11.5, weight: .semibold)
        let w = min(120, ceil((style.label as NSString).size(withAttributes: [.font: font]).width) + 16)
        tag.frame = CGRect(x: 16, y: -38, width: w, height: 18)
        tagText.frame = CGRect(x: 0, y: 1.5, width: w, height: 15)
        tag.isHidden = !style.showLabel || style.label.isEmpty
        for glow in glowLayers.values { styleGlow(glow) }
        bannerDot?.backgroundColor = style.accent.cgColor
        bannerLabel?.stringValue = style.banner
        bannerHint?.stringValue = style.escHint
        CATransaction.commit()
    }

    // MARK: placement and motion

    /// Moves the cursor layer to `p` immediately, switching displays if needed.
    private func place(_ p: CGPoint) {
        let id = Capture.display(containing: p)
        guard let window = windows[id] else { return }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        if cursorDisplay != id {
            cursor.removeFromSuperlayer()
            window.root.addSublayer(cursor)
            cursorDisplay = id
        }
        cursor.position = local(p, in: id)
        CATransaction.commit()
        position = p
    }

    /// Rotation and squash in screen space (degrees clockwise, y down),
    /// converted to the y-up layer space.
    private func pose(rotation: CGFloat, along: CGFloat, squash: CGFloat, scale: CGFloat = 1) {
        let r = -rotation * .pi / 180
        let a = -along * .pi / 180
        var t = CATransform3DMakeRotation(r, 0, 0, 1)
        t = CATransform3DConcat(t, CATransform3DMakeRotation(-a, 0, 0, 1))
        t = CATransform3DConcat(t, CATransform3DMakeScale(squash, 1, 1))
        t = CATransform3DConcat(t, CATransform3DMakeRotation(a, 0, 0, 1))
        t = CATransform3DConcat(t, CATransform3DMakeScale(scale * Overlay.glyphScale, scale * Overlay.glyphScale, 1))
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        glyph.transform = t
        CATransaction.commit()
    }

    /// Runs `frame(t)` for t in 0…1 at display rate; resumes when done or
    /// when another motion takes over.
    private func animate(duration: Double, frame: @escaping (CGFloat) -> Void) async {
        finishMotion?()
        timer?.invalidate()
        await withCheckedContinuation { (done: CheckedContinuation<Void, Never>) in
            var resumed = false
            let finish = { if !resumed { resumed = true; done.resume() } }
            finishMotion = finish
            let start = CACurrentMediaTime()
            let t = Timer(timeInterval: 1.0 / 120.0, repeats: true) { [weak self] timer in
                MainActor.assumeIsolated {
                    let progress = CGFloat(min(1, (CACurrentMediaTime() - start) / duration))
                    frame(progress)
                    if progress >= 1 {
                        timer.invalidate()
                        if self?.timer === timer { self?.timer = nil; self?.finishMotion = nil }
                        finish()
                    }
                }
            }
            timer = t
            RunLoop.main.add(t, forMode: .common)
        }
    }

    private static func ease(_ t: CGFloat) -> CGFloat { t < 0.5 ? 4 * t * t * t : 1 - pow(-2 * t + 2, 3) / 2 }
    private static func wrap(_ a: CGFloat) -> CGFloat { var x = (a + 540).truncatingRemainder(dividingBy: 360); if x < 0 { x += 360 }; return x - 180 }

    /// Glide the agent cursor to `target`. Returns once it has arrived, so
    /// the real event fires under the visible pointer.
    func travel(to target: CGPoint, from fallback: CGPoint) async {
        activate()
        generation += 1
        let gen = generation
        if position == nil {
            // Emerge at the user's pointer, then travel from there.
            place(fallback)
            await appear()
        }
        guard gen == generation, let start = position else { return }
        if Capture.display(containing: start) != Capture.display(containing: target) {
            place(target)
            await appear()
            return
        }
        let dx = target.x - start.x, dy = target.y - start.y
        let dist = hypot(dx, dy)
        guard dist > 1 else { place(target); return }

        let nx = dx / dist, ny = dy / dist
        let long = dist > 196
        let bow = long ? min(120, dist * 0.16) : 0
        let ctrl = CGPoint(x: (start.x + target.x) / 2 - ny * bow, y: (start.y + target.y) / 2 + nx * bow)
        let duration = Double(min(0.9, max(0.32, 0.26 + dist * 0.0005)))
        var prev = start
        var prevTime = CACurrentMediaTime()

        await animate(duration: duration) { [self] t in
            let e = Overlay.ease(t)
            let u = 1 - e
            let p = CGPoint(x: u * u * start.x + 2 * u * e * ctrl.x + e * e * target.x,
                            y: u * u * start.y + 2 * u * e * ctrl.y + e * e * target.y)
            let now = CACurrentMediaTime()
            let speed = hypot(p.x - prev.x, p.y - prev.y) / CGFloat(max(1.0 / 240, now - prevTime))
            let weight = sin(.pi * t)
            var along: CGFloat
            var rotation: CGFloat
            if long {
                let tx = 2 * (1 - e) * (ctrl.x - start.x) + 2 * e * (target.x - ctrl.x)
                let ty = 2 * (1 - e) * (ctrl.y - start.y) + 2 * e * (target.y - ctrl.y)
                along = atan2(ty, tx) * 180 / .pi
                rotation = Overlay.wrap(along - Overlay.restAngle) * weight
            } else {
                along = atan2(ny, nx) * 180 / .pi
                rotation = max(-1, min(1, nx * 0.75 - ny * 0.62)) * 70 * weight
            }
            let squash = long ? max(0.65, min(1, 1 - speed / 5500)) : 1 - 0.15 * weight
            place(p)
            pose(rotation: rotation, along: along, squash: squash)
            prev = p
            prevTime = now
        }
        guard gen == generation else { return }
        place(target)
        pose(rotation: 0, along: 0, squash: 1)
        settle()
    }

    private func appear() async {
        cursor.opacity = 1
        await animate(duration: 0.32) { [self] t in
            let e = 1 - pow(1 - t, 3)
            glyph.opacity = Float(e)
            pose(rotation: 0, along: 0, squash: 1, scale: 0.4 + 0.6 * e)
        }
        glyph.opacity = 1
        pose(rotation: 0, along: 0, squash: 1)
    }

    /// The "thinking" wiggle after arriving: ±12.5° under a sine envelope.
    private func settle() {
        Task { @MainActor in
            await animate(duration: 1.41) { [self] t in
                let seconds = t * 1.41
                let rot = 12.5 * sin(seconds / 0.66 * 2 * .pi) * sin(.pi * t)
                pose(rotation: rot, along: 0, squash: 1)
            }
        }
    }

    /// Press feedback; with clickPulse also a soft accent ring at the tip.
    func pulse(at p: CGPoint, double: Bool = false) {
        finishMotion?()
        timer?.invalidate()
        pose(rotation: 0, along: 0, squash: 1)
        let press = CAKeyframeAnimation(keyPath: "transform.scale")
        let g = Overlay.glyphScale
        press.values = [g, g * 0.85, g]
        press.keyTimes = [0, 0.35, 1]
        press.duration = 0.2
        glyph.add(press, forKey: "press")
        guard style.clickPulse else { return }

        let id = Capture.display(containing: p)
        guard let window = windows[id] else { return }
        for i in 0..<(double ? 2 : 1) {
            let ring = CAShapeLayer()
            let r: CGFloat = 16
            ring.path = CGPath(ellipseIn: CGRect(x: -r, y: -r, width: 2 * r, height: 2 * r), transform: nil)
            ring.fillColor = style.accent.withAlphaComponent(0.14).cgColor
            ring.strokeColor = style.accent.withAlphaComponent(0.85).cgColor
            ring.lineWidth = 1.5
            ring.position = local(p, in: id)
            ring.opacity = 0
            if cursor.superlayer === window.root { window.root.insertSublayer(ring, below: cursor) }
            else { window.root.addSublayer(ring) }

            let grow = CABasicAnimation(keyPath: "transform.scale")
            grow.fromValue = 0.2
            grow.toValue = 1.15
            let fade = CAKeyframeAnimation(keyPath: "opacity")
            fade.values = [0.95, 0.5, 0]
            fade.keyTimes = [0, 0.45, 1]
            let group = CAAnimationGroup()
            group.animations = [grow, fade]
            group.duration = 0.5
            group.beginTime = CACurrentMediaTime() + Double(i) * 0.14
            group.timingFunction = CAMediaTimingFunction(controlPoints: 0.19, 1, 0.22, 1)
            group.fillMode = .backwards
            CATransaction.begin()
            CATransaction.setCompletionBlock { ring.removeFromSuperlayer() }
            ring.add(group, forKey: "pulse")
            CATransaction.commit()
        }
    }

    /// Short status next to the cursor ("Klickt", "Tippt …"); nil hides it.
    func setActivity(_ text: String?) {
        CATransaction.begin()
        CATransaction.setAnimationDuration(0.18)
        if let text, !text.isEmpty, style.showActivity {
            let font = NSFont.systemFont(ofSize: 12, weight: .medium)
            let w = min(260, ceil((text as NSString).size(withAttributes: [.font: font]).width) + 20)
            activityText.string = text
            activity.frame = CGRect(x: 18, y: -(style.showLabel ? 64 : 40), width: w, height: 22)
            activityText.frame = CGRect(x: 0, y: 3, width: w, height: 16)
            activity.opacity = 1
        } else {
            activity.opacity = 0
        }
        CATransaction.commit()
    }

    /// Debug aid: renders the cursor in a few poses onto light and dark tiles.
    func renderPreview(to path: String) -> Bool {
        let tile: CGFloat = 120, scale: CGFloat = 2
        let poses: [(CGFloat, CGFloat, CGFloat)] = [(0, 0, 1), (35, 0, 1), (-60, 45, 0.75), (12.5, 0, 1)]
        let width = Int(tile * CGFloat(poses.count) * scale), height = Int(tile * 2 * scale)
        guard let ctx = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
                                  space: CGColorSpace(name: CGColorSpace.sRGB)!,
                                  bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return false }
        ctx.scaleBy(x: scale, y: scale)
        for (row, bg) in [NSColor.white, NSColor(white: 0.094, alpha: 1)].enumerated() {
            for (i, p) in poses.enumerated() {
                ctx.saveGState()
                ctx.setFillColor(bg.cgColor)
                ctx.fill(CGRect(x: CGFloat(i) * tile, y: CGFloat(row) * tile, width: tile, height: tile))
                ctx.translateBy(x: CGFloat(i) * tile + 50, y: CGFloat(row) * tile + 70)
                cursor.opacity = 1
                glyph.opacity = 1
                pose(rotation: p.0, along: p.1, squash: p.2)
                cursor.render(in: ctx)
                ctx.restoreGState()
            }
        }
        pose(rotation: 0, along: 0, squash: 1)
        cursor.opacity = position == nil ? 0 : 1
        guard let image = ctx.makeImage() else { return false }
        let rep = NSBitmapImageRep(cgImage: image)
        guard let data = rep.representation(using: .png, properties: [:]) else { return false }
        return (try? data.write(to: URL(fileURLWithPath: path))) != nil
    }

    // MARK: session chrome

    func activate() {
        idleWork?.cancel()
        let work = DispatchWorkItem { [weak self] in self?.deactivate() }
        idleWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + style.idleSeconds, execute: work)
        guard !active else { return }
        active = true
        for w in windows.values { w.orderFrontRegardless() }
        if style.showGlow { for g in glowLayers.values { setOpacity(g, 1, 0.35) } }
        showBanner()
    }

    func deactivate() {
        idleWork?.cancel()
        finishMotion?()
        timer?.invalidate()
        active = false
        setActivity(nil)
        for g in glowLayers.values { setOpacity(g, 0, 0.4) }
        setOpacity(cursor, 0, 0.3)
        if let banner {
            NSAnimationContext.runAnimationGroup { ctx in
                ctx.duration = 0.25
                banner.animator().alphaValue = 0
            }
        }
        position = nil
    }

    private func setOpacity(_ layer: CALayer, _ value: Float, _ duration: Double) {
        CATransaction.begin()
        CATransaction.setAnimationDuration(duration)
        layer.opacity = value
        CATransaction.commit()
    }

    private func makeGlow(size: CGSize) -> CALayer {
        // A thick accent ring just outside the display casts its shadow
        // inwards; the container clips everything beyond the screen edge.
        let glow = CALayer()
        glow.frame = CGRect(origin: .zero, size: size)
        glow.masksToBounds = true
        glow.opacity = 0
        let ring = CALayer()
        ring.frame = glow.bounds.insetBy(dx: -24, dy: -24)
        ring.borderWidth = 24
        ring.shadowOffset = .zero
        ring.shadowRadius = 18
        ring.shadowOpacity = 0.8
        glow.addSublayer(ring)
        styleGlow(glow)
        return glow
    }

    private func styleGlow(_ glow: CALayer) {
        glow.borderWidth = 1.5
        glow.borderColor = style.accent.withAlphaComponent(0.65).cgColor
        if let ring = glow.sublayers?.first {
            ring.borderColor = style.accent.cgColor
            ring.shadowColor = style.accent.cgColor
        }
    }

    private func showBanner() {
        let id = position.map { Capture.display(containing: $0) } ?? CGMainDisplayID()
        guard let window = windows[id] ?? windows.values.first, let screen = screen(for: window.displayID),
              let content = window.contentView else { return }

        let banner = self.banner ?? makeBanner()
        if banner.superview !== content { content.addSubview(banner) }
        banner.layoutSubtreeIfNeeded()
        let size = banner.fittingSize
        let topInset = screen.frame.maxY - screen.visibleFrame.maxY
        banner.frame = NSRect(x: (screen.frame.width - size.width) / 2,
                              y: screen.frame.height - topInset - size.height - 10,
                              width: size.width, height: size.height)
        NSAnimationContext.runAnimationGroup { ctx in
            ctx.duration = 0.25
            banner.animator().alphaValue = 1
        }
    }

    private func makeBanner() -> NSVisualEffectView {
        let v = NSVisualEffectView()
        v.material = .hudWindow
        v.blendingMode = .behindWindow
        v.state = .active
        v.appearance = NSAppearance(named: .vibrantDark)
        v.wantsLayer = true
        v.layer?.cornerRadius = 16
        v.layer?.masksToBounds = true
        v.alphaValue = 0

        let dot = NSView()
        dot.wantsLayer = true
        dot.layer?.cornerRadius = 4
        dot.layer?.backgroundColor = style.accent.cgColor
        dot.translatesAutoresizingMaskIntoConstraints = false
        dot.widthAnchor.constraint(equalToConstant: 8).isActive = true
        dot.heightAnchor.constraint(equalToConstant: 8).isActive = true
        let breathe = CABasicAnimation(keyPath: "opacity")
        breathe.fromValue = 1
        breathe.toValue = 0.35
        breathe.duration = 0.9
        breathe.autoreverses = true
        breathe.repeatCount = .infinity
        dot.layer?.add(breathe, forKey: "breathe")

        let label = NSTextField(labelWithString: style.banner)
        label.font = .systemFont(ofSize: 13, weight: .medium)
        label.textColor = .white
        let hint = NSTextField(labelWithString: style.escHint)
        hint.font = .systemFont(ofSize: 12, weight: .regular)
        hint.textColor = NSColor.white.withAlphaComponent(0.6)

        let stack = NSStackView(views: [dot, label, hint])
        stack.orientation = .horizontal
        stack.spacing = 10
        stack.setCustomSpacing(14, after: label)
        stack.edgeInsets = NSEdgeInsets(top: 7, left: 14, bottom: 7, right: 16)
        stack.translatesAutoresizingMaskIntoConstraints = false
        v.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: v.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: v.trailingAnchor),
            stack.topAnchor.constraint(equalTo: v.topAnchor),
            stack.bottomAnchor.constraint(equalTo: v.bottomAnchor),
        ])
        bannerDot = dot.layer
        bannerLabel = label
        bannerHint = hint
        self.banner = v
        return v
    }
}
