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

/// The header drags the whole stack; a click on it (or the preview) opens the target.
private final class HandleView: FlippedView {
    var onClick: (() -> Void)?
    var onDrag: ((NSSize) -> Void)?
    var cursor: NSCursor = .pointingHand
    private var down: NSPoint?
    private var last: NSPoint?
    private var moved = false

    override func mouseDown(with event: NSEvent) {
        let p = NSEvent.mouseLocation
        down = p
        last = p
        moved = false
    }

    override func mouseDragged(with event: NSEvent) {
        guard let down, let last else { return }
        let p = NSEvent.mouseLocation
        let d = NSSize(width: p.x - last.x, height: p.y - last.y)
        self.last = p
        if !moved, hypot(p.x - down.x, p.y - down.y) > 3 { moved = true }
        if moved { onDrag?(d) }
    }

    override func mouseUp(with event: NSEvent) {
        if !moved { onClick?() }
        down = nil
        last = nil
    }

    override func resetCursorRects() { addCursorRect(bounds, cursor: cursor) }
}

/// The visible card: a flat, borderless surface with rounded corners and a
/// hairline edge. No window material, no titlebar, no shadow.
private final class MiniCard: NSView {
    var radius: CGFloat = 3 { didSet { needsDisplay = true } }
    var onHover: ((Bool) -> Void)?
    private var tracking: NSTrackingArea?

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let tracking { removeTrackingArea(tracking) }
        let next = NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect], owner: self)
        addTrackingArea(next)
        tracking = next
    }
    override func mouseEntered(with event: NSEvent) { onHover?(true) }
    override func mouseExited(with event: NSEvent) { onHover?(false) }

    override var isFlipped: Bool { true }
    override var wantsUpdateLayer: Bool { true }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    override func updateLayer() {
        guard let layer else { return }
        // Always dark, like the Codex preview: the card is its own surface and
        // must not turn into a bright macOS panel over a dark DSH window.
        layer.cornerRadius = radius
        layer.cornerCurve = .continuous
        layer.masksToBounds = true
        layer.backgroundColor = NSColor(srgbRed: 0.11, green: 0.11, blue: 0.13, alpha: 1).cgColor
        layer.borderWidth = 1
        layer.borderColor = NSColor(white: 1, alpha: 0.10).cgColor
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        needsDisplay = true
    }
}

/// Codex-style "Mini": a small floating card that shows a live view of what
/// the agent operates in the background (an app window or the headless
/// browser), its own cursor inside that view, the current step and a stop
/// button. It never takes focus and is invisible to screenshots. Position,
/// size and visibility are owned by `MiniCenter`.
@MainActor
final class Mini {
    enum Source: Equatable { case window(CGWindowID, CGRect), display(CGDirectDisplayID, CGRect), frames }

    static let width: CGFloat = 232
    static let headerHeight: CGFloat = 36

    let owner: String
    var strings = MiniStrings()
    var accent = NSColor(srgbRed: 0x33 / 255.0, green: 0x9c / 255.0, blue: 0xff / 255.0, alpha: 1) { didSet { restyle() } }
    /// Seconds without a step before the window says "done", then hides.
    var doneAfter: Double = 6
    var hideAfter: Double = 24
    var onStop: (() -> Void)?
    var onOpen: (() -> Void)?
    var onDrag: ((NSSize) -> Void)?
    /// The card changed its size (preview aspect), so the stack has to re-flow.
    var onResize: (() -> Void)?
    /// The card went away and can be dropped from the stack.
    var onHidden: (() -> Void)?

    private(set) var wantsVisible = false
    /// Hidden because DSH is not the frontmost app; the card keeps its state.
    var suppressed = false { didSet { applyVisibility() } }
    /// Not the bottom card of the stack: its body is covered, so stop capturing.
    var occluded = false { didSet { if occluded { stopLive() } else { startLive() } } }

    var visible: Bool { wantsVisible && !suppressed }
    var onScreen: Bool { panel.isVisible && panel.alphaValue > 0.01 }

    private(set) var source: Source?
    private var requests: [String] = []

    private var expanded: Bool
    private let target = MiniTarget()

    private let panel: MiniPanel
    private let card = MiniCard(frame: .zero)
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
    private let liveCapture = LiveCapture()
    private var capturing = false
    private var doneWork: DispatchWorkItem?
    private var hideWork: DispatchWorkItem?
    private var cursorPoint: CGPoint?      // normalized 0…1
    private var motion: Timer?
    private var finishMotion: (() -> Void)?

    init(owner: String) {
        self.owner = owner
        self.expanded = UserDefaults.standard.object(forKey: "miniExpanded") as? Bool ?? true
        panel = MiniPanel(contentRect: NSRect(x: 0, y: 0, width: Mini.width, height: Mini.headerHeight),
                          styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: true)
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = true
        panel.level = .floating
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary, .ignoresCycle]
        panel.hidesOnDeactivate = false
        panel.isReleasedWhenClosed = false
        panel.sharingType = .none
        panel.isMovable = false
        panel.isMovableByWindowBackground = false
        panel.alphaValue = 0
        build()
        card.onHover = { [weak self] on in
            guard let self else { return }
            self.header.alphaValue = on || !self.expanded ? 1 : 0
        }
        liveCapture.onFrame = { [weak self] image in self?.setImage(image) }
        liveCapture.onStopped = { [weak self] in self?.onStop?() }
    }

    // MARK: construction

    private func build() {
        card.appearance = NSAppearance(named: .darkAqua)
        panel.contentView = card

        root.frame = NSRect(x: 0, y: 0, width: Mini.width, height: Mini.headerHeight)
        root.autoresizingMask = [.width, .height]
        card.addSubview(root)

        header.frame = NSRect(x: 0, y: 0, width: Mini.width, height: Mini.headerHeight)
        header.autoresizingMask = [.width]
        header.cursor = .openHand
        header.onClick = { [weak self] in self?.open() }
        header.onDrag = { [weak self] delta in self?.onDrag?(delta) }
        root.addSubview(header)

        header.wantsLayer = true
        header.layer?.backgroundColor = NSColor(white: 0, alpha: 0.78).cgColor
        header.alphaValue = expanded ? 0 : 1
        icon.frame = NSRect(x: 8, y: 9, width: 18, height: 18)
        icon.imageScaling = .scaleProportionallyUpOrDown
        header.addSubview(icon)
        busyDot.frame = CGRect(x: 14, y: 0, width: 6, height: 6)
        busyDot.cornerRadius = 3
        busyDot.borderWidth = 1.5
        busyDot.borderColor = NSColor.white.cgColor
        icon.wantsLayer = true
        icon.layer?.masksToBounds = false
        icon.layer?.addSublayer(busyDot)

        titleLabel.font = .systemFont(ofSize: 11, weight: .semibold)
        titleLabel.textColor = .labelColor
        titleLabel.lineBreakMode = .byTruncatingTail
        titleLabel.frame = NSRect(x: 32, y: 3, width: Mini.width - 32 - 60, height: 15)
        header.addSubview(titleLabel)

        subtitle.font = .systemFont(ofSize: 10, weight: .regular)
        subtitle.textColor = .secondaryLabelColor
        subtitle.lineBreakMode = .byTruncatingTail
        subtitle.frame = NSRect(x: 32, y: 18, width: Mini.width - 32 - 60, height: 14)
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
            button.target = target
            button.action = action
            header.addSubview(button)
        }
        stopButton.layer?.backgroundColor = NSColor.labelColor.withAlphaComponent(0.09).cgColor
        stopButton.frame = NSRect(x: Mini.width - 54, y: 7, width: 22, height: 22)
        toggleButton.frame = NSRect(x: Mini.width - 28, y: 7, width: 22, height: 22)
        target.mini = self

        preview.onClick = { [weak self] in self?.open() }
        preview.onDrag = { [weak self] delta in self?.onDrag?(delta) }
        preview.wantsLayer = true
        preview.layer?.cornerRadius = 3
        preview.layer?.masksToBounds = true
        preview.layer?.backgroundColor = NSColor.labelColor.withAlphaComponent(0.06).cgColor
        preview.toolTip = strings.open
        imageLayer.contentsGravity = .resizeAspect
        imageLayer.minificationFilter = .trilinear
        preview.layer?.addSublayer(imageLayer)
        buildCursor()
        preview.layer?.addSublayer(cursor)
        root.addSubview(preview)
        root.addSubview(header, positioned: .above, relativeTo: preview)
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
        let w = Mini.width
        return max(120, min(236, w * aspect.height / max(1, aspect.width)))
    }

    private var showsPreview: Bool { expanded && source != nil }

    var desiredHeight: CGFloat { showsPreview ? previewHeight : Mini.headerHeight }

    /// The stack owns position and size; the card only lays its content out.
    func applyFrame(_ rect: NSRect, animated: Bool = false) {
        if panel.frame != rect { panel.setFrame(rect, display: true, animate: animated && visible) }
        layoutContent()
    }

    private func layoutContent() {
        preview.isHidden = !showsPreview
        preview.frame = NSRect(x: 0, y: 0, width: Mini.width, height: previewHeight)
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        imageLayer.frame = preview.bounds
        CATransaction.commit()
        placeCursor()
        toggleButton.image = NSImage(systemSymbolName: expanded ? "chevron.down" : "chevron.up", accessibilityDescription: strings.collapse)?
            .withSymbolConfiguration(.init(pointSize: 11, weight: .semibold))
        toggleButton.isHidden = source == nil
    }

    // MARK: public API

    func show(title: String, iconImage: NSImage?, source: Source?, aspect: CGSize? = nil) {
        let changed = source != self.source
        self.source = source
        if let aspect, aspect.width > 0, aspect.height > 0 { self.aspect = aspect }
        if case .window(_, let frame)? = source, aspect == nil { self.aspect = frame.size }
        if changed { imageLayer.contents = nil; cursorPoint = nil; cursor.opacity = 0 }
        if changed { stopLive() }
        titleLabel.stringValue = title
        icon.image = iconImage ?? NSImage(systemSymbolName: "globe", accessibilityDescription: nil)
        let wasVisible = visible
        wantsVisible = true
        onResize?()
        if !wasVisible {
            applyVisibility()
            if let layer = card.layer {
                let pop = CASpringAnimation(keyPath: "transform.scale")
                pop.fromValue = 0.96
                pop.toValue = 1
                pop.damping = 20
                pop.initialVelocity = 2
                pop.duration = pop.settlingDuration
                layer.add(pop, forKey: "pop")
            }
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
        if relayout { onResize?() }
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
        guard wantsVisible else { return }
        wantsVisible = false
        source = nil
        let gone = { [weak self] in
            MainActor.assumeIsolated {
                guard let self, !self.wantsVisible else { return }
                self.panel.orderOut(nil)
                self.panel.alphaValue = 0
                self.onHidden?()
            }
        }
        if suppressed {
            gone()
        } else {
            NSAnimationContext.runAnimationGroup({ $0.duration = 0.18; panel.animator().alphaValue = 0 },
                                                 completionHandler: { gone() })
        }
    }

    /// Drops the panel for good; `MiniCenter` calls this once the card is gone.
    func teardown() {
        doneWork?.cancel()
        hideWork?.cancel()
        motion?.invalidate()
        stopLive()
        wantsVisible = false
        panel.orderOut(nil)
        panel.close()
    }

    /// Debug aid: let our own screenshots include the panel.
    func setShareable(_ on: Bool) { panel.sharingType = on ? .readOnly : .none }

    fileprivate func stopPressed() { onStop?() }

    fileprivate func toggle() {
        expanded.toggle()
        header.alphaValue = expanded ? 0 : 1
        UserDefaults.standard.set(expanded, forKey: "miniExpanded")
        onResize?()
        if !occluded { startLive() }
    }

    private func open() { onOpen?() }

    // MARK: visibility

    /// Orders the card in or out without touching its state. The controller
    /// calls this when the stack re-flows and when DSH gains or loses focus.
    func applyVisibility() {
        if visible {
            panel.orderFrontRegardless()
            if panel.alphaValue < 1 {
                NSAnimationContext.runAnimationGroup { $0.duration = 0.18; panel.animator().alphaValue = 1 }
            }
            if !occluded { startLive() }
        } else {
            // The capture belongs to the operation, not to whether its viewer
            // is in front. Keep macOS's native sharing indicator on the target.
            panel.orderOut(nil)
            panel.alphaValue = 0
        }
    }

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
        guard let source, source != .frames, !occluded, wantsVisible, !capturing else { return }
        capturing = true
        Task { @MainActor [weak self] in
            guard let self else { return }
            do { try await self.liveCapture.start(source: source) }
            catch { self.capturing = false }
        }
    }

    private func stopLive() {
        capturing = false
        liveCapture.stop()
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
    weak var mini: Mini?
    @objc func stop() { mini?.stopPressed() }
    @objc func toggle() { mini?.toggle() }
}
