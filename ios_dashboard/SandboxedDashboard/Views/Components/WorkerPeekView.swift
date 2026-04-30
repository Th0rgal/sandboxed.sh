//
//  WorkerPeekView.swift
//  SandboxedDashboard
//
//  Read-only view of a worker mission's chat messages.
//  Shows tool calls and assistant responses in a compact timeline.
//

import SwiftUI

struct WorkerPeekView: View {
    let mission: Mission
    @State private var events: [StoredEvent] = []
    @State private var isLoading = true
    @State private var error: String?

    private let api = APIService.shared

    var body: some View {
        NavigationStack {
            Group {
                if isLoading {
                    loadingView
                } else if let error {
                    errorView(error)
                } else if events.isEmpty {
                    emptyView
                } else {
                    eventList
                }
            }
            .background(Theme.backgroundPrimary)
            .navigationTitle(mission.displayTitle)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .principal) {
                    VStack(spacing: 1) {
                        Text(mission.displayTitle)
                            .font(.subheadline.weight(.semibold))
                            .lineLimit(1)
                        HStack(spacing: 4) {
                            StatusBadge(status: mission.status.statusType, compact: true)
                            if let backend = mission.backend {
                                Text(backend)
                                    .font(.system(size: 10))
                                    .foregroundStyle(Theme.textMuted)
                            }
                        }
                    }
                }
            }
        }
        .task {
            await loadEvents()
        }
    }

    // MARK: - Event List

    private var eventList: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 8) {
                ForEach(events) { event in
                    eventRow(event)
                }
            }
            .padding(16)
        }
    }

    private func eventRow(_ event: StoredEvent) -> some View {
        Group {
            switch event.eventType {
            case "user_message":
                userMessageRow(event)
            case "assistant_message":
                assistantMessageRow(event)
            case "tool_call":
                toolCallRow(event)
            case "tool_result":
                toolResultRow(event)
            case "thinking":
                thinkingRow(event)
            default:
                EmptyView()
            }
        }
    }

    private func userMessageRow(_ event: StoredEvent) -> some View {
        HStack {
            Spacer()
            Text(event.content)
                .font(.subheadline)
                .foregroundStyle(Theme.textPrimary)
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
                .background(Theme.accent.opacity(0.15))
                .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
                .frame(maxWidth: 300, alignment: .trailing)
        }
    }

    private func assistantMessageRow(_ event: StoredEvent) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(event.content)
                .font(.subheadline)
                .foregroundStyle(Theme.textPrimary)
                .textSelection(.enabled)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.borderSubtle)
        .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
    }

    private func toolCallRow(_ event: StoredEvent) -> some View {
        let name = event.toolName ?? "tool"
        let icon = toolIcon(name)

        return HStack(spacing: 6) {
            Image(systemName: icon)
                .font(.system(size: 10, weight: .medium))
                .foregroundStyle(Theme.accent)
            Text(name)
                .font(.caption.weight(.medium).monospaced())
                .foregroundStyle(Theme.textSecondary)

            if !event.content.isEmpty {
                Text(event.content.prefix(80))
                    .font(.caption2)
                    .foregroundStyle(Theme.textMuted)
                    .lineLimit(1)
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
        .background(Theme.accent.opacity(0.06))
        .clipShape(Capsule())
    }

    private func toolResultRow(_ event: StoredEvent) -> some View {
        let isError = event.content.contains("error") || event.content.contains("Error")
        let preview = String(event.content.prefix(120))

        return HStack(spacing: 6) {
            Image(systemName: isError ? "xmark.circle" : "checkmark.circle")
                .font(.system(size: 10))
                .foregroundStyle(isError ? Theme.error : Theme.success)
            Text(preview)
                .font(.caption2)
                .foregroundStyle(Theme.textTertiary)
                .lineLimit(2)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
    }

    private func thinkingRow(_ event: StoredEvent) -> some View {
        HStack(spacing: 6) {
            Image(systemName: "brain")
                .font(.system(size: 10, weight: .medium))
                .foregroundStyle(Theme.accent.opacity(0.6))
            Text(String(event.content.prefix(100)))
                .font(.caption2)
                .foregroundStyle(Theme.textMuted)
                .lineLimit(2)
                .italic()
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 4)
    }

    private func toolIcon(_ name: String) -> String {
        let lower = name.lowercased()
        if lower.contains("bash") || lower.contains("terminal") { return "terminal" }
        if lower.contains("read") || lower.contains("file") { return "doc.text" }
        if lower.contains("write") || lower.contains("edit") { return "square.and.pencil" }
        if lower.contains("grep") || lower.contains("glob") || lower.contains("search") { return "magnifyingglass" }
        if lower.contains("web") { return "globe" }
        if lower.contains("agent") || lower.contains("task") { return "person.2" }
        return "wrench"
    }

    // MARK: - States

    private var loadingView: some View {
        VStack(spacing: 12) {
            ProgressView()
                .tint(Theme.accent)
            Text("Loading worker output...")
                .font(.caption)
                .foregroundStyle(Theme.textMuted)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private func errorView(_ message: String) -> some View {
        VStack(spacing: 8) {
            Image(systemName: "exclamationmark.triangle")
                .font(.title2)
                .foregroundStyle(Theme.error)
            Text(message)
                .font(.caption)
                .foregroundStyle(Theme.textTertiary)
                .multilineTextAlignment(.center)
        }
        .padding()
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var emptyView: some View {
        VStack(spacing: 8) {
            Image(systemName: "tray")
                .font(.title2)
                .foregroundStyle(Theme.textMuted)
            Text("No output yet")
                .font(.caption)
                .foregroundStyle(Theme.textTertiary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    // MARK: - Data

    private func loadEvents() async {
        do {
            let types = ["user_message", "assistant_message", "tool_call", "tool_result", "text_delta", "thinking"]
            events = try await api.getMissionEvents(id: mission.id, types: types, limit: 200)
            isLoading = false
        } catch {
            self.error = error.localizedDescription
            isLoading = false
        }
    }
}
