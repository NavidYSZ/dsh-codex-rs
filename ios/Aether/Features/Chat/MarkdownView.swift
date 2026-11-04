import SwiftUI

/// Markdown for assistant answers: block structure by own code (paragraphs, headings, lists, quotes, fenced
/// code, tables, rules), inline styling by `AttributedString(markdown:)`. Nothing loads from the net.
struct MarkdownView: View {
    let text: String
    var muted = false

    var body: some View {
        let blocks = MarkdownParser.parse(text)
        VStack(alignment: .leading, spacing: 8) {
            ForEach(blocks) { block in
                MarkdownBlockView(block: block, muted: muted)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

struct MarkdownBlock: Identifiable {
    enum Kind {
        case paragraph(AttributedString)
        case heading(Int, AttributedString)
        case code(lang: String, text: String)
        case list(items: [ListItem], ordered: Bool, start: Int)
        case quote([AttributedString])
        case rule
        case table(header: [AttributedString], rows: [[AttributedString]])
    }

    struct ListItem: Identifiable {
        var id: Int
        var text: AttributedString
        var depth: Int
    }

    var id: Int
    var kind: Kind
}

private struct MarkdownBlockView: View {
    let block: MarkdownBlock
    let muted: Bool

    private var color: Color { muted ? Theme.textMuted : Theme.text }

    var body: some View {
        switch block.kind {
        case .paragraph(let s):
            Text(s).font(.body).foregroundStyle(color).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
        case .heading(let level, let s):
            Text(s)
                .font(level <= 1 ? .title2.weight(.semibold) : level == 2 ? .title3.weight(.semibold) : .headline)
                .foregroundStyle(color)
                .padding(.top, 4)
                .textSelection(.enabled)
        case .code(let lang, let code):
            CodeBlock(lang: lang, code: code)
        case .list(let items, let ordered, let start):
            VStack(alignment: .leading, spacing: 4) {
                ForEach(items) { item in
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        Text(ordered ? "\(start + item.id)." : "•")
                            .font(.body)
                            .foregroundStyle(Theme.textMuted)
                            .frame(minWidth: 16, alignment: .trailing)
                        Text(item.text).font(.body).foregroundStyle(color).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                    }
                    .padding(.leading, CGFloat(item.depth) * 16)
                }
            }
        case .quote(let lines):
            HStack(alignment: .top, spacing: 10) {
                RoundedRectangle(cornerRadius: 2).fill(Theme.border).frame(width: 3)
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(Array(lines.enumerated()), id: \.offset) { _, l in
                        Text(l).font(.body).foregroundStyle(Theme.textMuted).fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
        case .rule:
            Divider()
        case .table(let header, let rows):
            ScrollView(.horizontal, showsIndicators: false) {
                Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 6) {
                    GridRow {
                        ForEach(Array(header.enumerated()), id: \.offset) { _, h in Text(h).font(.footnote.weight(.semibold)).foregroundStyle(color) }
                    }
                    Divider()
                    ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                        GridRow {
                            ForEach(Array(row.enumerated()), id: \.offset) { _, c in Text(c).font(.footnote).foregroundStyle(color) }
                        }
                    }
                }
                .padding(10)
            }
            .background(Theme.surfaceRaised, in: RoundedRectangle(cornerRadius: Theme.rowRadius, style: .continuous))
        }
    }
}

/// A fenced code block with its language and a copy button.
struct CodeBlock: View {
    let lang: String
    let code: String
    @State private var copied = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text(lang.isEmpty ? "text" : lang).font(.caption2).foregroundStyle(Theme.textMuted)
                Spacer()
                Button {
                    UIPasteboard.general.string = code
                    copied = true
                    Task { try? await Task.sleep(for: .seconds(1.4)); copied = false }
                } label: {
                    Text(copied ? L.commonCopied : L.commonCopy).font(.caption2)
                }
                .accessibilityLabel(L.commonCopy)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
            .background(Theme.border.opacity(0.5))
            ScrollView(.horizontal, showsIndicators: false) {
                Text(code)
                    .font(.footnote.monospaced())
                    .foregroundStyle(Theme.text)
                    .textSelection(.enabled)
                    .padding(10)
            }
        }
        .background(Theme.surfaceRaised, in: RoundedRectangle(cornerRadius: Theme.rowRadius, style: .continuous))
        .clipShape(RoundedRectangle(cornerRadius: Theme.rowRadius, style: .continuous))
    }
}

enum MarkdownParser {
    static func inline(_ text: String) -> AttributedString {
        var s: AttributedString
        do {
            s = try AttributedString(markdown: text, options: .init(allowsExtendedAttributes: false, interpretedSyntax: .inlineOnlyPreservingWhitespace, failurePolicy: .returnPartiallyParsedIfPossible))
        } catch {
            s = AttributedString(text)
        }
        for run in s.runs {
            if let intent = run.inlinePresentationIntent, intent.contains(.code) {
                s[run.range].font = .footnote.monospaced()
                s[run.range].backgroundColor = Theme.surfaceRaised
            }
            if let link = run.link, !(link.scheme == "http" || link.scheme == "https" || link.scheme == "mailto") {
                s[run.range].link = nil
            }
        }
        return s
    }

    static func parse(_ text: String) -> [MarkdownBlock] {
        var blocks: [MarkdownBlock] = []
        var paragraph: [String] = []
        var listItems: [MarkdownBlock.ListItem] = []
        var listOrdered = false
        var listStart = 1
        var quote: [String] = []
        var table: [String] = []
        var id = 0
        func next() -> Int { id += 1; return id }
        func flushParagraph() {
            guard !paragraph.isEmpty else { return }
            blocks.append(MarkdownBlock(id: next(), kind: .paragraph(inline(paragraph.joined(separator: " ")))))
            paragraph = []
        }
        func flushList() {
            guard !listItems.isEmpty else { return }
            blocks.append(MarkdownBlock(id: next(), kind: .list(items: listItems, ordered: listOrdered, start: listStart)))
            listItems = []
        }
        func flushQuote() {
            guard !quote.isEmpty else { return }
            blocks.append(MarkdownBlock(id: next(), kind: .quote(quote.map(inline))))
            quote = []
        }
        func flushTable() {
            guard table.count >= 2 else { table.forEach { paragraph.append($0) }; table = []; return }
            let cells = { (line: String) -> [String] in
                var l = line.trimmingCharacters(in: .whitespaces)
                if l.hasPrefix("|") { l.removeFirst() }
                if l.hasSuffix("|") { l.removeLast() }
                return l.components(separatedBy: "|").map { $0.trimmingCharacters(in: .whitespaces) }
            }
            let header = cells(table[0]).map(inline)
            let rows = table.dropFirst(2).map { cells($0).map(inline) }
            blocks.append(MarkdownBlock(id: next(), kind: .table(header: header, rows: Array(rows))))
            table = []
        }
        func flushAll() { flushParagraph(); flushList(); flushQuote(); flushTable() }

        let lines = text.components(separatedBy: "\n")
        var i = 0
        while i < lines.count {
            let line = lines[i]
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            // Fenced code.
            if trimmed.hasPrefix("```") || trimmed.hasPrefix("~~~") {
                flushAll()
                let fence = String(trimmed.prefix(3))
                let lang = trimmed.dropFirst(3).trimmingCharacters(in: .whitespaces).split(separator: " ").first.map(String.init) ?? ""
                var code: [String] = []
                i += 1
                while i < lines.count, !lines[i].trimmingCharacters(in: .whitespaces).hasPrefix(fence) {
                    code.append(lines[i])
                    i += 1
                }
                blocks.append(MarkdownBlock(id: next(), kind: .code(lang: lang, text: code.joined(separator: "\n"))))
                i += 1
                continue
            }
            if trimmed.isEmpty {
                flushAll()
                i += 1
                continue
            }
            // Table: a header row followed by a separator row.
            if trimmed.contains("|"), i + 1 < lines.count, lines[i + 1].range(of: #"^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$"#, options: .regularExpression) != nil, table.isEmpty {
                flushParagraph(); flushList(); flushQuote()
                table = [line, lines[i + 1]]
                i += 2
                while i < lines.count, lines[i].contains("|"), !lines[i].trimmingCharacters(in: .whitespaces).isEmpty {
                    table.append(lines[i])
                    i += 1
                }
                flushTable()
                continue
            }
            if let r = trimmed.range(of: #"^#{1,6}\s+"#, options: .regularExpression) {
                flushAll()
                let level = trimmed[r].filter { $0 == "#" }.count
                blocks.append(MarkdownBlock(id: next(), kind: .heading(level, inline(String(trimmed[r.upperBound...]).replacingOccurrences(of: #"\s+#+$"#, with: "", options: .regularExpression)))))
                i += 1
                continue
            }
            if trimmed.range(of: #"^([-*_])(\s*\1){2,}$"#, options: .regularExpression) != nil {
                flushAll()
                blocks.append(MarkdownBlock(id: next(), kind: .rule))
                i += 1
                continue
            }
            if trimmed.hasPrefix(">") {
                flushParagraph(); flushList(); flushTable()
                quote.append(String(trimmed.dropFirst()).trimmingCharacters(in: .whitespaces))
                i += 1
                continue
            }
            if let r = line.range(of: #"^(\s*)([-*+]|\d+[.)])\s+"#, options: .regularExpression) {
                flushParagraph(); flushQuote(); flushTable()
                let marker = line[r]
                let indent = marker.prefix { $0 == " " || $0 == "\t" }.count
                let ordered = marker.contains { $0.isNumber }
                if listItems.isEmpty {
                    listOrdered = ordered
                    listStart = Int(marker.trimmingCharacters(in: .whitespaces).filter(\.isNumber)) ?? 1
                }
                var body = String(line[r.upperBound...])
                // A task list box or a continuation line.
                body = body.replacingOccurrences(of: #"^\[( |x|X)\]\s*"#, with: "", options: .regularExpression)
                while i + 1 < lines.count, !lines[i + 1].trimmingCharacters(in: .whitespaces).isEmpty,
                      lines[i + 1].range(of: #"^(\s*)([-*+]|\d+[.)])\s+"#, options: .regularExpression) == nil,
                      lines[i + 1].hasPrefix("  "), !lines[i + 1].trimmingCharacters(in: .whitespaces).hasPrefix("```") {
                    body += " " + lines[i + 1].trimmingCharacters(in: .whitespaces)
                    i += 1
                }
                listItems.append(MarkdownBlock.ListItem(id: listItems.count, text: inline(body), depth: min(indent / 2, 4)))
                i += 1
                continue
            }
            flushList(); flushQuote(); flushTable()
            paragraph.append(trimmed)
            i += 1
        }
        flushAll()
        return blocks
    }
}
