import Foundation

/// Newline-delimited JSON over a Unix domain socket. One request per
/// connection: the client writes one JSON line, the helper answers with one
/// JSON line and closes. The socket file is created with mode 0600.
final class SocketServer {
    typealias Handler = ([String: Any]) async -> [String: Any]

    let path: String
    private let handler: Handler
    private var listenFD: Int32 = -1

    init(path: String, handler: @escaping Handler) {
        self.path = path
        self.handler = handler
    }

    func start() throws {
        let dir = (path as NSString).deletingLastPathComponent
        try FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true,
                                                attributes: [.posixPermissions: 0o700])
        unlink(path)

        listenFD = socket(AF_UNIX, SOCK_STREAM, 0)
        guard listenFD >= 0 else { throw posixError("socket") }

        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let bytes = Array(path.utf8)
        guard bytes.count < MemoryLayout.size(ofValue: addr.sun_path) else {
            throw NSError(domain: "dsh-cua", code: 1, userInfo: [NSLocalizedDescriptionKey: "socket path too long"])
        }
        withUnsafeMutableBytes(of: &addr.sun_path) { raw in
            raw.copyBytes(from: bytes)
            raw[bytes.count] = 0
        }
        let len = socklen_t(MemoryLayout<sockaddr_un>.size)
        let bound = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(listenFD, $0, len) }
        }
        guard bound == 0 else { throw posixError("bind") }
        chmod(path, 0o600)
        guard listen(listenFD, 16) == 0 else { throw posixError("listen") }

        let fd = listenFD
        Thread.detachNewThread { [weak self] in
            while true {
                let client = accept(fd, nil, nil)
                if client < 0 { if errno == EINTR { continue }; break }
                Thread.detachNewThread { self?.serve(client) }
            }
        }
    }

    func stop() {
        if listenFD >= 0 { close(listenFD) }
        unlink(path)
    }

    private func serve(_ fd: Int32) {
        var buffer = Data()
        var chunk = [UInt8](repeating: 0, count: 65536)
        while !buffer.contains(0x0A) {
            let n = read(fd, &chunk, chunk.count)
            if n <= 0 { break }
            buffer.append(chunk, count: n)
        }
        guard let newline = buffer.firstIndex(of: 0x0A) else { close(fd); return }
        let line = buffer[buffer.startIndex..<newline]

        let request = (try? JSONSerialization.jsonObject(with: line)) as? [String: Any]
        let semaphore = DispatchSemaphore(value: 0)
        var response: [String: Any] = ["ok": false, "error": "invalid JSON request"]
        if let request {
            Task { @MainActor in
                response = await self.handler(request)
                if let id = request["id"] { response["id"] = id }
                semaphore.signal()
            }
            semaphore.wait()
        }

        var out = (try? JSONSerialization.data(withJSONObject: response)) ?? Data("{\"ok\":false}".utf8)
        out.append(0x0A)
        out.withUnsafeBytes { raw in
            var offset = 0
            while offset < raw.count {
                let n = write(fd, raw.baseAddress!.advanced(by: offset), raw.count - offset)
                if n <= 0 { break }
                offset += n
            }
        }
        close(fd)
    }

    private func posixError(_ what: String) -> NSError {
        NSError(domain: NSPOSIXErrorDomain, code: Int(errno),
                userInfo: [NSLocalizedDescriptionKey: "\(what): \(String(cString: strerror(errno)))"])
    }
}
