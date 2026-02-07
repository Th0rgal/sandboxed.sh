import SwiftUI

private enum MarkdownBlock {
    case paragraph(String)
    case heading(level: Int, text: String)
    case list(ordered: Bool, items: [String])
    case codeBlock(language: String?, code: String)
    case table(headers: [String], rows: [[String]])
    case blockquote(String)

}

struct MarkdownView: View {
    let content: String

    init(_ content: String) {
        self.content = content
    }

    var body: some View {
        let blocks = MarkdownParser.parse(content)
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                switch block {
                case .paragraph(let text):
                    MarkdownInlineText(text)
                case .heading(let level, let text):
                    MarkdownInlineText(text)
                        .font(headingFont(level))
                        .fontWeight(.semibold)
                case .list(let ordered, let items):
                    MarkdownListView(ordered: ordered, items: items)
                case .codeBlock(_, let code):
                    MarkdownCodeBlock(code: code)
                case .table(let headers, let rows):
                    MarkdownTableView(headers: headers, rows: rows)
                case .blockquote(let text):
                    MarkdownBlockquoteView(text: text)
                }
            }
        }
    }

    private func headingFont(_ level: Int) -> Font {
        switch level {
        case 1: return .title2
        case 2: return .title3
        case 3: return .headline
        default: return .subheadline
        }
    }
}

private struct MarkdownInlineText: View {
    let content: String

    init(_ content: String) {
        self.content = content
    }

    var body: some View {
        if let attributed = try? AttributedString(markdown: content, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)) {
            Text(attributed)
                .font(.body)
                .foregroundStyle(Theme.textPrimary)
                .tint(Theme.accent)
        } else {
            Text(content)
                .font(.body)
                .foregroundStyle(Theme.textPrimary)
        }
    }
}

private struct MarkdownListView: View {
    let ordered: Bool
    let items: [String]

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(items.indices, id: \.self) { index in
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(ordered ? "\(index + 1)." : "•")
                        .font(.body)
                        .foregroundStyle(Theme.textSecondary)
                        .frame(minWidth: 20, alignment: .leading)
                    MarkdownInlineText(items[index])
                }
            }
        }
    }
}

private struct MarkdownCodeBlock: View {
    let code: String

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            Text(code)
                .font(.system(.body, design: .monospaced))
                .foregroundStyle(Theme.textPrimary)
                .padding(12)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .background(Theme.backgroundTertiary)
        .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .stroke(Theme.border, lineWidth: 1)
        )
    }
}

private struct MarkdownTableView: View {
    let headers: [String]
    let rows: [[String]]

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            Grid(alignment: .leading, horizontalSpacing: 12, verticalSpacing: 8) {
                GridRow {
                    ForEach(headers.indices, id: \.self) { index in
                        MarkdownInlineText(headers[index])
                            .font(.subheadline)
                            .fontWeight(.semibold)
                            .padding(.vertical, 4)
                    }
                }
                Divider()
                ForEach(rows.indices, id: \.self) { rowIndex in
                    GridRow {
                        ForEach(rows[rowIndex].indices, id: \.self) { colIndex in
                            MarkdownInlineText(rows[rowIndex][colIndex])
                                .font(.subheadline)
                        }
                    }
                }
            }
            .padding(12)
        }
        .background(Theme.backgroundTertiary)
        .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .stroke(Theme.border, lineWidth: 1)
        )
    }
}

private struct MarkdownBlockquoteView: View {
    let text: String

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Rectangle()
                .fill(Theme.accent)
                .frame(width: 3)
                .clipShape(Capsule())
            MarkdownInlineText(text)
                .font(.body)
                .foregroundStyle(Theme.textSecondary)
        }
        .padding(.vertical, 4)
    }
}

private enum MarkdownParser {
    static func parse(_ content: String) -> [MarkdownBlock] {
        let normalized = content.replacingOccurrences(of: "\r\n", with: "\n")
        let lines = normalized.components(separatedBy: "\n")
        var blocks: [MarkdownBlock] = []
        var index = 0

        while index < lines.count {
            let line = lines[index]
            let trimmed = line.trimmingCharacters(in: .whitespaces)

            if trimmed.isEmpty {
                index += 1
                continue
            }

            if trimmed.hasPrefix("```") {
                let language = trimmed.dropFirst(3).trimmingCharacters(in: .whitespaces)
                var codeLines: [String] = []
                index += 1
                while index < lines.count {
                    let current = lines[index]
                    if current.trimmingCharacters(in: .whitespaces).hasPrefix("```") {
                        index += 1
                        break
                    }
                    codeLines.append(current)
                    index += 1
                }
                blocks.append(.codeBlock(language: language.isEmpty ? nil : String(language), code: codeLines.joined(separator: "\n")))
                continue
            }

            if isTableHeader(at: index, lines: lines) {
                let headerLine = lines[index]
                let headerCells = splitTableLine(headerLine)
                index += 2
                var rows: [[String]] = []
                while index < lines.count {
                    let rowLine = lines[index]
                    let rowTrimmed = rowLine.trimmingCharacters(in: .whitespaces)
                    if rowTrimmed.isEmpty || !rowLine.contains("|") {
                        break
                    }
                    rows.append(splitTableLine(rowLine))
                    index += 1
                }
                blocks.append(.table(headers: headerCells, rows: rows))
                continue
            }

            if let heading = parseHeading(trimmed) {
                blocks.append(.heading(level: heading.level, text: heading.text))
                index += 1
                continue
            }

            if trimmed.hasPrefix(">") {
                var quoteLines: [String] = []
                while index < lines.count {
                    let current = lines[index].trimmingCharacters(in: .whitespaces)
                    guard current.hasPrefix(">") else { break }
                    let stripped = current.dropFirst().trimmingCharacters(in: .whitespaces)
                    quoteLines.append(String(stripped))
                    index += 1
                }
                blocks.append(.blockquote(quoteLines.joined(separator: "\n")))
                continue
            }

            if let listItem = parseListItem(trimmed) {
                var items: [String] = [listItem.text]
                let ordered = listItem.ordered
                index += 1
                while index < lines.count {
                    let currentTrimmed = lines[index].trimmingCharacters(in: .whitespaces)
                    guard let nextItem = parseListItem(currentTrimmed), nextItem.ordered == ordered else { break }
                    items.append(nextItem.text)
                    index += 1
                }
                blocks.append(.list(ordered: ordered, items: items))
                continue
            }

            var paragraphLines: [String] = [trimmed]
            index += 1
            while index < lines.count {
                let current = lines[index]
                let currentTrimmed = current.trimmingCharacters(in: .whitespaces)
                if currentTrimmed.isEmpty || isBlockStart(at: index, lines: lines) {
                    break
                }
                paragraphLines.append(currentTrimmed)
                index += 1
            }
            blocks.append(.paragraph(paragraphLines.joined(separator: "\n")))
        }

        return blocks
    }

    private static func parseHeading(_ line: String) -> (level: Int, text: String)? {
        let hashes = line.prefix { $0 == "#" }.count
        guard hashes > 0, hashes <= 6 else { return nil }
        let text = line.dropFirst(hashes).trimmingCharacters(in: .whitespaces)
        return (hashes, text.isEmpty ? line : String(text))
    }

    private static func parseListItem(_ line: String) -> (ordered: Bool, text: String)? {
        if line.hasPrefix("- ") || line.hasPrefix("* ") || line.hasPrefix("+ ") {
            return (false, String(line.dropFirst(2)))
        }

        let components = line.split(separator: " ", maxSplits: 1, omittingEmptySubsequences: true)
        if components.count == 2, let first = components.first {
            if first.last == ".", first.dropLast().allSatisfy({ $0.isNumber }) {
                return (true, String(components[1]))
            }
        }

        return nil
    }

    private static func isTableHeader(at index: Int, lines: [String]) -> Bool {
        guard index + 1 < lines.count else { return false }
        let header = lines[index]
        let separator = lines[index + 1]
        return header.contains("|") && isTableSeparator(separator)
    }

    private static func isTableSeparator(_ line: String) -> Bool {
        let trimmed = line.trimmingCharacters(in: .whitespaces)
        let cleaned = trimmed.trimmingCharacters(in: CharacterSet(charactersIn: "|"))
        let parts = cleaned.split(separator: "|")
        guard !parts.isEmpty else { return false }
        for part in parts {
            let cell = part.trimmingCharacters(in: .whitespaces)
            if cell.isEmpty { return false }
            let trimmedCell = cell.trimmingCharacters(in: CharacterSet(charactersIn: ":"))
            if trimmedCell.count < 3 || !trimmedCell.allSatisfy({ $0 == "-" }) {
                return false
            }
        }
        return true
    }

    private static func splitTableLine(_ line: String) -> [String] {
        let trimmed = line.trimmingCharacters(in: .whitespaces)
        let cleaned = trimmed.trimmingCharacters(in: CharacterSet(charactersIn: "|"))
        return cleaned.split(separator: "|").map { $0.trimmingCharacters(in: .whitespaces) }
    }

    private static func isBlockStart(at index: Int, lines: [String]) -> Bool {
        let trimmed = lines[index].trimmingCharacters(in: .whitespaces)
        if trimmed.isEmpty { return true }
        if trimmed.hasPrefix("```") { return true }
        if parseHeading(trimmed) != nil { return true }
        if trimmed.hasPrefix(">") { return true }
        if parseListItem(trimmed) != nil { return true }
        if isTableHeader(at: index, lines: lines) { return true }
        return false
    }
}
