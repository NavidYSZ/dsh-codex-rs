import Darwin
import Foundation

/// Thread-safe byte buffer for pipe readers.
final class DataBox: @unchecked Sendable {
    private let lock = NSLock()
    private var data = Data()
    func append(_ chunk: Data) { lock.lock(); data.append(chunk); lock.unlock() }
    var value: Data { lock.lock(); defer { lock.unlock() }; return data }
}

struct CommandResult {
    var status: Int32
    var stdout: Data
    var stderr: Data
    var timedOut: Bool
    var stderrText: String { String(decoding: stderr, as: UTF8.self) }
}

/// Runs short-lived helper commands (ssh, the login shell). Blocks, so call it
/// off the main thread. Arguments and output are never logged: they can hold tokens.
enum Command {
    static func run(
        _ executable: String, _ arguments: [String],
        environment: [String: String]? = nil, timeout: TimeInterval
    ) -> CommandResult {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = arguments
        if let environment { process.environment = environment }
        let out = Pipe(), err = Pipe()
        process.standardOutput = out
        process.standardError = err
        process.standardInput = FileHandle.nullDevice
        let outBox = DataBox(), errBox = DataBox()
        let readers = DispatchGroup()
        for (pipe, box) in [(out, outBox), (err, errBox)] {
            readers.enter()
            pipe.fileHandleForReading.readabilityHandler = { handle in
                let chunk = handle.availableData
                if chunk.isEmpty {
                    handle.readabilityHandler = nil
                    readers.leave()
                } else {
                    box.append(chunk)
                }
            }
        }
        let done = DispatchSemaphore(value: 0)
        process.terminationHandler = { _ in done.signal() }
        do {
            try process.run()
        } catch {
            for pipe in [out, err] { pipe.fileHandleForReading.readabilityHandler = nil }
            return CommandResult(status: -1, stdout: Data(), stderr: Data(error.localizedDescription.utf8), timedOut: false)
        }
        var timedOut = false
        if done.wait(timeout: .now() + timeout) == .timedOut {
            timedOut = true
            process.terminate()
            if done.wait(timeout: .now() + 2) == .timedOut {
                kill(process.processIdentifier, SIGKILL)
                done.wait()
            }
        }
        // Background jobs of a login shell may keep the pipes open; do not wait for them.
        if readers.wait(timeout: .now() + 2) == .timedOut {
            for pipe in [out, err] { pipe.fileHandleForReading.readabilityHandler = nil }
        }
        return CommandResult(status: process.terminationStatus, stdout: outBox.value, stderr: errBox.value, timedOut: timedOut)
    }
}

/// The user's login-shell environment. Apps started from the Finder only get
/// `/usr/bin:/bin:/usr/sbin:/sbin`; the agent's tools need the real PATH.
enum ShellEnvironment {
    private static let lock = NSLock()
    nonisolated(unsafe) private static var cached: [String: String]?

    /// Resolves once (up to ~10 s on the first call), then returns the cached value.
    static func resolved() -> [String: String] {
        lock.lock()
        defer { lock.unlock() }
        if let cached { return cached }
        let env = resolve()
        cached = env
        return env
    }

    private static let skipped: Set<String> = [
        "PWD", "OLDPWD", "SHLVL", "_", "TERM", "TERM_PROGRAM", "TERM_PROGRAM_VERSION",
        "TERM_SESSION_ID", "ITERM_SESSION_ID", "COLORTERM", "PS1", "PROMPT",
    ]

    private static func resolve() -> [String: String] {
        var env = ProcessInfo.processInfo.environment
        let shell = env["SHELL"].flatMap { $0.isEmpty ? nil : $0 } ?? "/bin/zsh"
        let marker = "__DSHX_ENV_\(UUID().uuidString.prefix(8))__"
        let script = "printf '%s' '\(marker)'; /usr/bin/env -0; printf '%s' '\(marker)'"
        for flags in [["-i", "-l", "-c"], ["-l", "-c"]] {
            let result = Command.run(shell, flags + [script], timeout: 8)
            guard !result.timedOut else { continue }
            let parts = String(decoding: result.stdout, as: UTF8.self).components(separatedBy: marker)
            guard parts.count >= 3 else { continue }
            var found = 0
            for entry in parts[1].split(separator: "\0") {
                guard let eq = entry.firstIndex(of: "=") else { continue }
                let key = String(entry[..<eq])
                if key.isEmpty || skipped.contains(key) { continue }
                env[key] = String(entry[entry.index(after: eq)...])
                found += 1
            }
            if found > 0 { return env }
        }
        // Fallback: the usual tool locations.
        let home = NSHomeDirectory()
        var path = (env["PATH"] ?? "/usr/bin:/bin:/usr/sbin:/sbin").split(separator: ":").map(String.init)
        for dir in ["/opt/homebrew/bin", "/usr/local/bin", "\(home)/.cargo/bin", "\(home)/.local/bin"].reversed()
        where !path.contains(dir) {
            path.insert(dir, at: 0)
        }
        env["PATH"] = path.joined(separator: ":")
        return env
    }
}

/// Appends a child's output to a log file with tokens masked, and keeps the last lines for error messages.
final class LogSink: @unchecked Sendable {
    private let queue = DispatchQueue(label: "dsh-codex.log")
    private let handle: FileHandle?
    private var pending = Data()
    private var tail: [String] = []

    init(name: String) {
        Paths.ensurePrivateDirectory(Paths.logs)
        let url = Paths.logs.appendingPathComponent(name)
        // Keep one previous generation once the file passes 5 MB.
        if let size = (try? FileManager.default.attributesOfItem(atPath: url.path))?[.size] as? Int, size > 5_000_000 {
            let old = url.appendingPathExtension("1")
            try? FileManager.default.removeItem(at: old)
            try? FileManager.default.moveItem(at: url, to: old)
        }
        if !FileManager.default.fileExists(atPath: url.path) {
            FileManager.default.createFile(atPath: url.path, contents: nil, attributes: [.posixPermissions: 0o600])
        }
        handle = try? FileHandle(forWritingTo: url)
        _ = try? handle?.seekToEnd()
        handle?.write(Data("--- \(ISO8601DateFormatter().string(from: Date())) ---\n".utf8))
    }

    deinit { try? handle?.close() }

    func attach(_ pipe: Pipe) {
        pipe.fileHandleForReading.readabilityHandler = { [weak self] handle in
            let chunk = handle.availableData
            if chunk.isEmpty {
                handle.readabilityHandler = nil
                return
            }
            self?.queue.async { self?.consume(chunk) }
        }
    }

    /// The last non-empty lines (already masked).
    var lastLines: [String] { queue.sync { tail } }

    func note(_ line: String) { queue.async { self.write(line) } }

    /// Waits for pending writes (before the app quits).
    func flush() { queue.sync {} }

    private func consume(_ chunk: Data) {
        pending.append(chunk)
        while let newline = pending.firstIndex(of: 0x0A) {
            let line = String(decoding: pending[pending.startIndex..<newline], as: UTF8.self)
            pending.removeSubrange(pending.startIndex...newline)
            write(line)
        }
    }

    private func write(_ raw: String) {
        let line = Redact.secrets(raw)
        if !line.trimmingCharacters(in: .whitespaces).isEmpty {
            tail.append(line)
            if tail.count > 20 { tail.removeFirst(tail.count - 20) }
        }
        handle?.write(Data((LogSink.clock.string(from: Date()) + " " + line + "\n").utf8))
    }

    private static let clock: DateFormatter = {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "HH:mm:ss"
        return formatter
    }()
}

/// The shell's own log (`~/Library/Logs/dsh-codex/desktop.log`); tokens are masked.
let appLog = LogSink(name: "desktop.log")

enum Redact {
    private static let patterns: [NSRegularExpression] = [
        try! NSRegularExpression(pattern: "(token=)[^&\\s\"'<>]+", options: [.caseInsensitive]),
        try! NSRegularExpression(pattern: "(Bearer )[^\\s\"'<>]+", options: []),
        try! NSRegularExpression(pattern: "(\"token\"\\s*:\\s*\")[^\"]+", options: []),
    ]

    static func secrets(_ text: String) -> String {
        var out = text
        for re in patterns {
            out = re.stringByReplacingMatches(
                in: out, range: NSRange(out.startIndex..., in: out), withTemplate: "$1…")
        }
        return out
    }
}

/// Pulls the access token out of a start link (`…/?token=…`) or a bare token.
enum TokenParser {
    private static let allowed = CharacterSet(charactersIn:
        "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~")

    static func extract(_ output: String) -> String? {
        for line in output.split(whereSeparator: \.isNewline).reversed() {
            guard let range = line.range(of: "token=") else { continue }
            let token = String(line[range.upperBound...].unicodeScalars.prefix { allowed.contains($0) })
            if !token.isEmpty { return token }
        }
        let trimmed = output.trimmingCharacters(in: .whitespacesAndNewlines)
        if !trimmed.isEmpty, trimmed.count <= 512,
           trimmed.unicodeScalars.allSatisfy({ allowed.contains($0) }) {
            return trimmed
        }
        return nil
    }
}

enum Net {
    /// True when something accepts TCP connections on 127.0.0.1:`port`.
    static func isListening(port: Int) -> Bool {
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        guard fd >= 0 else { return false }
        defer { close(fd) }
        var addr = sockaddr_in()
        addr.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_port = in_port_t(UInt16(truncatingIfNeeded: port)).bigEndian
        addr.sin_addr.s_addr = inet_addr("127.0.0.1")
        return withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                connect(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) == 0
            }
        }
    }

    enum Probe: Equatable {
        /// `/api/info` answered 200 with this token.
        case ok
        /// A dsh-codex server that rejects the token (or none was sent).
        case unauthorized
        /// Something answered, but not like dsh-codex.
        case foreign(Int)
        case unreachable(String)
    }

    private static let session: URLSession = {
        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = 8
        config.timeoutIntervalForResource = 10
        config.requestCachePolicy = .reloadIgnoringLocalCacheData
        config.connectionProxyDictionary = [:]
        return URLSession(configuration: config)
    }()

    /// GET `<origin>/api/info`. Blocks; call off the main thread.
    static func probe(origin: URL, token: String?) -> Probe {
        guard let url = URL(string: "/api/info", relativeTo: origin) else { return .unreachable("ungültige URL") }
        var request = URLRequest(url: url)
        if let token { request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }
        let done = DispatchSemaphore(value: 0)
        let result = ProbeBox()
        session.dataTask(with: request) { data, response, error in
            if let error {
                result.value = .unreachable(error.localizedDescription)
            } else if let http = response as? HTTPURLResponse {
                switch http.statusCode {
                case 200:
                    result.value = .ok
                case 401:
                    let body = data.flatMap { try? JSONSerialization.jsonObject(with: $0) } as? [String: Any]
                    result.value = body?["error"] as? String == "unauthorized" ? .unauthorized : .foreign(401)
                default:
                    result.value = .foreign(http.statusCode)
                }
            }
            done.signal()
        }.resume()
        done.wait()
        return result.value
    }

    private final class ProbeBox: @unchecked Sendable { var value: Probe = .unreachable("keine Antwort") }

    /// `http://host:port/` of a page URL (drops path, query and token).
    static func origin(of url: URL) -> URL? {
        var parts = URLComponents()
        parts.scheme = url.scheme
        parts.host = url.host
        parts.port = url.port
        parts.path = "/"
        return parts.url
    }
}

/// Process helpers for stale local servers left behind by a crash.
enum Processes {
    static func executablePath(of pid: pid_t) -> String? {
        var buffer = [CChar](repeating: 0, count: 4 * Int(MAXPATHLEN))
        let length = proc_pidpath(pid, &buffer, UInt32(buffer.count))
        guard length > 0 else { return nil }
        return String(cString: buffer)
    }

    /// Terminates the process in `pidFile` if it still runs `binary`.
    static func terminateStale(pidFile: URL, binary: String) {
        defer { try? FileManager.default.removeItem(at: pidFile) }
        guard let text = try? String(contentsOf: pidFile, encoding: .utf8),
              let pid = pid_t(text.trimmingCharacters(in: .whitespacesAndNewlines)), pid > 1,
              kill(pid, 0) == 0,
              let path = executablePath(of: pid),
              URL(fileURLWithPath: path).resolvingSymlinksInPath().path
                == URL(fileURLWithPath: binary).resolvingSymlinksInPath().path
        else { return }
        kill(pid, SIGTERM)
        for _ in 0..<30 {
            if kill(pid, 0) != 0 { return }
            usleep(100_000)
        }
        kill(pid, SIGKILL)
    }
}
