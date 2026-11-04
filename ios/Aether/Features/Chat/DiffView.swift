import SwiftUI

/// Diff hunks of an edit: added lines green, removed lines red, context dimmed; long diffs clamp.
struct DiffView: View {
    let hunks: [[Diff.Row]]

    var body: some View {
        let rows: [Diff.Row] = hunks.enumerated().flatMap { i, h in (i > 0 ? [Diff.Row(op: .gap, text: "")] : []) + h }
        let lines = rows.map { r -> String in
            switch r.op {
            case .gap: return "⋯"
            case .context: return "  " + r.text
            case .delete: return "− " + r.text
            case .insert: return "+ " + r.text
            }
        }
        Clamp(lines: lines, max: 24) { shown in
            ScrollView(.horizontal, showsIndicators: false) {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(shown.enumerated()), id: \.offset) { _, line in
                        let op = line.first
                        Text(line)
                            .font(.caption.monospaced())
                            .foregroundStyle(op == "+" ? Theme.online : op == "−" ? Theme.danger : Theme.textMuted)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.horizontal, 8)
                            .padding(.vertical, 1)
                            .background(op == "+" ? Theme.online.opacity(0.1) : op == "−" ? Theme.danger.opacity(0.1) : Color.clear)
                    }
                }
                .padding(.vertical, 6)
                .textSelection(.enabled)
            }
            .background(Theme.surfaceRaised, in: RoundedRectangle(cornerRadius: Theme.rowRadius, style: .continuous))
        }
    }
}
