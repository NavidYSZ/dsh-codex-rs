import AppKit
import Foundation

enum ConnectionStatus: String {
    case idle, starting, ready, error
}

/// Brings one instance's UI within reach and reports where it lives. The page
/// URL carries the access token: keep it in memory, never log or print it.
@MainActor
class Connection {
    let instance: Instance
    private(set) var status: ConnectionStatus = .idle
    private(set) var message = ""
    /// The page to load (`http://127.0.0.1:<port>/?token=…`) once `status == .ready`.
    private(set) var pageURL: URL?
    var onChange: (() -> Void)?

    init(instance: Instance) { self.instance = instance }

    func start() {}
    func stop(_ done: @escaping () -> Void) { done() }

    /// Tears the connection down and builds it again.
    func reconnect() { stop { [weak self] in self?.start() } }

    func update(_ status: ConnectionStatus, _ message: String = "", url: URL? = nil) {
        if status != self.status || message != self.message {
            appLog.note("\(instance.id): \(status.rawValue) \(message)")
        }
        self.status = status
        self.message = message
        if status == .ready { pageURL = url } else if status == .idle { pageURL = nil }
        onChange?()
    }

    static func make(_ instance: Instance) -> Connection {
        switch instance.kind {
        case .local: return LocalConnection(instance: instance)
        case .ssh: return SSHConnection(instance: instance)
        case .url: return URLConnection(instance: instance)
        }
    }

    /// File-name-safe form of the instance id.
    var slug: String {
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "-_"))
        let cleaned = String(instance.id.unicodeScalars.map { allowed.contains($0) ? Character($0) : "_" })
        return cleaned.isEmpty ? "instance" : String(cleaned.prefix(40))
    }
}

/// Runs the bundled `dsh-codex web --no-open --port 0 --ready-file …` and
/// restarts it when it dies unexpectedly.
@MainActor
final class LocalConnection: Connection {
    private var process: Process?
    private var log: LogSink?
    private var stopping = false
    private var generation = 0
    private var failures = 0
    private var startedAt = Date()
    private var stopWaiters: [() -> Void] = []

    private var readyFile: URL { Paths.run.appendingPathComponent("local-\(slug).json") }
    /// The last port of this instance. Reusing it keeps the page's origin, and
    /// with it the browser storage (sidebar, theme, panel), across restarts.
    private var portKey: String { "localPort.\(slug)" }

    private func preferredPort() -> Int {
        let port = UserDefaults.standard.integer(forKey: portKey)
        return port > 0 && !Net.isListening(port: port) ? port : 0
    }
    private var pidFile: URL { Paths.run.appendingPathComponent("local-\(slug).pid") }

    var binaryPath: String? {
        if let custom = instance.binary, !custom.isEmpty { return (custom as NSString).expandingTildeInPath }
        return Bundle.main.path(forAuxiliaryExecutable: "dsh-codex")
    }

    override func start() {
        guard process == nil else { return }
        stopping = false
        generation += 1
        let gen = generation
        guard let binary = binaryPath, FileManager.default.isExecutableFile(atPath: binary) else {
            update(.error, "Das Programm dsh-codex wurde nicht gefunden (\(binaryPath ?? "nicht im App-Paket")).")
            return
        }
        update(.starting, "dsh-codex wird gestartet …")
        let pidFile = self.pidFile
        DispatchQueue.global(qos: .userInitiated).async {
            Paths.ensurePrivateDirectory(Paths.run)
            Processes.terminateStale(pidFile: pidFile, binary: binary)
            let env = ShellEnvironment.resolved()
            DispatchQueue.main.async {
                MainActor.assumeIsolated { self.launch(binary: binary, environment: env, generation: gen) }
            }
        }
    }

    private func launch(binary: String, environment: [String: String], generation gen: Int) {
        guard gen == generation, !stopping, process == nil else { return }
        try? FileManager.default.removeItem(at: readyFile)
        var env = environment
        env["DSHX_DESKTOP"] = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "1"
        if let data = instance.dataDirectory, !data.isEmpty {
            env["DSHX_HOME"] = (data as NSString).expandingTildeInPath
        }
        let cwd = instance.workingDirectory.flatMap { $0.isEmpty ? nil : ($0 as NSString).expandingTildeInPath }
            ?? NSHomeDirectory()

        let process = Process()
        process.executableURL = URL(fileURLWithPath: binary)
        process.arguments = ["web", "--no-open", "--port", String(preferredPort()), "--ready-file", readyFile.path]
        process.currentDirectoryURL = URL(fileURLWithPath: cwd, isDirectory: true)
        process.environment = env
        process.standardInput = FileHandle.nullDevice
        let output = Pipe()
        process.standardOutput = output
        process.standardError = output
        let log = LogSink(name: "local-\(slug).log")
        log.attach(output)
        self.log = log
        process.terminationHandler = { [weak self] proc in
            let status = proc.terminationStatus
            DispatchQueue.main.async {
                MainActor.assumeIsolated { self?.exited(proc, status: status) }
            }
        }
        do {
            try process.run()
        } catch {
            update(.error, "dsh-codex konnte nicht gestartet werden: \(error.localizedDescription)")
            return
        }
        self.process = process
        startedAt = Date()
        FileManager.default.createFile(
            atPath: pidFile.path, contents: Data("\(process.processIdentifier)\n".utf8),
            attributes: [.posixPermissions: 0o600])
        waitForReady(process, generation: gen, deadline: Date().addingTimeInterval(120))
    }

    private func waitForReady(_ process: Process, generation gen: Int, deadline: Date) {
        guard gen == generation, process === self.process, process.isRunning else { return }
        if let data = try? Data(contentsOf: readyFile),
           let info = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
           let raw = info["url"] as? String, let url = URL(string: raw) {
            if let port = info["port"] as? Int { UserDefaults.standard.set(port, forKey: portKey) }
            update(.ready, url: url)
            return
        }
        if Date() > deadline {
            update(.error, "dsh-codex hat sich nach zwei Minuten nicht gemeldet.")
            process.terminate()
            return
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.15) {
            MainActor.assumeIsolated { self.waitForReady(process, generation: gen, deadline: deadline) }
        }
    }

    private func exited(_ proc: Process, status: Int32) {
        guard proc === process else { return }
        process = nil
        try? FileManager.default.removeItem(at: pidFile)
        try? FileManager.default.removeItem(at: readyFile)
        if stopping {
            update(.idle)
            let waiters = stopWaiters
            stopWaiters = []
            waiters.forEach { $0() }
            return
        }
        if Date().timeIntervalSince(startedAt) > 60 { failures = 0 }
        failures += 1
        // The remembered port may be the reason (taken meanwhile): next start picks any.
        UserDefaults.standard.removeObject(forKey: portKey)
        let detail = log?.lastLines.last.map { "\n\($0)" } ?? ""
        log?.note("dsh-desktop: dsh-codex exited with status \(status)")
        if failures <= 3 {
            update(.starting, "dsh-codex wurde beendet (Status \(status)), Neustart …")
            let gen = generation
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.5 * Double(failures)) {
                MainActor.assumeIsolated {
                    guard gen == self.generation, !self.stopping else { return }
                    self.start()
                }
            }
        } else {
            update(.error, "dsh-codex wurde wiederholt beendet (Status \(status)).\(detail)")
        }
    }

    override func reconnect() {
        failures = 0
        super.reconnect()
    }

    override func stop(_ done: @escaping () -> Void) {
        stopping = true
        generation += 1
        guard let process, process.isRunning else {
            self.process = nil
            update(.idle)
            done()
            return
        }
        stopWaiters.append(done)
        process.terminate()
        let pid = process.processIdentifier
        DispatchQueue.main.asyncAfter(deadline: .now() + 6) {
            if process.isRunning { kill(pid, SIGKILL) }
        }
    }
}

/// `ssh -N -L 127.0.0.1:<local>:127.0.0.1:<remote>` as a supervised child with
/// its own ControlMaster socket; the token command reuses that connection.
@MainActor
final class SSHConnection: Connection {
    private var tunnel: Process?
    private var tunnelLog: LogSink?
    private var token: String?
    private var stopped = true
    private var generation = 0
    private var retries = 0
    private var health: Timer?
    /// The local port is forwarded by a tunnel we did not start (for example the .command starter).
    private var external = false
    private var stopWaiters: [() -> Void] = []

    private var config: String { ((instance.sshConfig ?? "") as NSString).expandingTildeInPath }
    private var host: String { instance.host ?? "" }
    private var localPort: Int { instance.localPort ?? 0 }
    private var remotePort: Int { instance.remotePort ?? 0 }
    private var origin: URL { URL(string: "http://127.0.0.1:\(localPort)/")! }

    private var socketPath: String {
        Paths.sockets.appendingPathComponent("\(String(slug.prefix(24))).sock").path
    }

    /// Options every ssh call shares. Host keys are checked as configured.
    private var baseArguments: [String] {
        var args: [String] = []
        if !config.isEmpty { args += ["-F", config] }
        return args + ["-o", "BatchMode=yes", "-o", "ConnectTimeout=15"]
    }

    override func start() {
        stopped = false
        generation += 1
        let gen = generation
        health?.invalidate()
        if let problem = validate() {
            update(.error, problem)
            return
        }
        update(.starting, "Verbindung zu \(host) wird aufgebaut …")
        let socket = socketPath, base = baseArguments, host = self.host, port = localPort
        DispatchQueue.global(qos: .userInitiated).async {
            Paths.ensurePrivateDirectory(Paths.sockets)
            // A master left behind by a crashed app would keep the port busy.
            if FileManager.default.fileExists(atPath: socket) {
                let check = Command.run("/usr/bin/ssh", base + ["-S", socket, "-O", "check", host], timeout: 5)
                if check.status == 0 {
                    _ = Command.run("/usr/bin/ssh", base + ["-S", socket, "-O", "exit", host], timeout: 5)
                    usleep(300_000)
                }
                try? FileManager.default.removeItem(atPath: socket)
            }
            let busy = Net.isListening(port: port)
            let probe = busy ? Net.probe(origin: URL(string: "http://127.0.0.1:\(port)/")!, token: nil) : nil
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    guard gen == self.generation, !self.stopped else { return }
                    switch probe {
                    case nil:
                        self.openTunnel(generation: gen)
                    case .unauthorized?, .ok?:
                        self.external = true
                        self.fetchToken(generation: gen)
                    case .foreign(let code)?:
                        self.update(.error, "Port \(port) ist belegt (HTTP \(code)), aber nicht von dsh-codex.")
                    case .unreachable(let why)?:
                        self.update(.error, "Port \(port) ist belegt und antwortet nicht: \(why)")
                    }
                }
            }
        }
    }

    private func validate() -> String? {
        if host.isEmpty { return "Kein SSH-Host angegeben." }
        if !config.isEmpty && !FileManager.default.fileExists(atPath: config) {
            return "SSH-Konfiguration nicht gefunden: \(config)"
        }
        if !(1...65535).contains(localPort) || !(1...65535).contains(remotePort) { return "Ungültiger Port." }
        if (instance.tokenCommand ?? "").trimmingCharacters(in: .whitespaces).isEmpty {
            return "Kein Token-Befehl angegeben."
        }
        return nil
    }

    private func openTunnel(generation gen: Int) {
        external = false
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/ssh")
        process.arguments = baseArguments + [
            "-o", "ExitOnForwardFailure=yes",
            "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=3",
            "-o", "ControlMaster=yes", "-o", "ControlPath=\(socketPath)", "-o", "ControlPersist=no",
            "-N", "-L", "127.0.0.1:\(localPort):127.0.0.1:\(remotePort)", host,
        ]
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = FileHandle.nullDevice
        let errors = Pipe()
        process.standardError = errors
        let log = LogSink(name: "ssh-\(slug).log")
        log.attach(errors)
        tunnelLog = log
        process.terminationHandler = { [weak self] proc in
            let status = proc.terminationStatus
            DispatchQueue.main.async {
                MainActor.assumeIsolated { self?.tunnelExited(proc, status: status) }
            }
        }
        do {
            try process.run()
        } catch {
            update(.error, "ssh konnte nicht gestartet werden: \(error.localizedDescription)")
            return
        }
        tunnel = process
        update(.starting, "SSH-Tunnel zu \(host) wird aufgebaut …")
        let port = localPort
        DispatchQueue.global(qos: .userInitiated).async {
            let deadline = Date().addingTimeInterval(30)
            var up = false
            while Date() < deadline && process.isRunning {
                if Net.isListening(port: port) { up = true; break }
                usleep(200_000)
            }
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    guard gen == self.generation, !self.stopped, process === self.tunnel else { return }
                    if up {
                        self.fetchToken(generation: gen)
                    } else if process.isRunning {
                        process.terminate()  // tunnelExited retries
                    }
                }
            }
        }
    }

    private func fetchToken(generation gen: Int) {
        update(.starting, "Zugangstoken wird geholt …")
        var args = baseArguments + ["-o", "ControlMaster=no"]
        if !external && tunnel != nil {
            args += ["-o", "ControlPath=\(socketPath)"]
        }
        args += [host, instance.tokenCommand ?? ""]
        let origin = self.origin
        DispatchQueue.global(qos: .userInitiated).async {
            let result = Command.run("/usr/bin/ssh", args, timeout: 45)
            let token = TokenParser.extract(String(decoding: result.stdout, as: UTF8.self))
            let probe = token.map { Net.probe(origin: origin, token: $0) }
            let errorLine = Redact.secrets(result.stderrText)
                .split(whereSeparator: \.isNewline).last.map(String.init) ?? ""
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    guard gen == self.generation, !self.stopped else { return }
                    guard let token, let probe else {
                        let why = result.timedOut ? "Zeitüberschreitung" : "Exit \(result.status)"
                        self.update(.error, "Der Token-Befehl lieferte keinen Startlink (\(why)). \(errorLine)")
                        self.scheduleHealth()
                        return
                    }
                    self.accept(token: token, probe: probe)
                }
            }
        }
    }

    private func accept(token: String, probe: Net.Probe) {
        switch probe {
        case .ok:
            self.token = token
            retries = 0
            update(.ready, url: pageURL(token: token))
        case .unauthorized:
            update(.error, "Der Server lehnt das geholte Token ab.")
        case .foreign(let code):
            update(.error, "Unerwartete Antwort durch den Tunnel (HTTP \(code)).")
        case .unreachable(let why):
            update(.error, "Durch den Tunnel nicht erreichbar: \(why)")
        }
        scheduleHealth()
    }

    private func pageURL(token: String) -> URL {
        var parts = URLComponents(url: origin, resolvingAgainstBaseURL: false)!
        parts.queryItems = [URLQueryItem(name: "token", value: token)]
        return parts.url!
    }

    private func tunnelExited(_ proc: Process, status: Int32) {
        guard proc === tunnel else { return }
        tunnel = nil
        if stopped {
            let waiters = stopWaiters
            stopWaiters = []
            waiters.forEach { $0() }
            return
        }
        health?.invalidate()
        retries += 1
        let delay = min(30.0, pow(2.0, Double(min(retries, 5))))
        let detail = tunnelLog?.lastLines.last.map { " (\($0))" } ?? ""
        update(.starting, "SSH-Tunnel getrennt\(detail). Neuer Versuch in \(Int(delay)) s …")
        let gen = generation
        DispatchQueue.main.asyncAfter(deadline: .now() + delay) {
            MainActor.assumeIsolated {
                guard gen == self.generation, !self.stopped else { return }
                self.start()
            }
        }
    }

    /// Every 15 s: is the UI still there and the token still valid?
    private func scheduleHealth() {
        health?.invalidate()
        let gen = generation
        health = Timer.scheduledTimer(withTimeInterval: 15, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.checkHealth(generation: gen) }
        }
    }

    private func checkHealth(generation gen: Int) {
        guard gen == generation, !stopped else { return }
        let token = self.token, origin = self.origin
        DispatchQueue.global(qos: .utility).async {
            let probe = Net.probe(origin: origin, token: token)
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    guard gen == self.generation, !self.stopped else { return }
                    switch probe {
                    case .ok:
                        if self.status != .ready, let token { self.update(.ready, url: self.pageURL(token: token)) }
                    case .unauthorized:
                        // The remote service restarted with a new token.
                        self.health?.invalidate()
                        self.fetchToken(generation: gen)
                    case .unreachable, .foreign:
                        if let tunnel = self.tunnel, tunnel.isRunning {
                            if self.status == .error { tunnel.terminate() }  // stale forward: rebuild
                            else { self.update(.error, "Server hinter dem Tunnel antwortet nicht.") }
                        } else if self.tunnel == nil {
                            self.health?.invalidate()
                            self.start()
                        }
                    }
                }
            }
        }
    }

    override func reconnect() {
        retries = 0
        super.reconnect()
    }

    override func stop(_ done: @escaping () -> Void) {
        stopped = true
        generation += 1
        health?.invalidate()
        health = nil
        token = nil
        update(.idle)
        guard let tunnel, tunnel.isRunning else {
            self.tunnel = nil
            done()
            return
        }
        stopWaiters.append(done)
        tunnel.terminate()
        let pid = tunnel.processIdentifier
        DispatchQueue.main.asyncAfter(deadline: .now() + 3) {
            if tunnel.isRunning { kill(pid, SIGKILL) }
        }
    }
}

/// A dsh-codex the user reaches some other way; the token comes from the keychain.
@MainActor
final class URLConnection: Connection {
    private var generation = 0
    private var health: Timer?
    private var stopped = true

    override func start() {
        stopped = false
        generation += 1
        let gen = generation
        guard let raw = instance.url, let base = URL(string: raw), base.scheme != nil, base.host != nil,
              let origin = Net.origin(of: base)
        else {
            update(.error, "Ungültige URL.")
            return
        }
        let token = Keychain.token(for: instance.id)
        update(.starting, "\(base.host ?? raw) wird geprüft …")
        DispatchQueue.global(qos: .userInitiated).async {
            let probe = Net.probe(origin: origin, token: token)
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    guard gen == self.generation, !self.stopped else { return }
                    switch probe {
                    case .ok:
                        var parts = URLComponents(url: base, resolvingAgainstBaseURL: false)!
                        if parts.path.isEmpty { parts.path = "/" }
                        if let token {
                            parts.queryItems = (parts.queryItems ?? []).filter { $0.name != "token" }
                                + [URLQueryItem(name: "token", value: token)]
                        }
                        self.update(.ready, url: parts.url)
                    case .unauthorized:
                        self.update(.error, token == nil ? "Kein Token im Schlüsselbund hinterlegt." : "Der Server lehnt das Token ab.")
                    case .foreign(let code):
                        self.update(.error, "Unerwartete Antwort (HTTP \(code)).")
                    case .unreachable(let why):
                        self.update(.error, "Nicht erreichbar: \(why)")
                        self.retryLater(generation: gen)
                    }
                }
            }
        }
    }

    private func retryLater(generation gen: Int) {
        health?.invalidate()
        health = Timer.scheduledTimer(withTimeInterval: 15, repeats: false) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self, gen == self.generation, !self.stopped else { return }
                self.start()
            }
        }
    }

    override func stop(_ done: @escaping () -> Void) {
        stopped = true
        generation += 1
        health?.invalidate()
        update(.idle)
        done()
    }
}
