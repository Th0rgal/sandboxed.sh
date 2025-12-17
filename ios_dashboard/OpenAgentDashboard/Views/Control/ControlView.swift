//
//  ControlView.swift
//  OpenAgentDashboard
//
//  Chat interface for the AI agent with real-time streaming
//

import SwiftUI

struct ControlView: View {
    @State private var messages: [ChatMessage] = []
    @State private var inputText = ""
    @State private var runState: ControlRunState = .idle
    @State private var queueLength = 0
    @State private var currentMission: Mission?
    @State private var isLoading = true
    @State private var streamTask: Task<Void, Never>?
    @State private var showMissionMenu = false
    @State private var shouldScrollToBottom = false
    
    @FocusState private var isInputFocused: Bool
    
    private let api = APIService.shared
    private let nav = NavigationState.shared
    private let bottomAnchorId = "bottom-anchor"
    
    var body: some View {
        ZStack {
            // Background with subtle accent glow
            Theme.backgroundPrimary.ignoresSafeArea()
            
            // Subtle radial gradients for liquid glass refraction
            backgroundGlows
            
            VStack(spacing: 0) {
                // Messages
                messagesView
                
                // Input area
                inputView
            }
        }
        .navigationTitle(currentMission?.displayTitle ?? "Control")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) {
                VStack(spacing: 2) {
                    Text(currentMission?.displayTitle ?? "Control")
                        .font(.headline)
                        .foregroundStyle(Theme.textPrimary)
                    
                    HStack(spacing: 4) {
                        StatusDot(status: runState.statusType, size: 5)
                        Text(runState.label)
                            .font(.caption2)
                            .foregroundStyle(Theme.textSecondary)
                        
                        if queueLength > 0 {
                            Text("• \(queueLength) queued")
                                .font(.caption2)
                                .foregroundStyle(Theme.textTertiary)
                        }
                    }
                }
            }
            
            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    Button {
                        Task { await createNewMission() }
                    } label: {
                        Label("New Mission", systemImage: "plus")
                    }
                    
                    if let mission = currentMission {
                        Divider()
                        
                        Button {
                            Task { await setMissionStatus(.completed) }
                        } label: {
                            Label("Mark Complete", systemImage: "checkmark.circle")
                        }
                        
                        Button(role: .destructive) {
                            Task { await setMissionStatus(.failed) }
                        } label: {
                            Label("Mark Failed", systemImage: "xmark.circle")
                        }
                        
                        if mission.status != .active {
                            Button {
                                Task { await setMissionStatus(.active) }
                            } label: {
                                Label("Reactivate", systemImage: "arrow.clockwise")
                            }
                        }
                    }
                } label: {
                    Image(systemName: "ellipsis.circle")
                        .font(.body)
                }
            }
        }
        .task {
            // Check if we're being opened with a specific mission from History
            if let pendingId = nav.consumePendingMission() {
                await loadMission(id: pendingId)
            } else {
                await loadCurrentMission()
            }
            startStreaming()
        }
        .onChange(of: nav.pendingMissionId) { _, newId in
            // Handle navigation from History while Control is already visible
            if let missionId = newId {
                nav.pendingMissionId = nil
                Task {
                    await loadMission(id: missionId)
                }
            }
        }
        .onDisappear {
            streamTask?.cancel()
        }
    }
    
    // MARK: - Background
    
    private var backgroundGlows: some View {
        ZStack {
            RadialGradient(
                colors: [Theme.accent.opacity(0.08), .clear],
                center: .topTrailing,
                startRadius: 20,
                endRadius: 400
            )
            .ignoresSafeArea()
            .allowsHitTesting(false)
            
            RadialGradient(
                colors: [Color.white.opacity(0.03), .clear],
                center: .bottomLeading,
                startRadius: 30,
                endRadius: 500
            )
            .ignoresSafeArea()
            .allowsHitTesting(false)
        }
    }
    
    // MARK: - Header (now in toolbar)
    
    private var headerView: some View {
        EmptyView() // Moved to navigation bar
    }
    
    // MARK: - Messages
    
    private var messagesView: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(spacing: 16) {
                    if messages.isEmpty && !isLoading {
                        emptyStateView
                    } else if isLoading {
                        LoadingView(message: "Loading conversation...")
                            .frame(height: 200)
                    } else {
                        ForEach(messages) { message in
                            MessageBubble(message: message)
                                .id(message.id)
                        }
                    }
                    
                    // Bottom anchor for scrolling past last message
                    Color.clear
                        .frame(height: 1)
                        .id(bottomAnchorId)
                }
                .padding()
            }
            .onTapGesture {
                // Dismiss keyboard when tapping on messages area
                isInputFocused = false
            }
            .onChange(of: messages.count) { _, _ in
                scrollToBottom(proxy: proxy)
            }
            .onChange(of: shouldScrollToBottom) { _, shouldScroll in
                if shouldScroll {
                    scrollToBottom(proxy: proxy)
                    shouldScrollToBottom = false
                }
            }
        }
    }
    
    private func scrollToBottom(proxy: ScrollViewProxy) {
        withAnimation {
            proxy.scrollTo(bottomAnchorId, anchor: .bottom)
        }
    }
    
    private var emptyStateView: some View {
        VStack(spacing: 32) {
            Spacer()
            
            // Animated brain icon
            Image(systemName: "brain")
                .font(.system(size: 56, weight: .light))
                .foregroundStyle(
                    LinearGradient(
                        colors: [Theme.accent, Theme.accent.opacity(0.6)],
                        startPoint: .topLeading,
                        endPoint: .bottomTrailing
                    )
                )
                .symbolEffect(.pulse, options: .repeating.speed(0.5))
            
            VStack(spacing: 12) {
                Text("Ready to Help")
                    .font(.title2.bold())
                    .foregroundStyle(Theme.textPrimary)
                
                Text("Send a message to start working\nwith the AI agent")
                    .font(.subheadline)
                    .foregroundStyle(Theme.textSecondary)
                    .multilineTextAlignment(.center)
                    .lineSpacing(4)
            }
            
            // Suggestion chips
            VStack(spacing: 10) {
                Text("Try asking:")
                    .font(.caption)
                    .foregroundStyle(Theme.textMuted)
                
                FlowLayout(spacing: 8) {
                    suggestionChip("Write a Python script")
                    suggestionChip("Debug this error")
                    suggestionChip("Explain this code")
                    suggestionChip("Create a file")
                }
            }
            .padding(.top, 8)
            
            Spacer()
            Spacer()
        }
        .padding(.horizontal, 32)
    }
    
    private func suggestionChip(_ text: String) -> some View {
        Button {
            inputText = text
            isInputFocused = true
        } label: {
            Text(text)
                .font(.caption.weight(.medium))
                .foregroundStyle(Theme.textSecondary)
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                .background(Theme.backgroundSecondary)
                .clipShape(Capsule())
                .overlay(
                    Capsule()
                        .stroke(Theme.border, lineWidth: 1)
                )
        }
    }
    
    // MARK: - Input
    
    private var inputView: some View {
        VStack(spacing: 0) {
            Divider()
                .background(Theme.border)
            
            HStack(alignment: .bottom, spacing: 12) {
                // Text input
                TextField("Message the agent...", text: $inputText, axis: .vertical)
                    .textFieldStyle(.plain)
                    .font(.body)
                    .lineLimit(1...5)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 12)
                    .background(Theme.backgroundSecondary)
                    .clipShape(RoundedRectangle(cornerRadius: 22, style: .continuous))
                    .overlay(
                        RoundedRectangle(cornerRadius: 22, style: .continuous)
                            .stroke(isInputFocused ? Theme.accent.opacity(0.4) : Theme.border, lineWidth: 1)
                    )
                    .focused($isInputFocused)
                    .submitLabel(.send)
                    .onSubmit {
                        sendMessage()
                    }
                
                // Send/Stop button
                Button {
                    if runState != .idle {
                        Task { await cancelRun() }
                    } else {
                        sendMessage()
                    }
                } label: {
                    Image(systemName: runState != .idle ? "stop.fill" : "arrow.up")
                        .font(.system(size: 16, weight: .bold))
                        .foregroundStyle(.white)
                        .frame(width: 36, height: 36)
                        .background(
                            runState != .idle ? Theme.error :
                            (inputText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? Theme.textMuted : Theme.accent)
                        )
                        .clipShape(Circle())
                }
                .disabled(runState == .idle && inputText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                .animation(.easeInOut(duration: 0.15), value: runState)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 12)
            .background(.thinMaterial)
        }
    }
    
    // MARK: - Actions
    
    private func loadCurrentMission() async {
        isLoading = true
        defer { isLoading = false }

        do {
            if let mission = try await api.getCurrentMission() {
                currentMission = mission
                messages = mission.history.enumerated().map { index, entry in
                    ChatMessage(
                        id: "\(mission.id)-\(index)",
                        type: entry.isUser ? .user : .assistant(success: true, costCents: 0, model: nil),
                        content: entry.content
                    )
                }
                
                // Scroll to bottom after loading
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) {
                    shouldScrollToBottom = true
                }
            }
        } catch {
            print("Failed to load mission: \(error)")
        }
    }
    
    private func loadMission(id: String) async {
        isLoading = true
        defer { isLoading = false }

        do {
            let missions = try await api.listMissions()
            if let mission = missions.first(where: { $0.id == id }) {
                currentMission = mission
                messages = mission.history.enumerated().map { index, entry in
                    ChatMessage(
                        id: "\(mission.id)-\(index)",
                        type: entry.isUser ? .user : .assistant(success: true, costCents: 0, model: nil),
                        content: entry.content
                    )
                }
                HapticService.success()
                
                // Scroll to bottom after loading
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) {
                    shouldScrollToBottom = true
                }
            }
        } catch {
            print("Failed to load mission: \(error)")
        }
    }
    
    private func createNewMission() async {
        do {
            let mission = try await api.createMission()
            currentMission = mission
            messages = []
            HapticService.success()
        } catch {
            print("Failed to create mission: \(error)")
            HapticService.error()
        }
    }
    
    private func setMissionStatus(_ status: MissionStatus) async {
        guard let mission = currentMission else { return }
        
        do {
            try await api.setMissionStatus(id: mission.id, status: status)
            currentMission?.status = status
            HapticService.success()
        } catch {
            print("Failed to set status: \(error)")
            HapticService.error()
        }
    }
    
    private func sendMessage() {
        let content = inputText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !content.isEmpty else { return }
        
        inputText = ""
        HapticService.lightTap()
        
        Task {
            do {
                let _ = try await api.sendMessage(content: content)
            } catch {
                print("Failed to send message: \(error)")
                HapticService.error()
            }
        }
    }
    
    private func cancelRun() async {
        do {
            try await api.cancelControl()
            HapticService.success()
        } catch {
            print("Failed to cancel: \(error)")
            HapticService.error()
        }
    }
    
    private func startStreaming() {
        streamTask = api.streamControl { eventType, data in
            Task { @MainActor in
                handleStreamEvent(type: eventType, data: data)
            }
        }
    }
    
    private func handleStreamEvent(type: String, data: [String: Any]) {
        switch type {
        case "status":
            if let state = data["state"] as? String {
                runState = ControlRunState(rawValue: state) ?? .idle
            }
            if let queue = data["queue_len"] as? Int {
                queueLength = queue
            }
            
        case "user_message":
            if let content = data["content"] as? String,
               let id = data["id"] as? String {
                let message = ChatMessage(id: id, type: .user, content: content)
                messages.append(message)
            }
            
        case "assistant_message":
            if let content = data["content"] as? String,
               let id = data["id"] as? String {
                let success = data["success"] as? Bool ?? true
                let costCents = data["cost_cents"] as? Int ?? 0
                let model = data["model"] as? String
                
                // Remove any incomplete thinking messages
                messages.removeAll { $0.isThinking && !$0.thinkingDone }
                
                let message = ChatMessage(
                    id: id,
                    type: .assistant(success: success, costCents: costCents, model: model),
                    content: content
                )
                messages.append(message)
            }
            
        case "thinking":
            if let content = data["content"] as? String {
                let done = data["done"] as? Bool ?? false
                
                // Find existing thinking message or create new
                if let index = messages.lastIndex(where: { $0.isThinking && !$0.thinkingDone }) {
                    messages[index].content += "\n\n---\n\n" + content
                    if done {
                        messages[index] = ChatMessage(
                            id: messages[index].id,
                            type: .thinking(done: true, startTime: Date()),
                            content: messages[index].content
                        )
                    }
                } else if !done {
                    let message = ChatMessage(
                        id: "thinking-\(Date().timeIntervalSince1970)",
                        type: .thinking(done: false, startTime: Date()),
                        content: content
                    )
                    messages.append(message)
                }
            }
            
        case "error":
            if let errorMessage = data["message"] as? String {
                let message = ChatMessage(
                    id: "error-\(Date().timeIntervalSince1970)",
                    type: .error,
                    content: errorMessage
                )
                messages.append(message)
            }
            
        case "tool_call":
            if let toolCallId = data["tool_call_id"] as? String,
               let name = data["name"] as? String,
               let args = data["args"] as? [String: Any] {
                // Parse UI tool calls
                if let toolUI = ToolUIContent.parse(name: name, args: args) {
                    let message = ChatMessage(
                        id: toolCallId,
                        type: .toolUI(name: name),
                        content: "",
                        toolUI: toolUI
                    )
                    messages.append(message)
                }
            }
            
        default:
            break
        }
    }
}

// MARK: - Message Bubble

private struct MessageBubble: View {
    let message: ChatMessage
    
    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            if message.isUser {
                Spacer(minLength: 60)
                userBubble
            } else if message.isThinking {
                thinkingBubble
                Spacer(minLength: 60)
            } else if message.isToolUI {
                toolUIBubble
                Spacer(minLength: 40)
            } else {
                assistantBubble
                Spacer(minLength: 60)
            }
        }
    }
    
    @ViewBuilder
    private var toolUIBubble: some View {
        if let toolUI = message.toolUI {
            ToolUIView(content: toolUI)
        }
    }
    
    private var userBubble: some View {
        VStack(alignment: .trailing, spacing: 4) {
            Text(message.content)
                .font(.body)
                .foregroundStyle(.white)
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
                .background(Theme.accent)
                .clipShape(RoundedRectangle(cornerRadius: 20, style: .continuous))
                .clipShape(
                    .rect(
                        topLeadingRadius: 20,
                        bottomLeadingRadius: 20,
                        bottomTrailingRadius: 6,
                        topTrailingRadius: 20
                    )
                )
        }
    }
    
    private var assistantBubble: some View {
        VStack(alignment: .leading, spacing: 8) {
            // Status header for assistant messages
            if case .assistant(let success, _, _) = message.type {
                HStack(spacing: 6) {
                    Image(systemName: success ? "checkmark.circle.fill" : "xmark.circle.fill")
                        .font(.caption2)
                        .foregroundStyle(success ? Theme.success : Theme.error)
                    
                    if let model = message.displayModel {
                        Text(model)
                            .font(.caption2.monospaced())
                            .foregroundStyle(Theme.textTertiary)
                    }
                    
                    if let cost = message.costFormatted {
                        Text("•")
                            .foregroundStyle(Theme.textMuted)
                        Text(cost)
                            .font(.caption2.monospaced())
                            .foregroundStyle(Theme.success)
                    }
                }
            }
            
            MarkdownText(message.content)
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
                .background(.ultraThinMaterial)
                .clipShape(
                    .rect(
                        topLeadingRadius: 20,
                        bottomLeadingRadius: 6,
                        bottomTrailingRadius: 20,
                        topTrailingRadius: 20
                    )
                )
                .overlay(
                    RoundedRectangle(cornerRadius: 20, style: .continuous)
                        .stroke(Theme.border, lineWidth: 0.5)
                )
        }
    }
    
    private var thinkingBubble: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                Image(systemName: "brain")
                    .font(.caption)
                    .foregroundStyle(Theme.accent)
                    .symbolEffect(.pulse, options: message.thinkingDone ? .nonRepeating : .repeating)
                
                Text(message.thinkingDone ? "Thought" : "Thinking...")
                    .font(.caption)
                    .foregroundStyle(Theme.textSecondary)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .background(Theme.accent.opacity(0.1))
            .clipShape(Capsule())
            
            if !message.content.isEmpty {
                Text(message.content)
                    .font(.caption)
                    .foregroundStyle(Theme.textTertiary)
                    .lineLimit(message.thinkingDone ? 3 : nil)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                    .background(Color.white.opacity(0.02))
                    .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
            }
        }
    }
}


// MARK: - Flow Layout

private struct FlowLayout: Layout {
    var spacing: CGFloat = 8
    
    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let result = FlowResult(in: proposal.width ?? 0, spacing: spacing, subviews: subviews)
        return result.size
    }
    
    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let result = FlowResult(in: bounds.width, spacing: spacing, subviews: subviews)
        for (index, subview) in subviews.enumerated() {
            subview.place(at: CGPoint(x: bounds.minX + result.positions[index].x,
                                       y: bounds.minY + result.positions[index].y),
                          proposal: .unspecified)
        }
    }
    
    struct FlowResult {
        var size: CGSize = .zero
        var positions: [CGPoint] = []
        
        init(in maxWidth: CGFloat, spacing: CGFloat, subviews: Subviews) {
            var x: CGFloat = 0
            var y: CGFloat = 0
            var rowHeight: CGFloat = 0
            
            for subview in subviews {
                let size = subview.sizeThatFits(.unspecified)
                
                if x + size.width > maxWidth && x > 0 {
                    x = 0
                    y += rowHeight + spacing
                    rowHeight = 0
                }
                
                positions.append(CGPoint(x: x, y: y))
                rowHeight = max(rowHeight, size.height)
                x += size.width + spacing
                self.size.width = max(self.size.width, x)
            }
            
            self.size.height = y + rowHeight
        }
    }
}

// MARK: - Markdown Text with Image Support

private struct MarkdownText: View {
    let content: String
    
    init(_ content: String) {
        self.content = content
    }
    
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(parseMarkdownContent().enumerated()), id: \.offset) { _, element in
                switch element {
                case .text(let text):
                    if let attributed = try? AttributedString(markdown: text, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)) {
                        Text(attributed)
                            .font(.body)
                            .foregroundStyle(Theme.textPrimary)
                            .tint(Theme.accent)
                    } else {
                        Text(text)
                            .font(.body)
                            .foregroundStyle(Theme.textPrimary)
                    }
                case .image(let alt, let url):
                    MarkdownImageView(url: url, alt: alt)
                }
            }
        }
    }
    
    /// Parse markdown content into text and image elements
    private func parseMarkdownContent() -> [MarkdownElement] {
        var elements: [MarkdownElement] = []
        var remaining = content
        
        // Regex to match markdown images: ![alt](url)
        let imagePattern = #/!\[([^\]]*)\]\(([^)]+)\)/#
        
        while let match = remaining.firstMatch(of: imagePattern) {
            // Add text before the image
            let textBefore = String(remaining[remaining.startIndex..<match.range.lowerBound])
            if !textBefore.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                elements.append(.text(textBefore))
            }
            
            // Add the image
            let alt = String(match.output.1)
            let url = String(match.output.2)
            elements.append(.image(alt: alt, url: url))
            
            // Continue with remaining content
            remaining = String(remaining[match.range.upperBound...])
        }
        
        // Add any remaining text
        if !remaining.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            elements.append(.text(remaining))
        }
        
        // If no elements were parsed, just return the original text
        if elements.isEmpty && !content.isEmpty {
            elements.append(.text(content))
        }
        
        return elements
    }
}

/// Represents a parsed markdown element
private enum MarkdownElement {
    case text(String)
    case image(alt: String, url: String)
}

/// View for displaying markdown images with loading state
private struct MarkdownImageView: View {
    let url: String
    let alt: String
    
    var body: some View {
        if let imageURL = URL(string: url) {
            AsyncImage(url: imageURL) { phase in
                switch phase {
                case .empty:
                    HStack(spacing: 8) {
                        ProgressView()
                            .controlSize(.small)
                        Text("Loading image...")
                            .font(.caption)
                            .foregroundStyle(Theme.textSecondary)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding()
                    .background(Color.white.opacity(0.05))
                    .clipShape(RoundedRectangle(cornerRadius: 12))
                    
                case .success(let image):
                    VStack(alignment: .leading, spacing: 4) {
                        image
                            .resizable()
                            .aspectRatio(contentMode: .fit)
                            .frame(maxWidth: .infinity, maxHeight: 400)
                            .clipShape(RoundedRectangle(cornerRadius: 12))
                            .overlay(
                                RoundedRectangle(cornerRadius: 12)
                                    .stroke(Theme.border, lineWidth: 0.5)
                            )
                        
                        if !alt.isEmpty {
                            Text(alt)
                                .font(.caption)
                                .foregroundStyle(Theme.textTertiary)
                                .italic()
                        }
                    }
                    
                case .failure:
                    HStack(spacing: 8) {
                        Image(systemName: "photo.badge.exclamationmark")
                            .foregroundStyle(Theme.error)
                        Text("Failed to load image")
                            .font(.caption)
                            .foregroundStyle(Theme.textSecondary)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding()
                    .background(Color.white.opacity(0.05))
                    .clipShape(RoundedRectangle(cornerRadius: 12))
                    
                @unknown default:
                    EmptyView()
                }
            }
        } else {
            HStack(spacing: 8) {
                Image(systemName: "link.badge.plus")
                    .foregroundStyle(Theme.warning)
                Text("Invalid image URL")
                    .font(.caption)
                    .foregroundStyle(Theme.textSecondary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding()
            .background(Color.white.opacity(0.05))
            .clipShape(RoundedRectangle(cornerRadius: 12))
        }
    }
}

#Preview {
    NavigationStack {
        ControlView()
    }
}
