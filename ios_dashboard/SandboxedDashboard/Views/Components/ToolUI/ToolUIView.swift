//
//  ToolUIView.swift
//  SandboxedDashboard
//
//  Main renderer for tool UI components
//

import SwiftUI

struct ToolUIView: View {
    let content: ToolUIContent
    let onOptionSelect: ((String, String) -> Void)?
    
    init(content: ToolUIContent, onOptionSelect: ((String, String) -> Void)? = nil) {
        self.content = content
        self.onOptionSelect = onOptionSelect
    }
    
    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            // Compact tool label
            HStack(spacing: 5) {
                Image(systemName: toolIcon)
                    .font(.system(size: 9, weight: .medium))
                    .foregroundStyle(Theme.accent)
                
                Text(toolName)
                    .font(.system(size: 10, weight: .medium).monospaced())
                    .foregroundStyle(Theme.accent.opacity(0.8))
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
            .background(Theme.accent.opacity(0.08))
            .clipShape(RoundedRectangle(cornerRadius: 6, style: .continuous))
            
            // Tool content
            toolContent
        }
    }
    
    @ViewBuilder
    private var toolContent: some View {
        switch content {
        case .dataTable(let table):
            ToolUIDataTableView(table: table)

        case .optionList(let list):
            ToolUIOptionListView(optionList: list) { optionId in
                if let listId = list.id {
                    onOptionSelect?(listId, optionId)
                }
            }

        case .progress(let progress):
            progressView(progress)

        case .alert(let alert):
            alertView(alert)

        case .codeBlock(let code):
            codeBlockView(code)

        case .unknown(let name, let args):
            unknownToolView(name: name, args: args)
        }
    }

    private var toolIcon: String {
        switch content {
        case .dataTable:
            return "tablecells"
        case .optionList:
            return "list.bullet"
        case .progress:
            return "chart.bar.fill"
        case .alert:
            return "bell.fill"
        case .codeBlock:
            return "chevron.left.forwardslash.chevron.right"
        case .unknown:
            return "questionmark.circle"
        }
    }

    private var toolName: String {
        switch content {
        case .dataTable:
            return "ui_dataTable"
        case .optionList:
            return "ui_optionList"
        case .progress:
            return "ui_progress"
        case .alert:
            return "ui_alert"
        case .codeBlock:
            return "ui_codeBlock"
        case .unknown(let name, _):
            return name
        }
    }

    private func progressView(_ progress: ToolUIProgress) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            if let title = progress.title {
                Text(title)
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(Theme.textPrimary)
            }

            HStack(spacing: 12) {
                // Progress bar
                GeometryReader { geo in
                    ZStack(alignment: .leading) {
                        RoundedRectangle(cornerRadius: 4)
                            .fill(Theme.backgroundTertiary)

                        RoundedRectangle(cornerRadius: 4)
                            .fill(Theme.accent)
                            .frame(width: geo.size.width * progress.percentage)
                    }
                }
                .frame(height: 8)

                // Percentage/count
                Text(progress.displayText)
                    .font(.caption.monospacedDigit())
                    .foregroundStyle(Theme.textSecondary)
            }

            if let status = progress.status {
                Text(status)
                    .font(.caption)
                    .foregroundStyle(Theme.textTertiary)
            }
        }
        .padding(12)
        .background(.ultraThinMaterial)
        .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .stroke(Theme.border, lineWidth: 0.5)
        )
    }

    private func alertView(_ alert: ToolUIAlert) -> some View {
        let (color, icon) = alertStyle(alert.alertType)

        return HStack(alignment: .top, spacing: 12) {
            Image(systemName: icon)
                .font(.system(size: 16))
                .foregroundStyle(color)
                .frame(width: 24, height: 24)

            VStack(alignment: .leading, spacing: 4) {
                Text(alert.title)
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(Theme.textPrimary)

                if let message = alert.message {
                    Text(message)
                        .font(.caption)
                        .foregroundStyle(Theme.textSecondary)
                }
            }

            Spacer(minLength: 0)
        }
        .padding(12)
        .background(color.opacity(0.1))
        .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .stroke(color.opacity(0.3), lineWidth: 1)
        )
    }

    private func alertStyle(_ type: ToolUIAlert.AlertType) -> (Color, String) {
        switch type {
        case .info:
            return (Theme.info, "info.circle.fill")
        case .success:
            return (Theme.success, "checkmark.circle.fill")
        case .warning:
            return (Theme.warning, "exclamationmark.triangle.fill")
        case .error:
            return (Theme.error, "xmark.circle.fill")
        }
    }

    private func codeBlockView(_ code: ToolUICodeBlock) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                if let title = code.title {
                    Text(title)
                        .font(.caption.weight(.medium))
                        .foregroundStyle(Theme.textSecondary)
                }

                Spacer()

                if let language = code.language {
                    Text(language.uppercased())
                        .font(.system(size: 9, weight: .medium).monospaced())
                        .foregroundStyle(Theme.textMuted)
                        .padding(.horizontal, 6)
                        .padding(.vertical, 2)
                        .background(Theme.backgroundTertiary)
                        .clipShape(RoundedRectangle(cornerRadius: 4))
                }

                Button {
                    UIPasteboard.general.string = code.code
                    HapticService.lightTap()
                } label: {
                    Image(systemName: "doc.on.doc")
                        .font(.system(size: 12))
                        .foregroundStyle(Theme.textMuted)
                }
            }

            ScrollView(.horizontal, showsIndicators: false) {
                Text(code.code)
                    .font(.system(size: 12, design: .monospaced))
                    .foregroundStyle(Theme.textPrimary)
                    .textSelection(.enabled)
            }
        }
        .padding(12)
        .background(Color.black.opacity(0.3))
        .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .stroke(Theme.border, lineWidth: 0.5)
        )
    }
    
    private func unknownToolView(name: String, args: String) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Unknown Tool UI")
                .font(.subheadline.weight(.medium))
                .foregroundStyle(Theme.textSecondary)
            
            Text(args)
                .font(.caption.monospaced())
                .foregroundStyle(Theme.textTertiary)
                .lineLimit(10)
        }
        .padding(12)
        .background(.ultraThinMaterial)
        .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .stroke(Theme.border, lineWidth: 0.5)
        )
    }
}

#Preview {
    VStack(spacing: 20) {
        Text("Tool UI Preview")
    }
    .padding()
    .background(Theme.backgroundPrimary)
}
