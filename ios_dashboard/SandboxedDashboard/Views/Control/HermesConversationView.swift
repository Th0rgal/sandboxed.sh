import SwiftUI

/// A Hermes session rendered with the mission transcript's own vocabulary.
///
/// Hermes conversations and missions are two kinds of the same thing here, so
/// this view maps Hermes history and stream events onto `ChatMessage` and hands
/// them to the shared renderer (`ControlView.buildGroupedItems` →
/// `ConversationRowsView`). Tool runs group, thinking stays out of the main
/// pane, and bubbles look identical to a mission's.
///
/// Transport is the backend's Hermes proxy: history over REST, one turn per
/// `POST …/chat/stream` (named-event SSE). Turns started elsewhere (Telegram,
/// cron) land on the next idle re-poll.
struct HermesConversationView: View {
    let sessionId: String
    /// Known session metadata, when the caller already has it (avoids a fetch
    /// just to render the title).
    var session: HermesSession?

    @State private var messages: [ChatMessage] = []
    @State private var groupedItems: [GroupedChatItem] = []
    @State private var expandedToolGroups: Set<String> = []
    @State private var copiedMessageId: String?
    @State private var input = ""
    @State private var isRunning = false
    @State private var historyLoaded = false
    @State private var errorText: String?
    @State private var resolvedSession: HermesSession?
    @State private var workerMissions: [Mission] = []

    /// Id of the assistant bubble currently being streamed into.
    @State private var streamId: String?
    /// Accumulated text of the in-flight assistant turn.
    @State private var streamText = ""
    /// Open tool bubbles by name — Hermes' REST tool events carry no call id,
    /// so completions are matched FIFO against the calls of the same name.
    @State private var openToolIdsByName: [String: [String]] = [:]
    @State private var streamTask: Task<Void, Never>?
    /// Bumped on session switch / send so superseded work can bail out.
    @State private var generation = 0

    private let api = APIService.shared
    private let hermesTint = Color.indigo

    private var title: String {
        (session ?? resolvedSession)?.displayTitle ?? "Session \(sessionId.prefix(8))"
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            if !workerMissions.isEmpty {
                workerStrip
            }
            transcript
            composer
        }
        .background(Theme.backgroundPrimary)
        .task(id: sessionId) {
            generation += 1
            resetState()
            await loadHistory()
            await loadSessionMetadata()
        }
        .onDisappear {
            streamTask?.cancel()
            streamTask = nil
        }
    }

    // MARK: - Header

    private var header: some View {
        HStack(spacing: 8) {
            Image(systemName: "sparkles")
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(hermesTint)
            VStack(alignment: .leading, spacing: 1) {
                Text(title)
                    .font(.system(size: 14, weight: .medium))
                    .foregroundStyle(Theme.textPrimary)
                    .lineLimit(1)
                Text("Hermes session")
                    .font(.system(size: 11))
                    .foregroundStyle(Theme.textTertiary)
            }
            Spacer(minLength: 8)
            if isRunning {
                HStack(spacing: 5) {
                    ProgressView().controlSize(.small)
                    Text("working")
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.textSecondary)
                }
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .background(Theme.backgroundSecondary)
        .overlay(alignment: .bottom) {
            Rectangle().fill(Theme.hairline).frame(height: 0.5)
        }
    }

    /// Missions this session spawned (`origin_session_id`), as tappable chips.
    private var workerStrip: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 6) {
                Text("Workers")
                    .font(.system(size: 11))
                    .foregroundStyle(Theme.textTertiary)
                ForEach(workerMissions) { mission in
                    Button {
                        NavigationState.shared.openMission(mission.id)
                    } label: {
                        HStack(spacing: 5) {
                            StatusDot(status: mission.status.statusType)
                            Text(mission.displayTitle)
                                .font(.system(size: 11))
                                .foregroundStyle(Theme.textSecondary)
                                .lineLimit(1)
                        }
                        .padding(.horizontal, 8)
                        .padding(.vertical, 4)
                        .background(Theme.card)
                        .clipShape(Capsule())
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 6)
        }
        .background(Theme.backgroundSecondary)
    }

    // MARK: - Transcript

    private var transcript: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 10) {
                    if !historyLoaded {
                        HStack(spacing: 8) {
                            ProgressView().controlSize(.small)
                            Text("Loading session…")
                                .font(.footnote)
                                .foregroundStyle(Theme.textMuted)
                        }
                        .frame(maxWidth: .infinity)
                        .padding(.top, 40)
                    } else if groupedItems.isEmpty {
                        emptyState
                    }

                    ConversationRowsView(
                        groupedItems: groupedItems,
                        copiedMessageId: copiedMessageId,
                        expandedToolGroups: $expandedToolGroups,
                        onCopy: copy,
                        onRetry: { _ in }
                    )

                    if let errorText {
                        Text(errorText)
                            .font(.caption)
                            .foregroundStyle(Theme.error)
                            .padding(8)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(Theme.error.opacity(0.1))
                            .clipShape(RoundedRectangle(cornerRadius: 8))
                            .id("error")
                    }

                    Color.clear.frame(height: 1).id("bottom")
                }
                .padding(16)
            }
            .onChange(of: groupedItems.count) { _, _ in
                withAnimation { proxy.scrollTo("bottom", anchor: .bottom) }
            }
            .onChange(of: streamText) { _, _ in
                proxy.scrollTo("bottom", anchor: .bottom)
            }
        }
    }

    private var emptyState: some View {
        VStack(spacing: 8) {
            Image(systemName: "sparkles")
                .font(.system(size: 22))
                .foregroundStyle(hermesTint.opacity(0.5))
            Text("Send the first message to start this session.")
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
            TextField("Message Hermes…", text: $input, axis: .vertical)
                .lineLimit(1...5)
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                .background(Theme.card)
                .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))

            if isRunning {
                Button(action: stop) {
                    Image(systemName: "stop.circle.fill")
                        .font(.system(size: 28))
                        .foregroundStyle(Theme.error)
                }
                .accessibilityLabel("Stop")
            } else {
                Button(action: send) {
                    Image(systemName: "arrow.up.circle.fill")
                        .font(.system(size: 28))
                        .foregroundStyle(canSend ? hermesTint : Theme.textMuted)
                }
                .disabled(!canSend)
                .accessibilityLabel("Send")
            }
        }
        .padding(12)
        .background(.ultraThinMaterial)
    }

    private var canSend: Bool {
        !input.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && !isRunning
    }

    // MARK: - Loading

    private func resetState() {
        streamTask?.cancel()
        streamTask = nil
        messages = []
        groupedItems = []
        expandedToolGroups = []
        streamId = nil
        streamText = ""
        openToolIdsByName = [:]
        isRunning = false
        historyLoaded = false
        errorText = nil
        workerMissions = []
        resolvedSession = nil
    }

    private func loadHistory() async {
        let gen = generation
        do {
            let history = try await api.getHermesSessionMessages(sessionId: sessionId)
            guard gen == generation else { return }
            messages = HermesTranscript.chatMessages(from: history)
            regroup()
            historyLoaded = true
        } catch {
            guard gen == generation else { return }
            historyLoaded = true
            errorText = friendlyMessage(for: error)
        }
    }

    /// Session title (when not supplied) and the missions this session spawned.
    private func loadSessionMetadata() async {
        let gen = generation
        if session == nil {
            if let sessions = try? await api.listHermesSessions(limit: 200), gen == generation {
                resolvedSession = sessions.first { $0.id == sessionId }
            }
        }
        if let missions = try? await api.listMissions(), gen == generation {
            workerMissions = missions.filter { $0.originSessionId == sessionId }
        }
    }

    // MARK: - Sending

    private func send() {
        let content = input.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !content.isEmpty, !isRunning else { return }
        input = ""
        errorText = nil
        HapticService.selectionChanged()

        messages.append(
            ChatMessage(type: .user, content: content, timestamp: Date())
        )
        isRunning = true
        regroup()

        let gen = generation
        streamTask = Task {
            do {
                for try await event in api.hermesChatStream(
                    sessionId: sessionId, message: content)
                {
                    if gen != generation || Task.isCancelled { return }
                    apply(event)
                }
                if gen == generation { finishTurn() }
            } catch is CancellationError {
                if gen == generation { finishTurn() }
            } catch {
                guard gen == generation else { return }
                finishTurn()
                errorText = friendlyMessage(for: error)
            }
        }
    }

    private func stop() {
        // Aborting the request cancels the run server-side.
        streamTask?.cancel()
        streamTask = nil
        finishTurn()
    }

    // MARK: - Stream folding

    private func apply(_ event: HermesStreamEvent) {
        switch event.name {
        case "assistant.delta":
            guard let text = event.text, !text.isEmpty else { return }
            streamText += text
            upsertStreamBubble()
        case "tool.progress":
            // Hermes surfaces reasoning as progress on the `_thinking` tool.
            guard event.toolName == "_thinking", let text = event.text, !text.isEmpty else {
                return
            }
            appendThinking(text)
        case "tool.started":
            startTool(named: event.toolName ?? "tool", preview: event.preview)
        case "tool.completed":
            completeTool(named: event.toolName ?? "tool", result: event.preview, failed: false)
        case "tool.failed":
            completeTool(
                named: event.toolName ?? "tool",
                result: event.preview ?? event.message ?? "failed",
                failed: true
            )
        case "assistant.completed":
            // Authoritative final text for the turn — replaces the deltas.
            if let text = event.text, !text.isEmpty {
                streamText = text
            }
            flushAssistantBubble()
        case "run.completed", "done":
            finishTurn()
        default:
            break
        }
    }

    private func upsertStreamBubble() {
        if let streamId, let index = messages.firstIndex(where: { $0.id == streamId }) {
            messages[index].content = streamText
        } else {
            let id = "hs-stream-\(UUID().uuidString)"
            streamId = id
            messages.append(
                ChatMessage(
                    id: id,
                    type: .assistant(
                        success: true, costCents: 0, costSource: .unknown,
                        model: nil, sharedFiles: nil),
                    content: streamText
                )
            )
        }
        regroup()
    }

    private func appendThinking(_ text: String) {
        if let index = messages.lastIndex(where: { $0.isThinking }),
            case .thinking(let done, _) = messages[index].type, !done
        {
            messages[index].content += text
        } else {
            messages.append(
                ChatMessage(
                    type: .thinking(done: false, startTime: Date()),
                    content: text
                )
            )
        }
        regroup()
    }

    private func finalizeThinking() {
        guard let index = messages.lastIndex(where: { $0.isThinking }),
            case .thinking(let done, let start) = messages[index].type, !done
        else { return }
        let existing = messages[index]
        messages[index] = ChatMessage(
            id: existing.id,
            type: .thinking(done: true, startTime: start),
            content: existing.content,
            timestamp: existing.timestamp
        )
    }

    private func startTool(named name: String, preview: String?) {
        // A tool run ends the current assistant segment; flush it so the tool
        // bubble lands after the text that introduced it.
        flushAssistantBubble()
        let callId = "hs-tool-\(UUID().uuidString)"
        openToolIdsByName[name, default: []].append(callId)
        messages.append(
            ChatMessage(
                id: callId,
                type: .toolCall(name: name, isActive: true),
                content: preview ?? "",
                toolData: ToolCallData(
                    toolCallId: callId,
                    name: name,
                    args: preview.map { ["preview": $0] } ?? [:],
                    startTime: Date(),
                    endTime: nil,
                    result: nil,
                    state: .running
                )
            )
        )
        regroup()
    }

    private func completeTool(named name: String, result: String?, failed: Bool) {
        guard var open = openToolIdsByName[name], !open.isEmpty else { return }
        let callId = open.removeFirst()
        openToolIdsByName[name] = open
        guard let index = messages.firstIndex(where: { $0.id == callId }) else { return }
        var data = messages[index].toolData
        data?.endTime = Date()
        data?.result = result
        data?.state = failed ? .error : .success
        let existing = messages[index]
        messages[index] = ChatMessage(
            id: existing.id,
            type: .toolCall(name: name, isActive: false),
            content: existing.content,
            toolData: data,
            timestamp: existing.timestamp
        )
        regroup()
    }

    /// Close the in-flight assistant bubble (if any) so later rows append after it.
    private func flushAssistantBubble() {
        finalizeThinking()
        guard !streamText.isEmpty else {
            streamId = nil
            return
        }
        upsertStreamBubble()
        streamId = nil
        streamText = ""
    }

    private func finishTurn() {
        flushAssistantBubble()
        finalizeThinking()
        // Any tool left open (stream cut mid-run) would spin forever.
        for (name, ids) in openToolIdsByName {
            for id in ids {
                guard let index = messages.firstIndex(where: { $0.id == id }) else { continue }
                var data = messages[index].toolData
                data?.endTime = Date()
                data?.state = .cancelled
                let existing = messages[index]
                messages[index] = ChatMessage(
                    id: existing.id,
                    type: .toolCall(name: name, isActive: false),
                    content: existing.content,
                    toolData: data,
                    timestamp: existing.timestamp
                )
            }
        }
        openToolIdsByName = [:]
        isRunning = false
        streamTask = nil
        regroup()
    }

    // MARK: - Helpers

    private func regroup() {
        groupedItems = ControlView.buildGroupedItems(from: messages)
    }

    private func copy(_ message: ChatMessage) {
        UIPasteboard.general.string = message.content
        copiedMessageId = message.id
        HapticService.selectionChanged()
        Task {
            try? await Task.sleep(for: .seconds(2))
            if copiedMessageId == message.id { copiedMessageId = nil }
        }
    }

    private func friendlyMessage(for error: Error) -> String {
        if let apiError = error as? APIError {
            switch apiError {
            case .httpError(let status, _) where status == 502 || status == 503:
                return "Hermes is not reachable from this server."
            default:
                break
            }
        }
        return error.localizedDescription
    }
}

// MARK: - History mapping

/// Maps persisted Hermes messages onto the shared `ChatMessage` model.
enum HermesTranscript {
    static func chatMessages(from history: [HermesMessage]) -> [ChatMessage] {
        var result: [ChatMessage] = []
        // Assistant tool_calls create the bubble; the later `tool` role message
        // carrying the same id fills in its result.
        var indexByToolCallId: [String: Int] = [:]

        for message in history {
            let timestamp = message.timestamp.map { Date(timeIntervalSince1970: $0) } ?? Date()
            let content = (message.content ?? "").trimmingCharacters(in: .whitespacesAndNewlines)

            switch message.role {
            case "user":
                guard !content.isEmpty else { continue }
                result.append(
                    ChatMessage(
                        id: message.id, type: .user, content: content, timestamp: timestamp))

            case "assistant":
                if let reasoning = message.reasoningText {
                    result.append(
                        ChatMessage(
                            id: "\(message.id)-thinking",
                            type: .thinking(done: true, startTime: timestamp),
                            content: reasoning,
                            timestamp: timestamp
                        )
                    )
                }
                if !content.isEmpty {
                    result.append(
                        ChatMessage(
                            id: message.id,
                            type: .assistant(
                                success: true, costCents: 0, costSource: .unknown,
                                model: nil, sharedFiles: nil),
                            content: content,
                            timestamp: timestamp
                        )
                    )
                }
                for call in message.toolCalls ?? [] {
                    let callId = call.id ?? "\(message.id)-\(call.toolName)"
                    indexByToolCallId[callId] = result.count
                    result.append(
                        ChatMessage(
                            id: "hermes-tool-\(callId)",
                            type: .toolCall(name: call.toolName, isActive: false),
                            content: "",
                            toolData: ToolCallData(
                                toolCallId: callId,
                                name: call.toolName,
                                args: decodeArguments(call.function?.arguments),
                                startTime: timestamp,
                                endTime: timestamp,
                                // Historical calls whose result row never got
                                // persisted must not render as still-running.
                                result: "",
                                state: .success
                            ),
                            timestamp: timestamp
                        )
                    )
                }

            case "tool":
                if let callId = message.toolCallId, let index = indexByToolCallId[callId] {
                    var data = result[index].toolData
                    data?.result = content
                    data?.endTime = timestamp
                    let existing = result[index]
                    result[index] = ChatMessage(
                        id: existing.id,
                        type: existing.type,
                        content: existing.content,
                        toolData: data,
                        timestamp: existing.timestamp
                    )
                } else if !content.isEmpty {
                    let name = message.toolName ?? "tool"
                    result.append(
                        ChatMessage(
                            id: message.id,
                            type: .toolCall(name: name, isActive: false),
                            content: "",
                            toolData: ToolCallData(
                                toolCallId: message.toolCallId ?? message.id,
                                name: name,
                                args: [:],
                                startTime: timestamp,
                                endTime: timestamp,
                                result: content,
                                state: .success
                            ),
                            timestamp: timestamp
                        )
                    )
                }

            default:
                continue
            }
        }
        return result
    }

    private static func decodeArguments(_ raw: String?) -> [String: Any] {
        guard let raw, let data = raw.data(using: .utf8),
            let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else {
            return raw.map { ["arguments": $0] } ?? [:]
        }
        return object
    }
}
