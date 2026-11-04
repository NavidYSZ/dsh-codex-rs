import Foundation

/// What a shell result shows: stdout, stderr, the exit code and a note (ports of ui/src/reduce.js
/// `splitBashOutput` and ui/src/persistent-shell.js `splitShellOutput`).
struct ShellOutput: Hashable, Sendable {
    var stdout: String
    var stderr: String
    /// nil when the note replaces it (a timed-out persistent shell).
    var exit: Int?
    var note: String?
    var noteError = false
}

enum BashOutput {
    /// The bash tool appends "[exit code: N]" when the command failed; no marker means 0.
    static func split(_ text: String) -> ShellOutput {
        var body = text
        var exit = 0
        if let m = body.range(of: #"\n?\[exit code: (-?\d+)\]\s*$"#, options: .regularExpression) {
            let marker = body[m]
            if let digits = marker.range(of: #"-?\d+"#, options: .regularExpression) { exit = Int(marker[digits]) ?? 0 }
            body = String(body[..<m.lowerBound])
        }
        // stdout comes first; "[stderr]" opens the stderr part.
        let at: Int
        if body.hasPrefix("[stderr]\n") { at = 0 }
        else if let r = body.range(of: "\n[stderr]\n") { at = body.distance(from: body.startIndex, to: r.lowerBound) }
        else { return ShellOutput(stdout: trimOne(body), stderr: "", exit: exit) }
        let out = at == 0 ? "" : String(body.prefix(at))
        let errStart = body.index(body.startIndex, offsetBy: at == 0 ? 9 : at + 10)
        let err = String(body[errStart...])
        return ShellOutput(stdout: trimOne(out), stderr: trimOne(err), exit: exit)
    }

    /// `ShellOutput` for a persistent-shell result (Minimal chats), or nil when the text is not one.
    static func splitPersistent(_ text: String) -> ShellOutput? {
        let reset = "The persistent bash shell was reset;"
        guard !text.isEmpty, text.contains("[Command finished with exit code") || text.contains(reset) else { return nil }
        var body = text
        var note: String?
        if let r = body.range(of: #"^\[The persistent bash shell was restarted for the [\w-]+ sandbox mode;[^\n]*\]\n"#, options: .regularExpression) {
            body = String(body[r.upperBound...])
            note = L.toolShellRestarted
        }
        if let r = body.range(of: #"^Your command timed out after (\d+) seconds or experienced an OOM error\. Below is partial output:\n"#, options: .regularExpression),
           let end = body.range(of: #"\n?\[Command timed out or OOM\]\nThe persistent bash shell was reset;[^\n]*$"#, options: .regularExpression) {
            let secondsText = body[r].split(separator: " ").first(where: { Int($0) != nil }).map(String.init) ?? "0"
            let partial = String(body[r.upperBound..<end.lowerBound])
            return ShellOutput(stdout: partial, stderr: "", exit: nil, note: L.toolShellTimedOut(Int(secondsText) ?? 0), noteError: true)
        }
        if let r = body.range(of: #"(?:^|\n)\[shell (?:exited: code (-?\d+)|killed by signal: (\w+)|exited)\]\nThe persistent bash shell was reset;[^\n]*$"#, options: .regularExpression) {
            let marker = body[r]
            var exit: Int?
            if let c = marker.range(of: #"code (-?\d+)"#, options: .regularExpression) {
                exit = Int(marker[c].dropFirst(5))
            }
            let cut = marker.hasPrefix("\n") ? body.index(after: r.lowerBound) : r.lowerBound
            return ShellOutput(stdout: String(body[..<cut]), stderr: "", exit: exit, note: L.toolShellReset, noteError: true)
        }
        guard let r = body.range(of: #"(?:^|\n)\[Command finished with exit code (-?\d+)\]$"#, options: .regularExpression) else { return nil }
        let marker = body[r]
        let digits = marker.range(of: #"-?\d+"#, options: .regularExpression).map { Int(marker[$0]) ?? 0 } ?? 0
        let cut = marker.hasPrefix("\n") ? body.index(after: r.lowerBound) : r.lowerBound
        return ShellOutput(stdout: String(body[..<cut]), stderr: "", exit: digits, note: note, noteError: false)
    }

    private static func trimOne(_ s: String) -> String {
        s.hasSuffix("\n") ? String(s.dropLast()) : s
    }
}
