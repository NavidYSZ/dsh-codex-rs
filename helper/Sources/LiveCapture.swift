import AppKit
import ScreenCaptureKit
import CoreMedia
import CoreImage

/// A real ScreenCaptureKit stream, rather than repeated still screenshots.
/// macOS owns the purple recording indicator and its Stop Sharing control.
@MainActor
final class LiveCapture: NSObject, SCStreamOutput, SCStreamDelegate {
    private var stream: SCStream?
    private var generation = 0
    private let queue = DispatchQueue(label: "dsh-cua.live-capture", qos: .userInitiated)
    nonisolated private let context = CIContext(options: [.cacheIntermediates: false])
    var onFrame: ((CGImage) -> Void)?
    var onStopped: (() -> Void)?

    func start(source: Mini.Source) async throws {
        stop()
        let ticket = generation
        guard CGPreflightScreenCaptureAccess() else { throw CaptureError.noPermission }
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false)
        let filter: SCContentFilter
        let size: CGSize
        switch source {
        case .window(let id, let frame):
            guard let window = content.windows.first(where: { $0.windowID == id }) else { throw AppError.noWindow("window \(id)") }
            filter = SCContentFilter(desktopIndependentWindow: window)
            size = frame.size
        case .display(let id, let frame):
            guard let display = content.displays.first(where: { $0.displayID == id }) else { throw CaptureError.noDisplay }
            let own = content.applications.filter { $0.processID == ProcessInfo.processInfo.processIdentifier }
            filter = SCContentFilter(display: display, excludingApplications: own, exceptingWindows: [])
            size = frame.size
        case .frames: return
        }
        guard ticket == generation else { return }
        let config = SCStreamConfiguration()
        let scale = min(1, 680 / max(1, size.width))
        config.width = max(2, Int((size.width * scale).rounded()))
        config.height = max(2, Int((size.height * scale).rounded()))
        config.showsCursor = false
        config.capturesAudio = false
        config.queueDepth = 3
        config.minimumFrameInterval = CMTime(value: 1, timescale: 4)
        config.pixelFormat = kCVPixelFormatType_32BGRA
        let next = SCStream(filter: filter, configuration: config, delegate: self)
        try next.addStreamOutput(self, type: .screen, sampleHandlerQueue: queue)
        stream = next
        do { try await next.startCapture() }
        catch { if stream === next { stream = nil }; throw error }
        if ticket != generation { try? await next.stopCapture() }
    }

    func stop() {
        generation += 1
        let previous = stream
        stream = nil
        if let previous { Task { try? await previous.stopCapture() } }
    }

    nonisolated func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, sampleBuffer.isValid,
              let buffer = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
        let image = CIImage(cvPixelBuffer: buffer)
        guard let cg = context.createCGImage(image, from: image.extent) else { return }
        Task { @MainActor [weak self] in
            guard let self, self.stream === stream else { return }
            self.onFrame?(cg)
        }
    }

    nonisolated func stream(_ stream: SCStream, didStopWithError error: Error) {
        Task { @MainActor [weak self] in
            guard let self, self.stream === stream else { return }
            self.stream = nil
            self.onStopped?()
        }
    }
}
