import XCTest
@testable import Aether

/// The Swift reducer against what the web UI's `reduce.js` makes of the same fixtures
/// (`Fixtures/*.expected.json`, written by `Tools/gen-expected.mjs`): same turns, same items in the same order,
/// same text for user/assistant/reasoning items, same tool calls with the same outcome and approval state.
final class ReducerTests: XCTestCase {
    private var testBundle: Bundle { Bundle(for: ReducerTests.self) }

    private func json(_ data: Data) throws -> JSONValue {
        try JSONDecoder().decode(JSONValue.self, from: data)
    }

    private func expected(_ name: String) throws -> JSONValue {
        let url = try XCTUnwrap(testBundle.url(forResource: "\(name).expected", withExtension: "json"), "\(name).expected.json not bundled")
        return try json(Data(contentsOf: url))
    }

    /// A web UI fixture: the copy bundled in the app (coding, approval) or, for the others, the repo's `ui/fixtures`.
    private func fixtureEvents(_ name: String) throws -> [SessionEvent] {
        var data: Data?
        if let url = Bundle.main.url(forResource: name, withExtension: "json") { data = try Data(contentsOf: url) }
        if data == nil {
            let repo = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            data = try? Data(contentsOf: repo.appending(path: "ui/fixtures/\(name).json"))
        }
        guard let data else { throw XCTSkip("fixture \(name).json not readable") }
        let events = try json(data)["session"]?["events"] ?? .array([])
        return try JSONDecoder().decode([SessionEvent].self, from: JSONEncoder().encode(events))
    }

    private func compare(_ tx: Transcript, _ exp: JSONValue, _ name: String) {
        XCTAssertEqual(tx.title, exp["title"]?.string ?? "", "\(name): title")
        XCTAssertEqual(tx.model, exp["model"]?.string, "\(name): model")
        XCTAssertEqual(tx.pendingApproval?.callId, exp["pendingApproval"]?["callId"]?.string, "\(name): pending approval")
        let turns = exp["turns"]?.array ?? []
        XCTAssertEqual(tx.turns.count, turns.count, "\(name): turn count")
        for (turn, e) in zip(tx.turns, turns) {
            let where_ = "\(name) turn \(turn.turn)"
            XCTAssertEqual(turn.turn, e["turn"]?.int, "\(where_): number")
            XCTAssertEqual(turn.status, e["status"]?.string, "\(where_): status")
            let items = e["items"]?.array ?? []
            XCTAssertEqual(turn.items.map(\.key), items.compactMap { $0["key"]?.string }, "\(where_): item keys")
            for (item, x) in zip(turn.items, items) {
                let at = "\(where_) \(item.key)"
                switch item {
                case .user(let u):
                    XCTAssertEqual(x["kind"]?.string, "user", at)
                    XCTAssertEqual(u.text, x["text"]?.string, "\(at): text")
                case .text(let t):
                    XCTAssertEqual(x["kind"]?.string, "text", at)
                    XCTAssertEqual(t.text, x["text"]?.string, "\(at): text")
                case .reasoning(let r):
                    XCTAssertEqual(x["kind"]?.string, "reasoning", at)
                    XCTAssertEqual(r.text, x["text"]?.string, "\(at): text")
                case .tool(let t):
                    XCTAssertEqual(x["kind"]?.string, "tool", at)
                    XCTAssertEqual(t.id, x["id"]?.string, "\(at): id")
                    XCTAssertEqual(t.name, x["name"]?.string, "\(at): name")
                    XCTAssertEqual(t.result != nil, x["hasResult"]?.bool, "\(at): has result")
                    if let isError = x["isError"]?.bool { XCTAssertEqual(t.result?.isError, isError, "\(at): isError") }
                    XCTAssertEqual(t.approval?.outcome, x["approval"]?["outcome"]?.string, "\(at): approval outcome")
                case .notice(let n):
                    XCTAssertEqual(x["kind"]?.string, "notice", at)
                    XCTAssertEqual(n.code, x["code"]?.string, "\(at): code")
                case .divider(let d):
                    XCTAssertEqual(x["kind"]?.string, "divider", at)
                    XCTAssertEqual(d.running, x["running"]?.bool ?? false, "\(at): running")
                }
            }
            let segments = e["segments"]?.array ?? []
            XCTAssertEqual(turn.segments.map(\.id), segments.compactMap { $0["key"]?.string }, "\(where_): segments")
        }
    }

    func testCodingFixture() throws {
        compare(Reduce.buildTranscript(try fixtureEvents("coding")), try expected("coding"), "coding")
    }

    func testApprovalFixture() throws {
        let tx = Reduce.buildTranscript(try fixtureEvents("approval"))
        compare(tx, try expected("approval"), "approval")
        XCTAssertNotNil(tx.pendingApproval, "approval fixture ends waiting for a decision")
    }

    func testApprovalDecisions() throws {
        for decision in ["allowed-once", "rejected"] {
            let url = try XCTUnwrap(testBundle.url(forResource: "approval-\(decision).events", withExtension: "json"))
            let events = try JSONDecoder().decode([SessionEvent].self, from: Data(contentsOf: url))
            let tx = Reduce.buildTranscript(events)
            compare(tx, try expected("approval-\(decision)"), "approval-\(decision)")
            XCTAssertNil(tx.pendingApproval, "\(decision): approval resolved")
        }
    }

    /// The other fixtures of the web UI, read from the repository next to this file (skipped when not readable).
    func testOtherFixtures() throws {
        var compared = 0
        for name in ["compacting", "error", "failures", "messages", "browser", "computer", "minimal", "markdown"] {
            guard testBundle.url(forResource: "\(name).expected", withExtension: "json") != nil else { continue }
            let events: [SessionEvent]
            do { events = try fixtureEvents(name) } catch is XCTSkip { continue }
            compare(Reduce.buildTranscript(events), try expected(name), name)
            compared += 1
        }
        if compared == 0 { throw XCTSkip("ui/fixtures not readable from the simulator") }
    }
}
