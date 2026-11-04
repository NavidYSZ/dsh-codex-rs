import XCTest
@testable import Aether

/// Answers every request of a stubbed URLSession from `handler`.
final class StubProtocol: URLProtocol {
    nonisolated(unsafe) static var handler: (@Sendable (URLRequest) -> (Int, Data))?
    nonisolated(unsafe) static var seen: [URLRequest] = []

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        StubProtocol.seen.append(request)
        let (status, body) = StubProtocol.handler?(request) ?? (404, Data())
        // Status 0 stands for a TLS handshake that broke off.
        if status == 0 {
            client?.urlProtocol(self, didFailWithError: URLError(.secureConnectionFailed))
            return
        }
        let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: "HTTP/1.1", headerFields: ["Content-Type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: body)
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}

    static func session() -> URLSession {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [StubProtocol.self]
        return URLSession(configuration: config)
    }
}

/// `DirectTransport` with a stubbed `URLProtocol` (SPEC §4 tests): probing, bearer, loopback-only plain http.
final class TransportTests: XCTestCase {
    private let info = Data(#"{"version":"0.3.0","models":[],"settings":{"model":"deepseek-flash"},"apiKeyConfigured":true}"#.utf8)

    override func setUp() {
        StubProtocol.seen = []
        StubProtocol.handler = nil
    }

    func testOpensFirstAnsweringURLWithBearer() async throws {
        let info = info
        StubProtocol.handler = { req in
            req.url?.host == "127.0.0.1" && req.url?.path == "/api/info" ? (200, info) : (500, Data())
        }
        let t = DirectTransport(kind: .http, urls: ["http://127.0.0.1:3190"], bearer: FixedBearer(token: "tok"), allowPlainHTTP: true,
                                session: StubProtocol.session())
        let endpoint = try await t.open()
        XCTAssertEqual(endpoint.baseURL.absoluteString, "http://127.0.0.1:3190")
        XCTAssertEqual(endpoint.label, "127.0.0.1:3190")
        XCTAssertEqual(StubProtocol.seen.first?.value(forHTTPHeaderField: "Authorization"), "Bearer tok")

        // The REST client on top of it decodes /api/info.
        let client = ServerClient(endpoint: endpoint)
        let decoded = try await client.info()
        XCTAssertEqual(decoded.version, "0.3.0")
    }

    func testFallsThroughToNextURL() async throws {
        let info = info
        StubProtocol.handler = { req in req.url?.port == 2 ? (200, info) : (502, Data()) }
        let t = DirectTransport(kind: .https, urls: ["https://10.0.0.1:1", "https://10.0.0.1:2"], bearer: FixedBearer(token: nil),
                                allowPlainHTTP: false, session: StubProtocol.session())
        let endpoint = try await t.open()
        XCTAssertEqual(endpoint.baseURL.port, 2)
    }

    func testUnauthorizedStopsImmediately() async {
        StubProtocol.handler = { _ in (401, Data()) }
        let t = DirectTransport(kind: .http, urls: ["http://127.0.0.1:3190", "http://localhost:3190"], bearer: FixedBearer(token: "bad"),
                                allowPlainHTTP: true, session: StubProtocol.session())
        do {
            _ = try await t.open()
            XCTFail("open succeeded")
        } catch {
            XCTAssertEqual(error as? ApiError, .unauthorized)
        }
    }

    /// A handshake that breaks off on one address must not end the attempt or read as a pin mismatch.
    func testFailedAddressDoesNotStopTheOthers() async throws {
        let info = info
        StubProtocol.handler = { req in req.url?.host == "10.0.0.2" ? (200, info) : (0, Data()) }
        let t = DirectTransport(kind: .https, urls: ["https://10.0.0.1:7443", "https://10.0.0.2:7443"], bearer: FixedBearer(token: "t"),
                                allowPlainHTTP: false, session: StubProtocol.session())
        let endpoint = try await t.open()
        XCTAssertEqual(endpoint.baseURL.host, "10.0.0.2")
        XCTAssertEqual(ApiError.wrap(URLError(.secureConnectionFailed)).isUnreachable, true)
    }

    func testPlainHTTPOnlyToLoopback() async {
        StubProtocol.handler = { _ in (200, Data("{}".utf8)) }
        let t = DirectTransport(kind: .http, urls: ["http://192.168.1.5:3190"], bearer: FixedBearer(token: nil), allowPlainHTTP: true,
                                session: StubProtocol.session())
        do {
            _ = try await t.open()
            XCTFail("plain http to a LAN host was accepted")
        } catch {
            guard case .noTransport = error as? ApiError else { return XCTFail("\(error)") }
        }
        XCTAssertTrue(StubProtocol.seen.isEmpty, "no request may leave for a non-loopback http URL")
        XCTAssertTrue(Loopback.isLoopback("127.0.0.1"))
        XCTAssertTrue(Loopback.isLoopback("localhost"))
        XCTAssertFalse(Loopback.isLoopback("example.com"))
    }

    /// SPEC §4 asks for the tunnel against this Mac's sshd with a temporary restricted key. That needs a key in
    /// ~/.ssh/authorized_keys, which a unit test should not touch; run it by hand (README) instead.
    func testSSHTunnelAgainstLocalSSHD() throws {
        throw XCTSkip("needs a temporary key in ~/.ssh/authorized_keys; see ios/README.md")
    }
}
