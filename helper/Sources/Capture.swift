import AppKit
import ScreenCaptureKit

/// Maps screenshot pixel coordinates back to global display points
/// (CoreGraphics space: origin at the top-left of the main display, y down).
struct ShotMapping {
    let displayID: CGDirectDisplayID
    let bounds: CGRect      // display bounds in global points
    let imageSize: CGSize   // pixels of the image the model saw

    func toGlobal(_ x: Double, _ y: Double) -> CGPoint {
        CGPoint(x: bounds.minX + CGFloat(x) * bounds.width / imageSize.width,
                y: bounds.minY + CGFloat(y) * bounds.height / imageSize.height)
    }

    func toImage(_ p: CGPoint) -> CGPoint {
        CGPoint(x: (p.x - bounds.minX) * imageSize.width / bounds.width,
                y: (p.y - bounds.minY) * imageSize.height / bounds.height)
    }
}

enum CaptureError: LocalizedError {
    case noPermission, noDisplay
    var errorDescription: String? {
        switch self {
        case .noPermission: return "Screen Recording permission missing for DSH Computer Use"
        case .noDisplay: return "no display available"
        }
    }
}

final class Capture {
    private(set) var mapping: ShotMapping?

    /// Mapping used before the first screenshot: the display under the
    /// cursor, scaled like a screenshot with the given max width.
    func defaultMapping(maxWidth: Int, cursor: CGPoint) -> ShotMapping {
        let id = Capture.display(containing: cursor)
        let bounds = CGDisplayBounds(id)
        let scale = min(1, CGFloat(maxWidth) / bounds.width)
        return ShotMapping(displayID: id, bounds: bounds,
                           imageSize: CGSize(width: (bounds.width * scale).rounded(), height: (bounds.height * scale).rounded()))
    }

    static func display(containing p: CGPoint) -> CGDirectDisplayID {
        var ids = [CGDirectDisplayID](repeating: 0, count: 16)
        var count: UInt32 = 0
        CGGetDisplaysWithPoint(p, 16, &ids, &count)
        return count > 0 ? ids[0] : CGMainDisplayID()
    }

    func screenshot(displayID requested: CGDirectDisplayID?, maxWidth: Int, jpegQuality: Double,
                    cursor: CGPoint, includeOverlay: Bool = false) async throws -> [String: Any] {
        guard CGPreflightScreenCaptureAccess() else {
            CGRequestScreenCaptureAccess()
            throw CaptureError.noPermission
        }
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        let wanted = requested ?? mapping?.displayID ?? Capture.display(containing: cursor)
        guard let display = content.displays.first(where: { $0.displayID == wanted }) ?? content.displays.first else {
            throw CaptureError.noDisplay
        }
        // Never photograph our own overlay (agent cursor, banner).
        let own = includeOverlay ? [] : content.applications.filter { $0.processID == ProcessInfo.processInfo.processIdentifier }
        let filter = SCContentFilter(display: display, excludingApplications: own, exceptingWindows: [])

        let bounds = CGDisplayBounds(display.displayID)
        let scale = min(1, CGFloat(maxWidth) / bounds.width)
        let config = SCStreamConfiguration()
        config.width = Int((bounds.width * scale).rounded())
        config.height = Int((bounds.height * scale).rounded())
        config.showsCursor = false
        config.captureResolution = .best

        let image = try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: config)
        let rep = NSBitmapImageRep(cgImage: image)
        let data: Data?
        let mime: String
        if jpegQuality > 0 {
            data = rep.representation(using: .jpeg, properties: [.compressionFactor: jpegQuality])
            mime = "image/jpeg"
        } else {
            data = rep.representation(using: .png, properties: [:])
            mime = "image/png"
        }
        let size = CGSize(width: image.width, height: image.height)
        if !includeOverlay { mapping = ShotMapping(displayID: display.displayID, bounds: bounds, imageSize: size) }

        let displays = content.displays.map { d -> [String: Any] in
            let b = CGDisplayBounds(d.displayID)
            return ["id": d.displayID, "x": b.minX, "y": b.minY, "width": b.width, "height": b.height,
                    "main": CGDisplayIsMain(d.displayID) != 0]
        }
        let front = NSWorkspace.shared.frontmostApplication
        return [
            "ok": true,
            "mime": mime,
            "data": data?.base64EncodedString() ?? "",
            "width": image.width,
            "height": image.height,
            "display": display.displayID,
            "displays": displays,
            "frontmostApp": front?.localizedName ?? "",
        ]
    }
}
