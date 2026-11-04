import AVFoundation
import SwiftUI
import UIKit
import VisionKit

enum QRScanner {
    /// A camera that can scan: VisionKit's scanner, else AVFoundation; the simulator has neither.
    static var isSupported: Bool {
        #if targetEnvironment(simulator)
        return false
        #else
        return DataScannerViewController.isSupported || AVCaptureDevice.default(for: .video) != nil
        #endif
    }
}

/// Scans QR codes with VisionKit's `DataScannerViewController`, falling back to an `AVCaptureSession`.
struct QRScannerView: UIViewControllerRepresentable {
    var onCode: (String) -> Void

    func makeCoordinator() -> Coordinator { Coordinator(onCode: onCode) }

    func makeUIViewController(context: Context) -> UIViewController {
        if DataScannerViewController.isSupported, DataScannerViewController.isAvailable {
            let scanner = DataScannerViewController(recognizedDataTypes: [.barcode(symbologies: [.qr])], qualityLevel: .balanced,
                                                    recognizesMultipleItems: false, isHighFrameRateTrackingEnabled: false,
                                                    isHighlightingEnabled: true)
            scanner.delegate = context.coordinator
            context.coordinator.scanner = scanner
            try? scanner.startScanning()
            return scanner
        }
        let fallback = CaptureScannerController()
        fallback.onCode = context.coordinator.deliver
        return fallback
    }

    func updateUIViewController(_ uiViewController: UIViewController, context: Context) {}

    static func dismantleUIViewController(_ uiViewController: UIViewController, coordinator: Coordinator) {
        coordinator.scanner?.stopScanning()
    }

    @MainActor
    final class Coordinator: NSObject, DataScannerViewControllerDelegate {
        let onCode: (String) -> Void
        weak var scanner: DataScannerViewController?
        private var delivered = false

        init(onCode: @escaping (String) -> Void) { self.onCode = onCode }

        func deliver(_ text: String) {
            guard !delivered else { return }
            delivered = true
            scanner?.stopScanning()
            onCode(text)
            // Allow another scan after a moment (a wrong code should not lock the sheet).
            Task { @MainActor in
                try? await Task.sleep(for: .seconds(2))
                delivered = false
                try? scanner?.startScanning()
            }
        }

        func dataScanner(_ dataScanner: DataScannerViewController, didAdd addedItems: [RecognizedItem], allItems: [RecognizedItem]) {
            for item in addedItems {
                if case .barcode(let barcode) = item, let text = barcode.payloadStringValue { deliver(text) }
            }
        }
    }
}

/// AVFoundation fallback for devices without VisionKit's scanner.
final class CaptureScannerController: UIViewController, AVCaptureMetadataOutputObjectsDelegate {
    var onCode: ((String) -> Void)?
    private let session = AVCaptureSession()
    private var preview: AVCaptureVideoPreviewLayer?
    private let label = UILabel()

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .black
        label.text = L.addCameraDenied
        label.textColor = .white
        label.numberOfLines = 0
        label.textAlignment = .center
        label.font = .preferredFont(forTextStyle: .footnote)
        label.isHidden = true
        label.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(label)
        NSLayoutConstraint.activate([
            label.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 16),
            label.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -16),
            label.centerYAnchor.constraint(equalTo: view.centerYAnchor),
        ])
        Task { await self.start() }
    }

    private func start() async {
        let granted = await AVCaptureDevice.requestAccess(for: .video)
        guard granted, let device = AVCaptureDevice.default(for: .video), let input = try? AVCaptureDeviceInput(device: device) else {
            label.isHidden = false
            return
        }
        let output = AVCaptureMetadataOutput()
        session.beginConfiguration()
        if session.canAddInput(input) { session.addInput(input) }
        if session.canAddOutput(output) {
            session.addOutput(output)
            output.setMetadataObjectsDelegate(self, queue: .main)
            output.metadataObjectTypes = [.qr]
        }
        session.commitConfiguration()
        let layer = AVCaptureVideoPreviewLayer(session: session)
        layer.videoGravity = .resizeAspectFill
        layer.frame = view.bounds
        view.layer.insertSublayer(layer, at: 0)
        preview = layer
        // AVCaptureSession is not Sendable; start/stop are documented as blocking calls meant for a background queue.
        nonisolated(unsafe) let s = session
        DispatchQueue.global(qos: .userInitiated).async { s.startRunning() }
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        preview?.frame = view.bounds
    }

    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        nonisolated(unsafe) let s = session
        DispatchQueue.global(qos: .userInitiated).async { s.stopRunning() }
    }

    nonisolated func metadataOutput(_ output: AVCaptureMetadataOutput, didOutput metadataObjects: [AVMetadataObject], from connection: AVCaptureConnection) {
        let codes = metadataObjects.compactMap { ($0 as? AVMetadataMachineReadableCodeObject)?.stringValue }
        guard let first = codes.first else { return }
        MainActor.assumeIsolated { onCode?(first) }
    }
}
