import XCTest
@testable import Aether

/// SPEC §2.1: `aether://pair#<base64url(JSON)>`, the bare base64url text, standard base64 with padding, bare JSON.
final class PairingTests: XCTestCase {
    private let json = """
    {"v":1,"server":{"id":"srv_1","name":"MacBook Pro","hostname":"mbp","platform":"darwin","version":"0.3.0"},
     "client":{"id":"cli_1","name":"iPhone"},"token":"act_secret",
     "transports":[{"kind":"https","urls":["https://192.168.178.20:7443"],"certSha256":"ABCDEF"},
                   {"kind":"carrier-pigeon","coop":"roof"},
                   {"kind":"http","urls":["http://127.0.0.1:3190"]},
                   {"kind":"hub","url":"https://hub.example","serverId":"srv_1"}]}
    """

    private func check(_ p: PairingPayload, file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertEqual(p.server.id, "srv_1", file: file, line: line)
        XCTAssertEqual(p.server.name, "MacBook Pro", file: file, line: line)
        XCTAssertEqual(p.token, "act_secret", file: file, line: line)
        XCTAssertEqual(p.client?.id, "cli_1", file: file, line: line)
        XCTAssertEqual(p.transports.map(\.kind), [.https, .unknown, .http, .hub], file: file, line: line)
        if case .https(let h) = p.transports[0] {
            XCTAssertEqual(h.urls, ["https://192.168.178.20:7443"], file: file, line: line)
            XCTAssertEqual(h.certSha256, "ABCDEF", file: file, line: line)
        } else {
            XCTFail("first transport is not https", file: file, line: line)
        }
    }

    func testBareJSON() throws {
        check(try PairingPayload.parse(json))
    }

    func testLinkWithBase64URLWithoutPadding() throws {
        let b64 = Data(json.utf8).base64URLEncodedString()
        XCTAssertFalse(b64.contains("="))
        check(try PairingPayload.parse("aether://pair#" + b64))
        check(try PairingPayload.parse("  aether://pair#\(b64)\n"))
    }

    func testBareBase64URLAndStandardBase64() throws {
        let data = Data(json.utf8)
        check(try PairingPayload.parse(data.base64URLEncodedString()))
        // Standard alphabet with padding (what `base64` on a Mac prints).
        check(try PairingPayload.parse(data.base64EncodedString()))
    }

    func testLinkRoundTrip() throws {
        let p = try PairingPayload.parse(json)
        let link = try p.link()
        XCTAssertTrue(link.hasPrefix("aether://pair#"))
        let again = try PairingPayload.parse(link)
        XCTAssertEqual(again.server, p.server)
        XCTAssertEqual(again.token, p.token)
        XCTAssertEqual(again.transports.filter { $0.kind != .unknown }, p.transports.filter { $0.kind != .unknown })
    }

    func testSSHTransportDefaults() throws {
        let p = try PairingPayload.parse("""
        {"v":1,"server":{"id":"srv_vps","name":"VPS 2"},"transports":[{"kind":"ssh","host":"1.2.3.4","port":22,"user":"ubuntu",
         "hostKeySha256":"SHA256:abc","privateKey":"-----BEGIN OPENSSH PRIVATE KEY-----\\nx\\n-----END OPENSSH PRIVATE KEY-----",
         "remoteHost":"127.0.0.1","remotePort":3080,"tokenCommand":"echo token=abc"}]}
        """)
        XCTAssertNil(p.token)
        guard case .ssh(let s) = p.transports.first else { return XCTFail("no ssh transport") }
        XCTAssertEqual(s.user, "ubuntu")
        XCTAssertEqual(s.remotePort, 3080)
        XCTAssertEqual(s.tokenCommand, "echo token=abc")
    }

    func testRejectsGarbage() {
        XCTAssertThrowsError(try PairingPayload.parse("")) { XCTAssertEqual($0 as? PairingError, .notALink) }
        XCTAssertThrowsError(try PairingPayload.parse("hello world!")) { XCTAssertEqual($0 as? PairingError, .notALink) }
        XCTAssertThrowsError(try PairingPayload.parse("aether://settings")) { XCTAssertNotNil($0 as? PairingError) }
    }

    func testRejectsPayloadWithoutUsableTransport() {
        let onlyUnknown = #"{"v":1,"server":{"id":"s","name":"x"},"transports":[{"kind":"smoke-signal"}]}"#
        XCTAssertThrowsError(try PairingPayload.parse(onlyUnknown)) { error in
            guard case .invalidPayload = error as? PairingError else { return XCTFail("\(error)") }
        }
        let noServer = #"{"v":1,"transports":[{"kind":"http","urls":["http://127.0.0.1:1"]}]}"#
        XCTAssertThrowsError(try PairingPayload.parse(noServer))
    }

    func testTokenFromCommandOutput() {
        XCTAssertEqual(SSHTunnelTransport.extractToken("dsh-codex web: http://127.0.0.1:3080/?token=abc123def\n"), "abc123def")
        XCTAssertEqual(SSHTunnelTransport.extractToken("token=old\nnoise\n0123456789abcdef0123\n"), "0123456789abcdef0123")
        XCTAssertNil(SSHTunnelTransport.extractToken("sudo: a password is required\n"))
    }
}
