import Foundation

/// Durations, ages and token counts the way the web UI words them.
enum Format {
    /// "4s", "1m 12s", "2h 3m" (ui/src/i18n.js `duration`).
    static func duration(ms: Double) -> String {
        let s = max(1, Int((ms / 1000).rounded()))
        if s < 60 { return "\(s)\(L.timeS)" }
        let m = s / 60
        let gap = L.timeGap
        if m < 60 { return "\(m)\(gap)\(L.timeM) \(s % 60)\(L.timeS)" }
        return "\(m / 60)\(gap)\(L.timeH) \(m % 60)\(gap)\(L.timeM)"
    }

    /// Short relative age for chat rows: "jetzt", "5 min", "3 h", "2 d", then a date.
    static func age(ms: Double, now: Date = Date()) -> String {
        let seconds = max(0, now.timeIntervalSince1970 - ms / 1000)
        if seconds < 60 { return L.timeNow }
        if seconds < 3600 { return L.timeMinutesShort(Int(seconds / 60)) }
        if seconds < 86400 { return L.timeHoursShort(Int(seconds / 3600)) }
        if seconds < 86400 * 7 { return L.timeDaysShort(Int(seconds / 86400)) }
        return Date(timeIntervalSince1970: ms / 1000).formatted(.dateTime.day().month(.abbreviated))
    }

    /// 1234 → "1.2k", 2_300_000 → "2.3M".
    static func compact(_ n: Int) -> String {
        if n < 1000 { return String(n) }
        if n < 1_000_000 { return String(format: n < 10000 ? "%.1fk" : "%.0fk", Double(n) / 1000) }
        return String(format: "%.1fM", Double(n) / 1_000_000)
    }

    static func usd(_ v: Double) -> String {
        if v < 0.01 { return String(format: "$%.4f", v) }
        return String(format: "$%.2f", v)
    }

    static func percent(_ v: Double) -> String {
        v < 1 ? String(format: "%.1f %%", v) : String(format: "%.0f %%", v)
    }

    /// Relative wording for "last connected" ("vor 5 Minuten").
    static func relative(_ date: Date) -> String {
        let f = RelativeDateTimeFormatter()
        f.unitsStyle = .short
        return f.localizedString(for: date, relativeTo: Date())
    }

    /// The last path component ("/Users/alex/code/tally" → "tally").
    static func folderName(_ path: String) -> String {
        path.split(separator: "/").last.map(String.init) ?? path
    }

    /// A path inside the workspace reads better relative.
    static func relPath(_ path: String, cwd: String) -> String {
        if path.isEmpty { return "" }
        if !cwd.isEmpty, path.hasPrefix(cwd + "/") { return String(path.dropFirst(cwd.count + 1)) }
        return path.hasPrefix("./") ? String(path.dropFirst(2)) : path
    }

    static func clip(_ s: String, _ n: Int) -> String {
        s.count > n ? String(s.prefix(n)) + "…" : s
    }

    static func quote(_ s: String) -> String {
        Locale.current.language.languageCode?.identifier == "de" ? "„\(s)“" : "“\(s)”"
    }
}
