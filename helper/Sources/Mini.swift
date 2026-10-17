import AppKit
import QuartzCore

/// Texts of the mini window; `configure` may localize them.
struct MiniStrings {
    var done = "Fertig"
    var stopped = "Gestoppt"
    var stop = "Stoppen"
    var open = "Anzeigen"
    var collapse = "Vorschau ein-/ausblenden"
}

private final class MiniPanel: NSPanel {
    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}

private final class MiniButton: NSButton {
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
}

/// y-down container, so preview math matches screenshot pixels.
private class FlippedView: NSView {
    override var isFlipped: Bool { true }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
}

/// The header drags the panel; a click on it (or the preview) opens the target.
private final class HandleView: FlippedView {
    var onClick: (() -> Void)?
    private var downAt: NSPoint?
    private var originAt: NSPoint?
    private var moved = false
    override func mouseDown(with event: NSEvent) {
        downAt = NSEvent.mouseLocation
        originAt = window?.frame.origin
        moved = false
    }
    override func mouseDragged(with event: NSEvent) {
        guard let downAt, let originAt, let window else { return }
        let p = NSEvent.mouseLocation
        if hypot(p.x - downAt.x, p.y - downAt.y) > 3 { moved = true }
        if moved { window.setFrameOrigin(NSPoint(x: originAt.x + p.x - downAt.x, y: originAt.y + p.y - downAt.y)) }
    }
    override func mouseUp(with event: NSEvent) {
        if !moved { onClick?() }
        downAt = nil
    }
    override func resetCursorRects() { if onClick != nil { addCursorRect(bounds, cursor: .pointingHand) } }
}

/// Codex-style "Mini": a small floating glass window that shows a live view
/// of what the agent operates in the background (an app window or the
/// headless browser), its own cursor inside that view, the current step and
/// a stop button. It never takes focus and is invisible to screenshots.
@MainActor
final class Mini {
    enum Source: Equatable { case window(CGWindowID, CGRect), frames }

    var strings = MiniStrings()
    var accent = NSColor(srgbRed: 0x33 / 255.0, green: 0x9c / 255.0, blue: 0xff / 255.0, alpha: 1) { didSet { restyle() } }
    /// Seconds without a step before the window says "done", then hides.
    var doneAfter: Double = 6
    var hideAfter: Double = 24
    var onStop: (() -> Void)?
    var onOpen: (() -> Void)?

    private(set) var visible = false
    private(set) var source: Source?
    private var requests: [String] = []

    private let width: CGFloat = 340
    private let headerHeight: CGFloat = 56
    private var expanded = UserDefaults.standard.object(forKey: "miniExpanded") as? Bool ?? true

    private let panel: MiniPanel
    private let root = FlippedView()
    private let header = HandleView()
    private let icon = NSImageView()
    private let busyDot = CALayer()
    private let titleLabel = NSTextField(labelWithString: "")
    private let subtitle = NSTextField(labelWithString: "")
    private let shimmer = CAGradientLayer()
    private let stopButton = MiniButton()
    private let toggleButton = MiniButton()
    private let preview = HandleView()
    private let imageLayer = CALayer()
    private let cursor = CALayer()
    private let glyph = CALayer()
    private let glow = CAShapeLayer()
    private let arrow = CAShapeLayer()

    private var aspect = CGSize(width: 16, height: 10)
    private var hasImage = false
    private var liveTimer: Timer?
    private var capturing = false
    private var doneWork: DispatchWorkItem?
    private var hideWork: DispatchWorkItem?
    private var cursorPoint: CGPoint?      // normalized 0…1
    private var motion: Timer?
    private var finishMotion: (() -> Void)?

    init() {
        panel = MiniPanel(contentRect: NSRect(x: 0, y: 0, width: 340, height: 56),
                          styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: true)
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = true
        panel.level = .floating
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary, .ignoresCycle]
        panel.hidesOnDeactivate = false
        panel.isReleasedWhenClosed = false
        panel.sharingType = .none
        panel.isMovable = true
        panel.alphaValue = 0
        build()
        NotificationCenter.default.addObserver(forName: NSWindow.didMoveNotification, object: panel, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self, self.visible else { return }
                UserDefaults.standard.set(NSStringFromPoint(self.panel.frame.origin), forKey: "miniOrigin")
            }
        }
    }

    // MARK: construction

    private func build() {
        let frame = NSRect(x: 0, y: 0, width: width, height: headerHeight)
        root.frame = frame
        root.autoresizingMask = [.width, .height]
        if #available(macOS 26.0, *) {
            let glass = NSGlassEffectView(frame: frame)
            glass.cornerRadius = 20
            glass.contentView = root
            panel.contentView = glass
        } else {
            let fx = NSVisualEffectView(frame: frame)
            fx.material = .popover
            fx.blendingMode = .behindWindow
            fx.state = .active
            fx.wantsLayer = true
            fx.layer?.cornerRadius = 18
            fx.layer?.masksToBounds = true
            fx.addSubview(root)
            panel.contentView = fx
        }

        header.frame = NSRect(x: 0, y: 0, width: width, height: headerHeight)
        header.autoresizingMask = [.width]
        header.onClick = { [weak self] in self?.open() }
        root.addSubview(header)

        icon.frame = NSRect(x: 12, y: 12, width: 32, height: 32)
        icon.imageScaling = .scaleProportionallyUpOrDown
        header.addSubview(icon)
        busyDot.frame = CGRect(x: 26, y: 0, width: 9, height: 9)
        busyDot.cornerRadius = 4.5
        busyDot.borderWidth = 1.5
        busyDot.borderColor = NSColor.white.cgColor
        icon.wantsLayer = true
        icon.layer?.masksToBounds = false
        icon.layer?.addSublayer(busyDot)

        titleLabel.font = .systemFont(ofSize: 13, weight: .semibold)
        titleLabel.textColor = .labelColor
        titleLabel.lineBreakMode = .byTruncatingTail
        titleLabel.frame = NSRect(x: 54, y: 10, width: width - 54 - 84, height: 18)
        header.addSubview(titleLabel)

        subtitle.font = .systemFont(ofSize: 12, weight: .regular)
        subtitle.textColor = .secondaryLabelColor
        subtitle.lineBreakMode = .byTruncatingTail
        subtitle.frame = NSRect(x: 54, y: 28, width: width - 54 - 84, height: 17)
        subtitle.wantsLayer = true
        header.addSubview(subtitle)
        shimmer.colors = [NSColor(white: 0, alpha: 0.45).cgColor, NSColor.black.cgColor, NSColor(white: 0, alpha: 0.45).cgColor]
        shimmer.locations = [0.35, 0.5, 0.65]
        shimmer.startPoint = CGPoint(x: 0, y: 0.5)
        shimmer.endPoint = CGPoint(x: 1, y: 0.5)

        for (button, symbol, tip, action) in [
            (stopButton, "stop.fill", strings.stop, #selector(MiniTarget.stop)),
            (toggleButton, expanded ? "chevron.down" : "chevron.up", strings.collapse, #selector(MiniTarget.toggle)),
        ] {
            button.isBordered = false
            button.bezelStyle = .regularSquare
            button.imagePosition = .imageOnly
            button.image = NSImage(systemSymbolName: symbol, accessibilityDescription: tip)?
                .withSymbolConfiguration(.init(pointSize: symbol == "stop.fill" ? 9 : 11, weight: .semibold))
            button.contentTintColor = .labelColor
            button.toolTip = tip
            button.wantsLayer = true
            button.layer?.cornerRadius = 14
            button.target = MiniTarget.shared
            button.action = action
            header.addSubview(button)
        }
        stopButton.layer?.backgroundColor = NSColor.labelColor.withAlphaComponent(0.09).cgColor
        stopButton.frame = NSRect(x: width - 12 - 28 - 4 - 28, y: 14, width: 28, height: 28)
        toggleButton.frame = NSRect(x: width - 12 - 28, y: 14, width: 28, height: 28)
        MiniTarget.shared.mini = self

        preview.onClick = { [weak self] in self?.open() }
        preview.wantsLayer = true
        preview.layer?.cornerRadius = 12
        preview.layer?.masksToBounds = true
        preview.layer?.backgroundColor = NSColor.labelColor.withAlphaComponent(0.06).cgColor
        preview.toolTip = strings.open
        imageLayer.contentsGravity = .resizeAspect
        imageLayer.minificationFilter = .trilinear
        preview.layer?.addSublayer(imageLayer)
        buildCursor()
        preview.layer?.addSublayer(cursor)
        root.addSubview(preview)
        restyle()
    }

    private func buildCursor() {
        let b = Overlay.box
        let path = Overlay.arrowPath(yDown: true)
        cursor.bounds = .zero
        cursor.opacity = 0
        glyph.bounds = CGRect(x: 0, y: 0, width: b, height: b)
        glyph.anchorPoint = CGPoint(x: 2 / b, y: 2 / b)
        glyph.position = .zero
        cursor.addSublayer(glyph)
        glow.path = path
        glow.shadowPath = path
        glow.shadowRadius = 5
        glow.shadowOpacity = 0.9
        glow.shadowOffset = .zero
        glow.frame = glyph.bounds
        glyph.addSublayer(glow)
        arrow.path = path
        arrow.frame = glyph.bounds
        arrow.fillColor = NSColor(white: 0.05, alpha: 1).cgColor
        arrow.strokeColor = NSColor.white.cgColor
        arrow.lineWidth = 2
        arrow.lineJoin = .round
        arrow.contentsScale = NSScreen.main?.backingScaleFactor ?? 2
        glyph.addSublayer(arrow)
        glyph.transform = CATransform3DMakeScale(0.8, 0.8, 1)
    }

    private func restyle() {
        glow.fillColor = accent.cgColor
        glow.shadowColor = accent.cgColor
        busyDot.backgroundColor = accent.cgColor
    }

    // MARK: layout

    private var previewHeight: CGFloat {
        let w = width - 16
        return max(120, min(236, w * aspect.height / max(1, aspect.width)))
    }

    private var showsPreview: Bool { expanded && source != nil }

    private func layout(animated: Bool) {
        let height = headerHeight + (showsPreview ? previewHeight + 8 : 0)
        preview.isHidden = !showsPreview
        preview.frame = NSRect(x: 8, y: headerHeight, width: width - 16, height: previewHeight)
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        imageLayer.frame = preview.bounds
        CATransaction.commit()
        placeCursor()
        var frame = panel.frame
        frame.size = NSSize(width: width, height: height)   // bottom edge stays put
        panel.setFrame(frame, display: true, animate: animated && visible)
        toggleButton.image = NSImage(systemSymbolName: expanded ? "chevron.down" : "chevron.up", accessibilityDescription: strings.collapse)?
            .withSymbolConfiguration(.init(pointSize: 11, weight: .semibold))
        toggleButton.isHidden = source == nil
    }

    private func initialOrigin(height: CGFloat) -> NSPoint {
        if let saved = UserDefaults.standard.string(forKey: "miniOrigin") {
            let p = NSPointFromString(saved)
            if NSScreen.screens.contains(where: { $0.visibleFrame.insetBy(dx: -20, dy: -20).contains(NSPoint(x: p.x + 40, y: p.y + 20)) }) { return p }
        }
        let mouse = NSEvent.mouseLocation
        let screen = NSScreen.screens.first { $0.frame.contains(mouse) } ?? NSScreen.main ?? NSScreen.screens[0]
        let vf = screen.visibleFrame
        return NSPoint(x: vf.maxX - width - 20, y: vf.minY + 20)
    }

    // MARK: public API

    func show(title: String, iconImage: NSImage?, source: Source?, aspect: CGSize? = nil) {
        let changed = source != self.source
        self.source = source
        if let aspect, aspect.width > 0, aspect.height > 0 { self.aspect = aspect }
        if case .window(_, let frame)? = source, aspect == nil { self.aspect = frame.size }
        if changed { hasImage = false; imageLayer.contents = nil; cursorPoint = nil; cursor.opacity = 0 }
        titleLabel.stringValue = title
        icon.image = iconImage ?? NSImage(systemSymbolName: "globe", accessibilityDescription: nil)
        layout(animated: true)
        if !visible {
            visible = true
            let height = panel.frame.height
            panel.setFrameOrigin(initialOrigin(height: height))
            panel.orderFrontRegardless()
            if let layer = panel.contentView?.layer {
                let pop = CASpringAnimation(keyPath: "transform.scale")
                pop.fromValue = 0.92
                pop.toValue = 1
                pop.damping = 18
                pop.initialVelocity = 4
                pop.duration = pop.settlingDuration
                layer.add(pop, forKey: "pop")
            }
            NSAnimationContext.runAnimationGroup { $0.duration = 0.22; panel.animator().alphaValue = 1 }
        }
        busy()
        startLive()
    }

    func setActivity(_ text: String) {
        subtitle.stringValue = text
        busy()
    }

    func setImage(_ image: CGImage) {
        let newAspect = CGSize(width: image.width, height: image.height)
        let relayout = abs(newAspect.width / newAspect.height - aspect.width / aspect.height) > 0.02
        aspect = newAspect
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        imageLayer.contents = image
        CATransaction.commit()
        hasImage = true
        if relayout { layout(animated: false) }
    }

    func setImage(base64 data: String) {
        guard let raw = Data(base64Encoded: data), let rep = NSBitmapImageRep(data: raw), let cg = rep.cgImage else { return }
        setImage(cg)
    }

    func drainRequests() -> [String] { defer { requests = [] }; return requests }

    func request(_ name: String) { if !requests.contains(name) { requests.append(name) } }

    /// The step finished; the window says so, then fades out.
    func finished() {
        doneWork?.cancel()
        markIdle(strings.done)
    }

    func stopped() {
        markIdle(strings.stopped)
        scheduleHide(after: 1.6)
    }

    func hide() {
        doneWork?.cancel()
        hideWork?.cancel()
        stopLive()
        guard visible else { return }
        visible = false
        NSAnimationContext.runAnimationGroup({ $0.duration = 0.22; panel.animator().alphaValue = 0 },
                                             completionHandler: { [weak self] in
            MainActor.assumeIsolated { if self?.visible == false { self?.panel.orderOut(nil) } }
        })
        source = nil
    }

    /// Debug aid: let our own screenshots include the panel.
    func setShareable(_ on: Bool) { panel.sharingType = on ? .readOnly : .none }

    fileprivate func stopPressed() { onStop?() }

    fileprivate func toggle() {
        expanded.toggle()
        UserDefaults.standard.set(expanded, forKey: "miniExpanded")
        layout(animated: true)
        if expanded { startLive() } else { stopLive() }
    }

    private func open() { onOpen?() }

    // MARK: status

    private func busy() {
        doneWork?.cancel()
        hideWork?.cancel()
        stopButton.isHidden = false
        subtitle.textColor = .secondaryLabelColor
        if shimmer.animation(forKey: "sweep") == nil {
            shimmer.frame = CGRect(x: -subtitle.bounds.width, y: 0, width: subtitle.bounds.width * 3, height: subtitle.bounds.height)
            subtitle.layer?.mask = shimmer
            let sweep = CABasicAnimation(keyPath: "position.x")
            sweep.fromValue = -subtitle.bounds.width / 2
            sweep.toValue = subtitle.bounds.width * 1.5
            sweep.duration = 1.6
            sweep.repeatCount = .infinity
            shimmer.add(sweep, forKey: "sweep")
        }
        if busyDot.animation(forKey: "breathe") == nil {
            busyDot.opacity = 1
            let breathe = CABasicAnimation(keyPath: "opacity")
            breathe.fromValue = 1
            breathe.toValue = 0.3
            breathe.duration = 0.8
            breathe.autoreverses = true
            breathe.repeatCount = .infinity
            busyDot.add(breathe, forKey: "breathe")
        }
        let work = DispatchWorkItem { [weak self] in MainActor.assumeIsolated { self?.markIdle(self?.strings.done ?? "") } }
        doneWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + doneAfter, execute: work)
    }

    private func markIdle(_ text: String) {
        shimmer.removeAllAnimations()
        subtitle.layer?.mask = nil
        busyDot.removeAllAnimations()
        busyDot.opacity = 0
        stopButton.isHidden = true
        subtitle.stringValue = text
        stopLive()
        scheduleHide(after: hideAfter)
    }

    private func scheduleHide(after seconds: Double) {
        hideWork?.cancel()
        let work = DispatchWorkItem { [weak self] in MainActor.assumeIsolated { self?.hide() } }
        hideWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + seconds, execute: work)
    }

    // MARK: live view of a window

    private func startLive() {
        guard case .window(let id, let frame)? = source, expanded, liveTimer == nil else { return }
        let refresh = { [weak self] in
            guard let self, !self.capturing else { return }
            self.capturing = true
            Task { @MainActor in
                defer { self.capturing = false }
                if let image = try? await AppControl.captureWindow(id, frame: frame, maxWidth: 680),
                   self.source == .window(id, frame) { self.setImage(image) }
            }
        }
        refresh()
        let timer = Timer(timeInterval: 0.5, repeats: true) { _ in MainActor.assumeIsolated { refresh() } }
        RunLoop.main.add(timer, forMode: .common)
        liveTimer = timer
    }

    private func stopLive() {
        liveTimer?.invalidate()
        liveTimer = nil
    }

    // MARK: cursor

    /// Where a normalized point lands inside the aspect-fitted image.
    private func pointInPreview(_ n: CGPoint) -> CGPoint {
        let b = preview.bounds
        let scale = min(b.width / max(1, aspect.width), b.height / max(1, aspect.height))
        let w = aspect.width * scale, h = aspect.height * scale
        return CGPoint(x: (b.width - w) / 2 + n.x * w, y: (b.height - h) / 2 + n.y * h)
    }

    private func placeCursor() {
        guard let n = cursorPoint else { return }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        cursor.position = pointInPreview(n)
        CATransaction.commit()
    }

    private func pose(_ rotation: CGFloat, scale: CGFloat = 1) {
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        // Positive rotation is clockwise on screen; the layer is y-down here.
        glyph.transform = CATransform3DConcat(CATransform3DMakeRotation(rotation * .pi / 180, 0, 0, 1),
                                              CATransform3DMakeScale(0.8 * scale, 0.8 * scale, 1))
        CATransaction.commit()
    }

    /// Glides the cursor to `n` (normalized) with a short bowed path and
    /// tilt; returns on arrival so the real action happens under it.
    func moveCursor(to n: CGPoint, click: Bool) async {
        guard showsPreview, visible else { cursorPoint = n; return }
        let target = CGPoint(x: max(0, min(1, n.x)), y: max(0, min(1, n.y)))
        guard let start = cursorPoint else {
            cursorPoint = target
            placeCursor()
            cursor.opacity = 1
            await animate(0.28) { [self] t in
                let e = 1 - pow(1 - t, 3)
                glyph.opacity = Float(e)
                pose(0, scale: 0.4 + 0.6 * e)
            }
            if click { press() }
            return
        }
        let a = pointInPreview(start), b = pointInPreview(target)
        let dx = b.x - a.x, dy = b.y - a.y
        let dist = hypot(dx, dy)
        cursor.opacity = 1
        guard dist > 0.5 else { if click { press() }; return }
        let nx = dx / dist, ny = dy / dist
        let bow = min(30, dist * 0.18)
        let ctrl = CGPoint(x: (a.x + b.x) / 2 - ny * bow, y: (a.y + b.y) / 2 + nx * bow)
        await animate(Double(min(0.62, max(0.3, 0.24 + dist * 0.0025)))) { [self] t in
            let e = t < 0.5 ? 4 * t * t * t : 1 - pow(-2 * t + 2, 3) / 2
            let u = 1 - e
            let p = CGPoint(x: u * u * a.x + 2 * u * e * ctrl.x + e * e * b.x, y: u * u * a.y + 2 * u * e * ctrl.y + e * e * b.y)
            CATransaction.begin()
            CATransaction.setDisableActions(true)
            cursor.position = p
            CATransaction.commit()
            pose(max(-1, min(1, nx * 0.75 - ny * 0.62)) * 40 * sin(.pi * t))
        }
        cursorPoint = target
        placeCursor()
        pose(0)
        if click { press() }
    }

    private func press() {
        let squeeze = CAKeyframeAnimation(keyPath: "transform.scale")
        squeeze.values = [0.8, 0.66, 0.8]
        squeeze.keyTimes = [0, 0.35, 1]
        squeeze.duration = 0.2
        glyph.add(squeeze, forKey: "press")
        let ring = CAShapeLayer()
        let r: CGFloat = 11
        ring.path = CGPath(ellipseIn: CGRect(x: -r, y: -r, width: 2 * r, height: 2 * r), transform: nil)
        ring.fillColor = accent.withAlphaComponent(0.16).cgColor
        ring.strokeColor = accent.withAlphaComponent(0.9).cgColor
        ring.lineWidth = 1.5
        ring.position = cursor.position
        ring.opacity = 0
        preview.layer?.insertSublayer(ring, below: cursor)
        let grow = CABasicAnimation(keyPath: "transform.scale")
        grow.fromValue = 0.2
        grow.toValue = 1.2
        let fade = CAKeyframeAnimation(keyPath: "opacity")
        fade.values = [0.95, 0.5, 0]
        fade.keyTimes = [0, 0.45, 1]
        let group = CAAnimationGroup()
        group.animations = [grow, fade]
        group.duration = 0.5
        group.timingFunction = CAMediaTimingFunction(controlPoints: 0.19, 1, 0.22, 1)
        CATransaction.begin()
        CATransaction.setCompletionBlock { ring.removeFromSuperlayer() }
        ring.add(group, forKey: "pulse")
        CATransaction.commit()
    }

    private func animate(_ duration: Double, _ frame: @escaping (CGFloat) -> Void) async {
        finishMotion?()
        motion?.invalidate()
        await withCheckedContinuation { (done: CheckedContinuation<Void, Never>) in
            var resumed = false
            let finish = { if !resumed { resumed = true; done.resume() } }
            finishMotion = finish
            let start = CACurrentMediaTime()
            let timer = Timer(timeInterval: 1.0 / 120.0, repeats: true) { [weak self] timer in
                MainActor.assumeIsolated {
                    let t = CGFloat(min(1, (CACurrentMediaTime() - start) / duration))
                    frame(t)
                    if t >= 1 {
                        timer.invalidate()
                        if self?.motion === timer { self?.motion = nil; self?.finishMotion = nil }
                        finish()
                    }
                }
            }
            motion = timer
            RunLoop.main.add(timer, forMode: .common)
        }
    }

    static func icon(for spec: String?) -> NSImage? {
        guard let spec, !spec.isEmpty else { return nil }
        if spec.hasPrefix("/"), FileManager.default.fileExists(atPath: spec) { return NSWorkspace.shared.icon(forFile: spec) }
        if let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: spec) { return NSWorkspace.shared.icon(forFile: url.path) }
        return NSImage(systemSymbolName: spec, accessibilityDescription: nil)
    }
}

/// Objective-C target for the buttons (Mini itself is not an NSObject).
@MainActor
private final class MiniTarget: NSObject {
    static let shared = MiniTarget()
    weak var mini: Mini?
    @objc func stop() { mini?.stopPressed() }
    @objc func toggle() { mini?.toggle() }
}
