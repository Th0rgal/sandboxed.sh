//
//  QueueSheet.swift
//  SandboxedDashboard
//
//  Bottom sheet for viewing and managing queued messages
//

import SwiftUI

struct QueueSheet: View {
    let items: [QueuedMessage]
    let onRemove: (String) -> Void
    let onClearAll: () -> Void
    let onDismiss: () -> Void

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                if items.isEmpty {
                    emptyState
                } else {
                    queueList
                }
            }
            .background(Theme.backgroundSecondary)
            .navigationTitle("Message Queue")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button("Done") {
                        onDismiss()
                    }
                    .foregroundStyle(Theme.accent)
                }

                if items.count > 1 {
                    ToolbarItem(placement: .topBarTrailing) {
                        Button(role: .destructive) {
                            onClearAll()
                            HapticService.success()
                        } label: {
                            Text("Clear All")
                                .foregroundStyle(Theme.error)
                        }
                    }
                }
            }
        }
    }

    private var emptyState: some View {
        VStack(spacing: 16) {
            Spacer()

            Image(systemName: "tray")
                .font(.system(size: 48, weight: .light))
                .foregroundStyle(Theme.textMuted)

            VStack(spacing: 8) {
                Text("Queue Empty")
                    .font(.title3.weight(.semibold))
                    .foregroundStyle(Theme.textPrimary)

                Text("Messages sent while the agent is busy will appear here")
                    .font(.subheadline)
                    .foregroundStyle(Theme.textSecondary)
                    .multilineTextAlignment(.center)
            }

            Spacer()
            Spacer()
        }
        .padding(.horizontal, 32)
    }

    private var queueList: some View {
        List {
            Section {
                ForEach(Array(items.enumerated()), id: \.element.id) { index, item in
                    QueueItemRow(item: item, position: index + 1)
                        .listRowBackground(Theme.backgroundTertiary)
                        .listRowInsets(EdgeInsets(top: 8, leading: 16, bottom: 8, trailing: 16))
                }
                .onDelete { indexSet in
                    for index in indexSet {
                        let item = items[index]
                        onRemove(item.id)
                        HapticService.lightTap()
                    }
                }
            } header: {
                Text("\(items.count) message\(items.count == 1 ? "" : "s") waiting")
                    .font(.caption)
                    .foregroundStyle(Theme.textSecondary)
            } footer: {
                Text("Swipe left to remove individual messages")
                    .font(.caption)
                    .foregroundStyle(Theme.textMuted)
            }
        }
        .listStyle(.insetGrouped)
        .scrollContentBackground(.hidden)
    }
}

struct QueueItemRow: View {
    let item: QueuedMessage
    let position: Int

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            // Position badge
            Text("\(position)")
                .font(.caption.weight(.semibold).monospacedDigit())
                .foregroundStyle(Theme.textMuted)
                .frame(width: 24, height: 24)
                .background(Theme.backgroundSecondary)
                .clipShape(Circle())

            // Message content
            VStack(alignment: .leading, spacing: 4) {
                if let agent = item.agent {
                    HStack(spacing: 4) {
                        Image(systemName: "at")
                            .font(.caption2)
                        Text(agent)
                            .font(.caption.weight(.medium))
                    }
                    .foregroundStyle(Theme.success)
                }

                Text(item.content)
                    .font(.subheadline)
                    .foregroundStyle(Theme.textPrimary)
                    .lineLimit(3)
            }

            Spacer(minLength: 0)
        }
        .padding(.vertical, 4)
    }
}

#Preview("With Items") {
    QueueSheet(
        items: [
            QueuedMessage(id: "1", content: "Can you also fix the login bug?", agent: nil),
            QueuedMessage(id: "2", content: "Run the tests after that", agent: "claude"),
            QueuedMessage(id: "3", content: "This is a much longer message that should get truncated at some point to prevent it from taking up too much space in the queue list view", agent: nil)
        ],
        onRemove: { _ in },
        onClearAll: {},
        onDismiss: {}
    )
}

#Preview("Empty") {
    QueueSheet(
        items: [],
        onRemove: { _ in },
        onClearAll: {},
        onDismiss: {}
    )
}
