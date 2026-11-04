import CryptoKit
import Foundation
import os

/// Where requests go once a transport is open: a base URL, the URLSession that may talk to it (pinned or not)
/// and the bearer token to send.
struct Endpoint: Sendable {
    var kind: TransportKind
    var baseURL: URL
    var session: URLSession
    var bearer: any BearerSource
    /// Shown on the device card ("192.168.178.20:7443", "ubuntu@vps").
    var label: String
}

/// The bearer token of a device: fixed (client or start token) or produced by `tokenCommand` over SSH.
protocol BearerSource: Sendable {
    func current() async throws -> String?
    /// After a 401: a fresh token, or nil when there is no way to get one.
    func refresh() async throws -> String?
}

struct FixedBearer: BearerSource {
    var token: String?
    func current() async throws -> String? { token }
    func refresh() async throws -> String? { nil }
}

/// A way to reach a server (SPEC §2). `open()` establishes the path; `close()` tears it down.
protocol Transport: AnyObject, Sendable {
    var kind: TransportKind { get }
    func open() async throws -> Endpoint
    func close() async
}

/// Hosts the app accepts plain http for: loopback only (the simulator's dev server, the local end of an SSH tunnel).
enum Loopback {
    static func isLoopback(_ host: String?) -> Bool {
        guard let host = host?.lowercased() else { return false }
        return host == "127.0.0.1" || host == "localhost" || host == "::1" || host == "[::1]" || host.hasPrefix("127.")
    }
}

// MARK: - Direct (https with an optional pin, http to loopback)

/// `https`/`http` transports: URLSession straight to the server. With `certSha256` the server-trust challenge
/// accepts exactly the certificate whose DER SHA-256 matches and ignores the chain; without it the system validates.
final class DirectTransport: Transport {
    let kind: TransportKind
    private let urls: [String]
    private let certSha256: String?
    private let bearer: any BearerSource
    private let allowPlainHTTP: Bool
    private let probeTimeout: TimeInterval
    /// Optional: a URLSession to use instead of a new one (tests inject a stubbed `URLProtocol`).
    private let sessionOverride: URLSession?

    init(kind: TransportKind, urls: [String], certSha256: String? = nil, bearer: any BearerSource, allowPlainHTTP: Bool,
         probeTimeout: TimeInterval = 6, session: URLSession? = nil) {
        self.kind = kind
        self.urls = urls
        self.certSha256 = certSha256?.lowercased()
        self.bearer = bearer
        self.allowPlainHTTP = allowPlainHTTP
        self.probeTimeout = probeTimeout
        self.sessionOverride = session
    }

    static func makeSession(certSha256: String?, timeout: TimeInterval = 30) -> URLSession {
        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = timeout
        config.timeoutIntervalForResource = 600
        config.waitsForConnectivity = false
        config.httpAdditionalHeaders = ["Accept": "application/json"]
        return URLSession(configuration: config, delegate: PinningDelegate(certSha256: certSha256), delegateQueue: nil)
    }

    /// Probes every URL (LAN, Tailscale, others) with `GET /api/info` at the same time; the first that answers wins.
    /// An address that is blocked or belongs to another network then costs one timeout in total, not one per address.
    /// A 401 ends the attempt (the token is wrong for this server); a failed handshake or a pin mismatch on one address
    /// does not, since another address may reach the right machine.
    func open() async throws -> Endpoint {
        var reasons: [String] = []
        let session = sessionOverride ?? DirectTransport.makeSession(certSha256: certSha256)
        let pinning = session.delegate as? PinningDelegate
        var candidates: [URL] = []
        for raw in urls {
            guard let url = URL(string: raw), let scheme = url.scheme?.lowercased(), let host = url.host else {
                reasons.append(ApiError.invalidURL.message)
                continue
            }
            if scheme == "http" {
                guard Loopback.isLoopback(host) else { reasons.append(ApiError.plainHTTPRefused(host: host).message); continue }
                guard allowPlainHTTP else { reasons.append(ApiError.plainHTTPOff.message); continue }
            } else if scheme != "https" {
                reasons.append(ApiError.invalidURL.message)
                continue
            }
            candidates.append(url)
        }
        let outcome = await withTaskGroup(of: (URL, ApiError?).self, returning: ProbeOutcome.self) { group in
            for url in candidates {
                group.addTask { [self] in
                    do {
                        try await probe(url, session: session)
                        return (url, nil)
                    } catch {
                        // The pinning delegate cancels a wrong certificate; only it knows that this was a mismatch.
                        if let host = url.host, pinning?.rejected(host: host) == true { return (url, .certificateMismatch) }
                        return (url, ApiError.wrap(error))
                    }
                }
            }
            var failures: [URL: ApiError] = [:]
            for await (url, error) in group {
                guard let error else {
                    group.cancelAll()
                    return .found(url)
                }
                if error == .unauthorized {
                    group.cancelAll()
                    return .unauthorized
                }
                failures[url] = error
            }
            return .failed(failures)
        }
        switch outcome {
        case .found(let url):
            let label = (url.host ?? "") + (url.port.map { ":\($0)" } ?? "")
            return Endpoint(kind: kind, baseURL: url, session: session, bearer: bearer, label: label)
        case .unauthorized:
            throw ApiError.unauthorized
        case .failed(let failures):
            // Every address failed: one sentence per distinct reason, in payload order.
            for url in candidates {
                guard let message = failures[url]?.message, !reasons.contains(message) else { continue }
                reasons.append(message)
            }
            throw ApiError.noTransport(reasons)
        }
    }

    private enum ProbeOutcome: Sendable {
        case found(URL)
        case unauthorized
        case failed([URL: ApiError])
    }

    func close() async {}

    private func probe(_ base: URL, session: URLSession) async throws {
        var req = URLRequest(url: base.appending(path: "api/info"))
        req.timeoutInterval = probeTimeout
        if let token = try await bearer.current() { req.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }
        let (_, response) = try await session.data(for: req)
        guard let http = response as? HTTPURLResponse else { throw ApiError.badResponse }
        if http.statusCode == 401 || http.statusCode == 403 { throw ApiError.unauthorized }
        guard (200..<300).contains(http.statusCode) else { throw ApiError.http(status: http.statusCode, message: nil, code: nil) }
    }
}

/// Accepts the pinned certificate (SHA-256 of the DER leaf), else lets the system validate.
final class PinningDelegate: NSObject, URLSessionDelegate, Sendable {
    let certSha256: String?
    /// Hosts whose certificate did not match the pin (a cancelled challenge otherwise looks like any cancellation).
    private let rejectedHosts = OSAllocatedUnfairLock<Set<String>>(initialState: [])

    init(certSha256: String?) {
        self.certSha256 = certSha256?.lowercased()
    }

    func rejected(host: String) -> Bool {
        rejectedHosts.withLock { $0.contains(host.lowercased()) }
    }

    func urlSession(_ session: URLSession, didReceive challenge: URLAuthenticationChallenge,
                    completionHandler: @escaping @Sendable (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
        guard challenge.protectionSpace.authenticationMethod == NSURLAuthenticationMethodServerTrust,
              let trust = challenge.protectionSpace.serverTrust else {
            completionHandler(.performDefaultHandling, nil)
            return
        }
        guard let pin = certSha256 else {
            completionHandler(.performDefaultHandling, nil)
            return
        }
        if let leaf = Self.leafSha256(trust), leaf == pin {
            completionHandler(.useCredential, URLCredential(trust: trust))
        } else {
            let host = challenge.protectionSpace.host.lowercased()
            rejectedHosts.withLock { _ = $0.insert(host) }
            completionHandler(.cancelAuthenticationChallenge, nil)
        }
    }

    static func leafSha256(_ trust: SecTrust) -> String? {
        guard let chain = SecTrustCopyCertificateChain(trust) as? [SecCertificate], let leaf = chain.first else { return nil }
        let der = SecCertificateCopyData(leaf) as Data
        return SHA256.hash(data: der).map { String(format: "%02x", $0) }.joined()
    }
}
