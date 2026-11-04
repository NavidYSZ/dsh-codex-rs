import Foundation

/// Line diff for the edit/write rows, ported from ui/src/diff.js. dsh stores one {oldText, newText} pair per hunk
/// (3 context lines each side), so inputs are small and a plain LCS table is enough.
enum Diff {
    struct Row: Hashable, Sendable {
        enum Op: Hashable, Sendable { case context, delete, insert, gap }
        var op: Op
        var text: String
    }

    private static let maxCells = 400_000

    static func lines(_ oldText: String?, _ newText: String?) -> [Row] {
        let a = oldText.map { $0.components(separatedBy: "\n") } ?? []
        let b = newText.map { $0.components(separatedBy: "\n") } ?? []
        if a.isEmpty { return b.map { Row(op: .insert, text: $0) } }
        if b.isEmpty { return a.map { Row(op: .delete, text: $0) } }
        // Trim the common head and tail so the table only covers the change.
        var head = 0
        while head < a.count, head < b.count, a[head] == b[head] { head += 1 }
        var tail = 0
        while tail < a.count - head, tail < b.count - head, a[a.count - 1 - tail] == b[b.count - 1 - tail] { tail += 1 }
        let A = Array(a[head..<(a.count - tail)])
        let B = Array(b[head..<(b.count - tail)])
        var mid: [Row] = []
        if (A.count + 1) * (B.count + 1) > maxCells {
            mid = A.map { Row(op: .delete, text: $0) } + B.map { Row(op: .insert, text: $0) }
        } else {
            let n = A.count, m = B.count, w = m + 1
            var L = [UInt32](repeating: 0, count: (n + 1) * w)
            if n > 0, m > 0 {
                for i in stride(from: n - 1, through: 0, by: -1) {
                    for j in stride(from: m - 1, through: 0, by: -1) {
                        L[i * w + j] = A[i] == B[j] ? L[(i + 1) * w + j + 1] + 1 : max(L[(i + 1) * w + j], L[i * w + j + 1])
                    }
                }
            }
            var i = 0, j = 0
            while i < n, j < m {
                if A[i] == B[j] { mid.append(Row(op: .context, text: A[i])); i += 1; j += 1 }
                else if L[(i + 1) * w + j] >= L[i * w + j + 1] { mid.append(Row(op: .delete, text: A[i])); i += 1 }
                else { mid.append(Row(op: .insert, text: B[j])); j += 1 }
            }
            while i < n { mid.append(Row(op: .delete, text: A[i])); i += 1 }
            while j < m { mid.append(Row(op: .insert, text: B[j])); j += 1 }
        }
        return a[0..<head].map { Row(op: .context, text: $0) } + mid + a[(a.count - tail)...].map { Row(op: .context, text: $0) }
    }

    static func countChanges(_ rows: [Row]) -> (add: Int, del: Int) {
        var add = 0, del = 0
        for r in rows {
            if r.op == .insert { add += 1 } else if r.op == .delete { del += 1 }
        }
        return (add, del)
    }
}
