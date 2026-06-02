import SwiftUI

/// Ask — the iOS surface for the non-interrupting sidecar co-pilot.
///
/// Presented as a bottom sheet (medium/large detents) over the mission. It runs
/// in its own lane: it never touches the mission's queue or the working agent.
/// Threads/messages live in a separate backend store and are rendered here with
/// a distinct cyan "co-pilot" identity.
struct AskSheet: View {
    let missionId: String
    /// Drop an Ask answer into the real mission composer (optional bridge).
    var onSendToAgent: ((String) -> Void)? = nil
    let onDismiss: () -> Void

    @State private var threads: [AskThread] = []
    @State private var threadId: String?
    @State private var messages: [AskMessage] = []
    @State private var input: String = ""
    @State private var isLoading = false
    @State private var errorText: String?

    private let api = APIService.shared
    private let copilot = Color.cyan

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                conversation
                composer
            }
            .background(Theme.backgroundSecondary)
            .navigationTitle("Ask")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { toolbarContent }
            .task { await loadThreads() }
        }
    }

    // MARK: - Conversation

    private var conversation: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 10) {
                    if messages.isEmpty && !isLoading {
                        emptyState
                    }
                    ForEach(messages) { message in
                        AskBubble(message: message, copilot: copilot, onSendToAgent: onSendToAgent)
                            .id(message.id)
                    }
                    if isLoading {
                        HStack(spacing: 6) {
                            ProgressView().controlSize(.small)
                            Text("thinking…")
                                .font(.caption)
                                .foregroundStyle(copilot.opacity(0.8))
                        }
                        .id("loading")
                    }
                    if let errorText {
                        Text(errorText)
                            .font(.caption)
                            .foregroundStyle(Theme.error)
                            .padding(8)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(Theme.error.opacity(0.1))
                            .clipShape(RoundedRectangle(cornerRadius: 8))
                    }
                }
                .padding(16)
            }
            .onChange(of: messages.count) { _, _ in
                if let last = messages.last {
                    withAnimation { proxy.scrollTo(last.id, anchor: .bottom) }
                }
            }
            .onChange(of: isLoading) { _, loading in
                if loading { withAnimation { proxy.scrollTo("loading", anchor: .bottom) } }
            }
        }
    }

    private var emptyState: some View {
        VStack(spacing: 8) {
            Image(systemName: "sparkles")
                .font(.system(size: 22))
                .foregroundStyle(copilot.opacity(0.5))
            Text("Ask about this mission — what it's doing, why, or inspect the workspace. The working agent is never interrupted.")
                .font(.footnote)
                .foregroundStyle(Theme.textMuted)
                .multilineTextAlignment(.center)
        }
        .frame(maxWidth: .infinity)
        .padding(.top, 40)
    }

    // MARK: - Composer

    private var composer: some View {
        HStack(spacing: 8) {
            TextField("Ask the co-pilot…", text: $input, axis: .vertical)
                .lineLimit(1...4)
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                .background(Theme.card)
                .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                .onSubmit { Task { await send() } }

            Button {
                Task { await send() }
            } label: {
                Image(systemName: "arrow.up.circle.fill")
                    .font(.system(size: 28))
                    .foregroundStyle(input.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || isLoading ? Theme.textMuted : copilot)
            }
            .disabled(input.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || isLoading)
        }
        .padding(12)
        .background(.ultraThinMaterial)
    }

    // MARK: - Toolbar

    @ToolbarContentBuilder
    private var toolbarContent: some ToolbarContent {
        ToolbarItem(placement: .topBarLeading) {
            Button("Done") { onDismiss() }
                .foregroundStyle(copilot)
        }
        ToolbarItem(placement: .topBarTrailing) {
            HStack(spacing: 14) {
                Menu {
                    Button {
                        newThread()
                    } label: {
                        Label("New thread", systemImage: "plus")
                    }
                    if !threads.isEmpty {
                        Divider()
                        ForEach(threads) { thread in
                            Button {
                                Task { await selectThread(thread.id) }
                            } label: {
                                Label(thread.displayTitle, systemImage: thread.id == threadId ? "checkmark" : "bubble.left")
                            }
                        }
                    }
                } label: {
                    Image(systemName: "bubble.left.and.bubble.right")
                }

                Button(role: .destructive) {
                    Task { await clearThread() }
                } label: {
                    Image(systemName: "trash")
                }
                .disabled(threadId == nil)
            }
            .foregroundStyle(copilot)
        }
    }

    // MARK: - Actions

    private func loadThreads() async {
        do {
            let fetched = try await api.listAskThreads(missionId: missionId)
            threads = fetched
            if let first = fetched.first {
                await selectThread(first.id)
            }
        } catch {
            // Non-fatal — just start with an empty thread.
        }
    }

    private func selectThread(_ id: String) async {
        threadId = id
        do {
            let detail = try await api.getAskThread(missionId: missionId, threadId: id)
            messages = detail.messages
        } catch {
            messages = []
        }
    }

    private func newThread() {
        threadId = nil
        messages = []
        input = ""
        errorText = nil
    }

    private func send() async {
        let content = input.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !content.isEmpty, !isLoading else { return }
        input = ""
        errorText = nil
        isLoading = true

        // Optimistic user bubble.
        let optimistic = AskMessage(
            id: "temp-\(UUID().uuidString)",
            threadId: threadId ?? "",
            seq: messages.count + 1,
            role: "user",
            content: content,
            toolName: nil,
            toolCallId: nil,
            createdAt: ISO8601DateFormatter().string(from: Date())
        )
        messages.append(optimistic)

        do {
            let response = try await api.postAsk(missionId: missionId, content: content, threadId: threadId)
            threadId = response.threadId
            messages = response.messages
            if let refreshed = try? await api.listAskThreads(missionId: missionId) {
                threads = refreshed
            }
        } catch {
            errorText = error.localizedDescription
            messages.removeAll { $0.id == optimistic.id }
            input = content
        }
        isLoading = false
    }

    private func clearThread() async {
        guard let id = threadId else { return }
        try? await api.deleteAskThread(missionId: missionId, threadId: id)
        if let refreshed = try? await api.listAskThreads(missionId: missionId) {
            threads = refreshed
        }
        newThread()
    }
}

// MARK: - Bubble

private struct AskBubble: View {
    let message: AskMessage
    let copilot: Color
    var onSendToAgent: ((String) -> Void)?

    var body: some View {
        if message.isUser {
            HStack {
                Spacer(minLength: 40)
                Text(message.content)
                    .font(.subheadline)
                    .foregroundStyle(Theme.textPrimary)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                    .background(Theme.card)
                    .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
            }
        } else if message.isTool {
            HStack(alignment: .top, spacing: 6) {
                Image(systemName: "terminal")
                    .font(.system(size: 10))
                    .foregroundStyle(Theme.textMuted)
                Text(toolSummary)
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(Theme.textMuted)
                    .lineLimit(3)
            }
            .padding(.leading, 24)
            .frame(maxWidth: .infinity, alignment: .leading)
        } else {
            // assistant
            HStack(alignment: .top, spacing: 8) {
                Image(systemName: "sparkles")
                    .font(.system(size: 13))
                    .foregroundStyle(copilot)
                    .padding(.top, 2)
                VStack(alignment: .leading, spacing: 6) {
                    Text(message.content)
                        .font(.subheadline)
                        .foregroundStyle(Theme.textPrimary)
                        .textSelection(.enabled)
                    if let onSendToAgent {
                        Button {
                            onSendToAgent(message.content)
                        } label: {
                            Label("Send to agent", systemImage: "arrow.uturn.left")
                                .font(.caption2)
                                .foregroundStyle(copilot.opacity(0.8))
                        }
                    }
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                .background(copilot.opacity(0.08))
                .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
            }
        }
    }

    private var toolSummary: String {
        let label = message.toolName.map { "\($0) → " } ?? "↳ "
        let body: String
        if message.isToolCall, let data = message.content.data(using: .utf8),
           let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
            body = (obj["command"] as? String) ?? (obj["path"] as? String) ?? message.content
        } else {
            body = message.content
        }
        let trimmed = body.count > 200 ? String(body.prefix(200)) + "…" : body
        return label + trimmed
    }
}
