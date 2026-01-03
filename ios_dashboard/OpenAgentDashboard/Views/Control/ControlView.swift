//
//  ControlView.swift
//  OpenAgentDashboard
//
//  Chat interface for the AI agent with real-time streaming
//

import SwiftUI
import os

struct ControlView: View {
    @State private var messages: [ChatMessage] = []
    @State private var inputText = ""
    @State private var runState: ControlRunState = .idle
    @State private var queueLength = 0
    @State private var currentMission: Mission?
    @State private var viewingMission: Mission?
    @State private var isLoading = true
    @State private var streamTask: Task<Void, Never>?
    @State private var showMissionMenu = false
    @State private var shouldScrollToBottom = false
    @State private var progress: ExecutionProgress?
    @State private var isAtBottom = true
    @State private var copiedMessageId: String?

    // Connection state for SSE stream - starts as disconnected until first event received
    @State private var connectionState: ConnectionState = .disconnected
    @State private var reconnectAttempt = 0

    // Parallel missions state
    @State private var runningMissions: [RunningMissionInfo] = []
    @State private var viewingMissionId: String?
    @State private var showRunningMissions = false
    @State private var pollingTask: Task<Void, Never>?

    // Track pending fetch to prevent race conditions
    @State private var fetchingMissionId: String?

    // Desktop stream state
    @State private var showDesktopStream = false
    @State private var desktopDisplayId = ":99"

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
                // Running missions bar (when there are parallel missions)
                if showRunningMissions && (!runningMissions.isEmpty || currentMission != nil) {
                    runningMissionsBar
                }
                
                // Messages
                messagesView
                
                // Input area
                inputView
            }
        }
        .navigationTitle(viewingMission?.displayTitle ?? "Control")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) {
                VStack(spacing: 2) {
                    Text(viewingMission?.displayTitle ?? "Control")
                        .font(.headline)
                        .foregroundStyle(Theme.textPrimary)

                    HStack(spacing: 4) {
                        // Show connection state or run state
                        if !connectionState.isConnected {
                            // Connection issue - show reconnecting/disconnected state
                            Image(systemName: connectionState.icon)
                                .font(.system(size: 9))
                                .foregroundStyle(Theme.warning)
                                .symbolEffect(.pulse, options: .repeating)
                            Text(connectionState.label)
                                .font(.caption2)
                                .foregroundStyle(Theme.warning)
                        } else {
                            // Connected - show normal run state
                            StatusDot(status: runState.statusType, size: 5)
                            Text(runState.label)
                                .font(.caption2)
                                .foregroundStyle(Theme.textSecondary)

                            if queueLength > 0 {
                                Text("• \(queueLength) queued")
                                    .font(.caption2)
                                    .foregroundStyle(Theme.textTertiary)
                            }

                            // Progress indicator
                            if let progress = progress, progress.total > 0 {
                                Text("•")
                                    .foregroundStyle(Theme.textMuted)
                                Text(progress.displayText)
                                    .font(.caption2.weight(.medium))
                                    .foregroundStyle(Theme.success)
                            }
                        }
                    }
                }
            }
            
            ToolbarItem(placement: .topBarLeading) {
                // Running missions toggle
                Button {
                    withAnimation(.easeInOut(duration: 0.2)) {
                        showRunningMissions.toggle()
                    }
                    HapticService.selectionChanged()
                } label: {
                    HStack(spacing: 4) {
                        Image(systemName: "square.stack.3d.up")
                            .font(.system(size: 14))
                        if !runningMissions.isEmpty {
                            Text("\(runningMissions.count)")
                                .font(.caption2.weight(.semibold))
                        }
                    }
                    .foregroundStyle(showRunningMissions ? Theme.accent : Theme.textSecondary)
                }
            }
            
            ToolbarItem(placement: .topBarTrailing) {
                // Desktop stream button
                Button {
                    showDesktopStream = true
                    HapticService.lightTap()
                } label: {
                    Image(systemName: "display")
                        .font(.system(size: 14))
                        .foregroundStyle(Theme.textSecondary)
                }
            }

            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    Button {
                        Task { await createNewMission() }
                    } label: {
                        Label("New Mission", systemImage: "plus")
                    }

                    // Desktop stream option in menu too
                    Button {
                        showDesktopStream = true
                    } label: {
                        Label("View Desktop", systemImage: "display")
                    }

                    if let mission = viewingMission {
                        Divider()

                        // Resume button for interrupted/blocked missions
                        if mission.canResume {
                            Button {
                                Task { await resumeMission() }
                            } label: {
                                Label("Resume Mission", systemImage: "play.circle")
                            }
                        }

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

                        if mission.status != .active && !mission.canResume {
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
                // Also load the current mission in the background for main-session context
                await loadCurrentMission(updateViewing: false)
            } else {
                await loadCurrentMission(updateViewing: true)
            }
            
            // Fetch initial running missions
            await refreshRunningMissions()
            
            // Auto-show bar if there are multiple running missions
            if runningMissions.count > 1 {
                showRunningMissions = true
            }
            
            startStreaming()
            startPollingRunningMissions()
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
        .onChange(of: currentMission?.id) { _, newId in
            // Sync viewing mission with current mission if nothing is being viewed yet
            if viewingMissionId == nil, let id = newId, let mission = currentMission, mission.id == id {
                applyViewingMission(mission)
            }
        }
        .onDisappear {
            streamTask?.cancel()
            connectionState = .disconnected
            reconnectAttempt = 0
            pollingTask?.cancel()
        }
        .sheet(isPresented: $showDesktopStream) {
            DesktopStreamView(displayId: desktopDisplayId)
                .presentationDetents([.medium, .large])
                .presentationDragIndicator(.visible)
                .presentationBackgroundInteraction(.enabled(upThrough: .medium))
        }
    }
    
    // MARK: - Running Missions Bar
    
    private var runningMissionsBar: some View {
        RunningMissionsBar(
            runningMissions: runningMissions,
            currentMission: currentMission,
            viewingMissionId: viewingMissionId,
            onSelectMission: { missionId in
                Task { await switchToMission(id: missionId) }
            },
            onCancelMission: { missionId in
                Task { await cancelMission(id: missionId) }
            },
            onRefresh: {
                Task { await refreshRunningMissions() }
            }
        )
        .transition(AnyTransition.move(edge: .top).combined(with: .opacity))
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
        ZStack(alignment: .bottom) {
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(spacing: 16) {
                        if messages.isEmpty && !isLoading {
                            // Show working indicator when this specific mission is running but no messages yet
                            if viewingMissionIsRunning {
                                agentWorkingIndicator
                            } else {
                                emptyStateView
                            }
                        } else if isLoading {
                            LoadingView(message: "Loading conversation...")
                                .frame(height: 200)
                        } else {
                            ForEach(messages) { message in
                                MessageBubble(
                                    message: message,
                                    isCopied: copiedMessageId == message.id,
                                    onCopy: { copyMessage(message) }
                                )
                                .id(message.id)
                            }

                            // Show working indicator after messages when this mission is running but no active streaming item
                            if viewingMissionIsRunning && !hasActiveStreamingItem {
                                agentWorkingIndicator
                            }
                        }
                        
                        // Bottom anchor for scrolling past last message
                        Color.clear
                            .frame(height: 1)
                            .id(bottomAnchorId)
                    }
                    .padding()
                    .background(
                        GeometryReader { geo in
                            Color.clear.preference(
                                key: ScrollOffsetPreferenceKey.self,
                                value: geo.frame(in: .named("scroll")).maxY
                            )
                        }
                    )
                }
                .coordinateSpace(name: "scroll")
                .onPreferenceChange(ScrollOffsetPreferenceKey.self) { maxY in
                    // Check if we're at the bottom (within 100 points)
                    isAtBottom = maxY < UIScreen.main.bounds.height + 100
                }
                .onTapGesture {
                    // Dismiss keyboard when tapping on messages area
                    isInputFocused = false
                }
                .onChange(of: messages.count) { _, _ in
                    if isAtBottom {
                        scrollToBottom(proxy: proxy)
                    }
                }
                .onChange(of: shouldScrollToBottom) { _, shouldScroll in
                    if shouldScroll {
                        scrollToBottom(proxy: proxy)
                        shouldScrollToBottom = false
                    }
                }
                .overlay(alignment: .bottom) {
                    // Scroll to bottom button
                    if !isAtBottom && !messages.isEmpty {
                        Button {
                            withAnimation(.spring(duration: 0.3)) {
                                proxy.scrollTo(bottomAnchorId, anchor: .bottom)
                            }
                            isAtBottom = true
                        } label: {
                            Image(systemName: "arrow.down")
                                .font(.system(size: 14, weight: .semibold))
                                .foregroundStyle(.white)
                                .frame(width: 36, height: 36)
                                .background(.ultraThinMaterial)
                                .clipShape(Circle())
                                .overlay(
                                    Circle()
                                        .stroke(Theme.border, lineWidth: 1)
                                )
                                .shadow(color: .black.opacity(0.2), radius: 8, y: 4)
                        }
                        .padding(.bottom, 16)
                        .transition(.scale.combined(with: .opacity))
                    }
                }
            }
        }
    }
    
    private var hasActiveStreamingItem: Bool {
        messages.contains { msg in
            (msg.isThinking && !msg.thinkingDone) || msg.isPhase
        }
    }

    /// Check if the currently viewed mission is running (not just any mission)
    private var viewingMissionIsRunning: Bool {
        guard let viewingId = viewingMissionId else {
            // No specific mission being viewed - fall back to global state
            return runState != .idle
        }
        // Check if this specific mission is in the running missions list
        guard let missionInfo = runningMissions.first(where: { $0.missionId == viewingId }) else {
            return false
        }
        return missionInfo.state == "running" || missionInfo.state == "waiting_for_tool"
    }
    
    private var agentWorkingIndicator: some View {
        HStack(spacing: 12) {
            ProgressView()
                .progressViewStyle(.circular)
                .tint(Theme.accent)

            VStack(alignment: .leading, spacing: 2) {
                Text("Agent is working...")
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(Theme.textPrimary)

                Text("Updates will appear here as they arrive")
                    .font(.caption)
                    .foregroundStyle(Theme.textTertiary)
            }

            Spacer()
        }
        .padding(16)
        .background(.ultraThinMaterial)
        .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
        .transition(.opacity.combined(with: .scale(scale: 0.95)))
    }
    
    private func scrollToBottom(proxy: ScrollViewProxy) {
        withAnimation {
            proxy.scrollTo(bottomAnchorId, anchor: .bottom)
        }
    }
    
    private func copyMessage(_ message: ChatMessage) {
        UIPasteboard.general.string = message.content
        copiedMessageId = message.id
        HapticService.lightTap()
        
        // Reset after delay
        DispatchQueue.main.asyncAfter(deadline: .now() + 2) {
            if copiedMessageId == message.id {
                copiedMessageId = nil
            }
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
            
            // Quick action templates
            VStack(spacing: 12) {
                Text("Quick actions:")
                    .font(.caption)
                    .foregroundStyle(Theme.textMuted)

                LazyVGrid(columns: [GridItem(.flexible()), GridItem(.flexible())], spacing: 8) {
                    quickActionButton(
                        icon: "doc.text.fill",
                        title: "Analyze files",
                        prompt: "Read the files in /root/context and summarize what they contain",
                        color: Theme.accent
                    )
                    quickActionButton(
                        icon: "globe",
                        title: "Search web",
                        prompt: "Search the web for the latest news about ",
                        color: Theme.success
                    )
                    quickActionButton(
                        icon: "chevron.left.forwardslash.chevron.right",
                        title: "Write code",
                        prompt: "Write a Python script that ",
                        color: Theme.warning
                    )
                    quickActionButton(
                        icon: "terminal.fill",
                        title: "Run command",
                        prompt: "Run the command: ",
                        color: Theme.info
                    )
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

    private func quickActionButton(icon: String, title: String, prompt: String, color: Color) -> some View {
        Button {
            inputText = prompt
            isInputFocused = true
        } label: {
            HStack(spacing: 8) {
                Image(systemName: icon)
                    .font(.system(size: 14))
                    .foregroundStyle(color)
                    .frame(width: 20)

                Text(title)
                    .font(.caption.weight(.medium))
                    .foregroundStyle(Theme.textSecondary)

                Spacer()
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
            .background(Theme.backgroundSecondary)
            .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .stroke(Theme.border, lineWidth: 1)
            )
        }
    }
    
    // MARK: - Input

    private var inputView: some View {
        VStack(spacing: 0) {
            // ChatGPT-style input: clean outline, no fill, integrated send button
            HStack(alignment: .bottom, spacing: 0) {
                // Text input - minimal style with just a border
                TextField("Message the agent...", text: $inputText, axis: .vertical)
                    .textFieldStyle(.plain)
                    .font(.body)
                    .foregroundStyle(Theme.textPrimary)
                    .lineLimit(1...5)
                    .padding(.leading, 16)
                    .padding(.trailing, 8)
                    .padding(.vertical, 12)
                    .focused($isInputFocused)
                    .submitLabel(.send)
                    .onSubmit {
                        sendMessage()
                    }

                // Send/Stop button inside the input area
                Button {
                    if runState != .idle {
                        Task { await cancelRun() }
                    } else {
                        sendMessage()
                    }
                } label: {
                    Image(systemName: runState != .idle ? "stop.fill" : "arrow.up")
                        .font(.system(size: 14, weight: .semibold))
                        .foregroundStyle(
                            runState != .idle ? .white :
                            (inputText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? Theme.textMuted : .white)
                        )
                        .frame(width: 32, height: 32)
                        .background(
                            runState != .idle ? Theme.error :
                            (inputText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? Color.clear : Theme.accent)
                        )
                        .clipShape(Circle())
                        .overlay(
                            Circle()
                                .stroke(
                                    inputText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && runState == .idle
                                    ? Theme.border : Color.clear,
                                    lineWidth: 1
                                )
                        )
                }
                .disabled(runState == .idle && inputText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                .animation(.easeInOut(duration: 0.15), value: runState)
                .animation(.easeInOut(duration: 0.15), value: inputText.isEmpty)
                .padding(.trailing, 6)
                .padding(.bottom, 6)
            }
            .background(Color.clear)
            .clipShape(RoundedRectangle(cornerRadius: 24, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 24, style: .continuous)
                    .stroke(Theme.border, lineWidth: 1)
            )
            .padding(.horizontal, 16)
            .padding(.top, 12)
            .padding(.bottom, 16)
            .background(Theme.backgroundPrimary)
        }
    }
    
    // MARK: - Actions
    
    private func applyViewingMission(_ mission: Mission, scrollToBottom: Bool = true) {
        viewingMission = mission
        viewingMissionId = mission.id
        messages = mission.history.enumerated().map { index, entry in
            ChatMessage(
                id: "\(mission.id)-\(index)",
                type: entry.isUser ? .user : .assistant(success: true, costCents: 0, model: nil),
                content: entry.content
            )
        }

        if scrollToBottom {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) {
                shouldScrollToBottom = true
            }
        }
    }

    private func loadCurrentMission(updateViewing: Bool) async {
        isLoading = true
        defer { isLoading = false }

        do {
            if let mission = try await api.getCurrentMission() {
                currentMission = mission
                if updateViewing || viewingMissionId == nil || viewingMissionId == mission.id {
                    applyViewingMission(mission)
                }
            }
        } catch {
            print("Failed to load mission: \(error)")
        }
    }
    
    private func loadMission(id: String) async {
        // Set target immediately for race condition tracking
        fetchingMissionId = id
        let previousViewingMission = viewingMission
        let previousViewingId = viewingMissionId
        viewingMissionId = id
        
        isLoading = true

        do {
            let mission = try await api.getMission(id: id)
            
            // Race condition guard: only update if this is still the mission we want
            guard fetchingMissionId == id else {
                return // Another mission was requested, discard this response
            }
            
            if currentMission?.id == mission.id {
                currentMission = mission
            }
            applyViewingMission(mission)
            isLoading = false
            HapticService.success()
        } catch {
            // Race condition guard
            guard fetchingMissionId == id else { return }
            
            isLoading = false
            print("Failed to load mission: \(error)")
            
            // Revert viewing state to avoid filtering out events
            if let fallback = previousViewingMission ?? currentMission {
                applyViewingMission(fallback, scrollToBottom: false)
            } else {
                viewingMissionId = previousViewingId
            }
        }
    }
    
    private func createNewMission() async {
        do {
            let mission = try await api.createMission()
            currentMission = mission
            applyViewingMission(mission, scrollToBottom: false)

            // Reset status for the new mission - it hasn't started yet
            runState = .idle
            queueLength = 0
            progress = nil

            // Refresh running missions to show the new mission
            await refreshRunningMissions()

            // Show the bar when creating new missions
            if !showRunningMissions && !runningMissions.isEmpty {
                withAnimation(.easeInOut(duration: 0.2)) {
                    showRunningMissions = true
                }
            }

            HapticService.success()
        } catch {
            print("Failed to create mission: \(error)")
            HapticService.error()
        }
    }
    
    private func setMissionStatus(_ status: MissionStatus) async {
        guard let mission = viewingMission else { return }
        
        do {
            try await api.setMissionStatus(id: mission.id, status: status)
            viewingMission?.status = status
            if currentMission?.id == mission.id {
                currentMission?.status = status
            }
            HapticService.success()
        } catch {
            print("Failed to set status: \(error)")
            HapticService.error()
        }
    }
    
    private func resumeMission() async {
        guard let mission = viewingMission, mission.canResume else { return }
        
        do {
            let resumed = try await api.resumeMission(id: mission.id)
            currentMission = resumed
            applyViewingMission(resumed)
            
            // Refresh running missions
            await refreshRunningMissions()
            
            HapticService.success()
        } catch {
            print("Failed to resume mission: \(error)")
            HapticService.error()
        }
    }
    
    private func sendMessage() {
        let content = inputText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !content.isEmpty else { return }

        inputText = ""
        HapticService.lightTap()

        // Generate temp ID and add message optimistically BEFORE the API call
        // This ensures messages appear in send order, not response order
        let tempId = "temp-\(UUID().uuidString)"
        let tempMessage = ChatMessage(id: tempId, type: .user, content: content)
        messages.append(tempMessage)
        shouldScrollToBottom = true

        Task { @MainActor in
            do {
                let (messageId, _) = try await api.sendMessage(content: content)

                // Replace temp ID with server-assigned ID, preserving timestamp
                // This allows SSE handler to correctly deduplicate
                if let index = messages.firstIndex(where: { $0.id == tempId }) {
                    let originalTimestamp = messages[index].timestamp
                    messages[index] = ChatMessage(id: messageId, type: .user, content: content, timestamp: originalTimestamp)
                }

                // If we don't have a current mission, the backend may have just created one
                // Refresh to get the new mission context
                if currentMission == nil {
                    await loadCurrentMission(updateViewing: true)
                }
            } catch {
                print("Failed to send message: \(error)")
                // Remove the optimistic message on error
                messages.removeAll { $0.id == tempId }
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
        streamTask = Task {
            // Exponential backoff: 1s, 2s, 4s, 8s, 16s, max 30s
            let maxBackoff: UInt64 = 30
            var currentBackoff: UInt64 = 1

            while !Task.isCancelled {
                // Reset connection state and attempt counter on new connection
                await MainActor.run {
                    if reconnectAttempt > 0 {
                        connectionState = .reconnecting(attempt: reconnectAttempt)
                    }
                }

                // Start streaming - this will block until the stream ends
                // Use OSAllocatedUnfairLock for thread-safe boolean access across actor boundaries
                // Track successful (non-error) events separately from all events
                let receivedSuccessfulEvent = OSAllocatedUnfairLock(initialState: false)

                let streamCompleted = await withCheckedContinuation { continuation in
                    let innerTask = api.streamControl { eventType, data in
                        // Only count non-error events as successful for backoff reset
                        if eventType != "error" {
                            receivedSuccessfulEvent.withLock { $0 = true }
                        }
                        Task { @MainActor in
                            // Successfully received an event - we're connected
                            let wasReconnecting = !self.connectionState.isConnected && self.reconnectAttempt > 0
                            if !self.connectionState.isConnected {
                                self.connectionState = .connected
                                self.reconnectAttempt = 0

                                // If we just reconnected, refresh the viewed mission's history to catch missed events
                                if wasReconnecting, let viewingId = self.viewingMissionId {
                                    Task {
                                        do {
                                            let mission = try await self.api.getMission(id: viewingId)
                                            await MainActor.run {
                                                self.applyViewingMission(mission)
                                            }
                                        } catch {
                                            // Ignore errors - we'll get updates via stream
                                        }
                                    }
                                }
                            }
                            self.handleStreamEvent(type: eventType, data: data)
                        }
                    }

                    // Wait for the stream task to complete
                    Task {
                        await innerTask.value
                        continuation.resume(returning: true)
                    }
                }

                // Reset backoff only after receiving successful (non-error) events
                // This prevents error events from resetting backoff when server is unavailable
                if receivedSuccessfulEvent.withLock({ $0 }) {
                    currentBackoff = 1
                }

                // Stream ended - check if we should reconnect
                guard !Task.isCancelled else { break }

                // Update state to reconnecting
                await MainActor.run {
                    reconnectAttempt += 1
                    connectionState = .reconnecting(attempt: reconnectAttempt)
                }

                // Wait before reconnecting (exponential backoff)
                try? await Task.sleep(for: .seconds(currentBackoff))
                currentBackoff = min(currentBackoff * 2, maxBackoff)

                // Check cancellation again after sleep
                guard !Task.isCancelled else { break }
            }
        }
    }
    
    // MARK: - Parallel Missions
    
    private func refreshRunningMissions() async {
        do {
            runningMissions = try await api.getRunningMissions()
        } catch {
            print("Failed to refresh running missions: \(error)")
        }
    }
    
    private func startPollingRunningMissions() {
        pollingTask = Task {
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(3))
                guard !Task.isCancelled else { break }
                await refreshRunningMissions()
            }
        }
    }
    
    private func switchToMission(id: String) async {
        guard id != viewingMissionId else { return }

        // Set the target mission ID immediately for race condition tracking
        let previousViewingMission = viewingMission
        let previousViewingId = viewingMissionId
        let previousRunState = runState
        let previousQueueLength = queueLength
        let previousProgress = progress
        viewingMissionId = id
        fetchingMissionId = id

        isLoading = true

        // Determine the run state for this mission from runningMissions
        if let runningInfo = runningMissions.first(where: { $0.missionId == id }) {
            // This mission is in the running list - map state string to enum properly
            switch runningInfo.state {
            case "running":
                runState = .running
            case "waiting_for_tool":
                runState = .waitingForTool
            default:
                runState = .idle
            }
            queueLength = runningInfo.queueLen
        } else {
            // Not in the running list - assume idle
            runState = .idle
            queueLength = 0
        }
        progress = nil

        do {
            // Load the mission from API
            let mission = try await api.getMission(id: id)

            // Race condition guard: only update if this is still the mission we want
            guard fetchingMissionId == id else {
                return // Another mission was requested, discard this response
            }

            // Update current mission if this is the main mission, and update the viewed mission
            if currentMission?.id == mission.id {
                currentMission = mission
            }
            applyViewingMission(mission)

            isLoading = false
            HapticService.selectionChanged()
        } catch {
            // Race condition guard: only show error if this is still the mission we want
            guard fetchingMissionId == id else { return }

            isLoading = false
            print("Failed to switch mission: \(error)")
            HapticService.error()

            // Revert viewing state and status indicators to avoid filtering out events
            runState = previousRunState
            queueLength = previousQueueLength
            progress = previousProgress
            if let fallback = previousViewingMission ?? currentMission {
                applyViewingMission(fallback, scrollToBottom: false)
            } else {
                viewingMissionId = previousViewingId
            }
        }
    }
    
    private func cancelMission(id: String) async {
        do {
            try await api.cancelMission(id: id)
            
            // Refresh running missions
            await refreshRunningMissions()
            
            // If we were viewing this mission, switch to current
            if viewingMissionId == id {
                if let currentId = currentMission?.id {
                    await switchToMission(id: currentId)
                }
            }
            
            HapticService.success()
        } catch {
            print("Failed to cancel mission: \(error)")
            HapticService.error()
        }
    }
    
    private func handleStreamEvent(type: String, data: [String: Any]) {
        // Filter events by mission_id - only show events for the mission we're viewing
        // This prevents cross-mission contamination when parallel missions are running
        let eventMissionId = data["mission_id"] as? String
        let viewingId = viewingMissionId
        let currentId = currentMission?.id

        // Only allow status events from any mission (for global state)
        // All other events must match the mission we're viewing
        if type != "status" {
            if let eventId = eventMissionId {
                // Event has a mission_id
                if let vId = viewingId {
                    // We're viewing a specific mission - must match
                    if eventId != vId {
                        return // Skip events from other missions
                    }
                } else if let cId = currentId {
                    // Not viewing any mission but have a current one - must match current
                    if eventId != cId {
                        return // Skip events from other missions
                    }
                }
                // If both viewingId and currentId are nil, accept the event
                // This handles the case where a new mission was just created
            } else if viewingId != nil && viewingId != currentId {
                // Event has NO mission_id (from main session)
                // Skip if we're viewing a different (parallel) mission
                return
            }
        }
        
        switch type {
        case "status":
            // Status events: only apply if viewing the mission this status is for
            // - mission_id == nil: this is the main session's status (applies to currentMission)
            // - mission_id == some_id: this is a parallel mission's status
            let statusMissionId = eventMissionId
            let shouldApply: Bool

            if let statusId = statusMissionId {
                // Status for a specific mission - only apply if we're viewing that mission
                shouldApply = statusId == viewingId
            } else {
                // Status for main session - only apply if viewing the current (main) mission
                // or if we don't have a current mission yet
                shouldApply = viewingId == nil || viewingId == currentId
            }

            if shouldApply {
                if let state = data["state"] as? String {
                    let newState = ControlRunState(rawValue: state) ?? .idle
                    runState = newState

                    // Clear progress when idle
                    if newState == .idle {
                        progress = nil
                    }
                }
                if let queue = data["queue_len"] as? Int {
                    queueLength = queue
                }
            }
            
        case "user_message":
            if let content = data["content"] as? String,
               let id = data["id"] as? String {
                // Skip if we already have this message with this ID
                guard !messages.contains(where: { $0.id == id }) else { break }

                // Check if there's a pending temp message with matching content (SSE arrived before API response)
                // We verify content to avoid mismatching with messages from other sessions/devices
                if let tempIndex = messages.firstIndex(where: {
                    $0.isUser && $0.id.hasPrefix("temp-") && $0.content == content
                }) {
                    // Replace temp ID with server ID, preserving original timestamp
                    let originalTimestamp = messages[tempIndex].timestamp
                    messages[tempIndex] = ChatMessage(id: id, type: .user, content: content, timestamp: originalTimestamp)
                } else {
                    // No matching temp message found, add new (message came from another client/session)
                    let message = ChatMessage(id: id, type: .user, content: content)
                    messages.append(message)
                }
            }
            
        case "assistant_message":
            if let content = data["content"] as? String,
               let id = data["id"] as? String {
                let success = data["success"] as? Bool ?? true
                let costCents = data["cost_cents"] as? Int ?? 0
                let model = data["model"] as? String
                
                // Remove any incomplete thinking messages and phase messages
                messages.removeAll { ($0.isThinking && !$0.thinkingDone) || $0.isPhase }
                
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
                
                // Remove phase items when thinking starts
                messages.removeAll { $0.isPhase }
                
                // Find existing thinking message or create new
                if let index = messages.lastIndex(where: { $0.isThinking && !$0.thinkingDone }) {
                    let existingStartTime = messages[index].thinkingStartTime ?? Date()
                    messages[index].content += "\n\n---\n\n" + content
                    if done {
                        messages[index] = ChatMessage(
                            id: messages[index].id,
                            type: .thinking(done: true, startTime: existingStartTime),
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
            
        case "agent_phase":
            let phase = data["phase"] as? String ?? ""
            let detail = data["detail"] as? String
            let agent = data["agent"] as? String
            
            // Remove existing phase messages
            messages.removeAll { $0.isPhase }
            
            // Add new phase message
            let message = ChatMessage(
                id: "phase-\(Date().timeIntervalSince1970)",
                type: .phase(phase: phase, detail: detail, agent: agent),
                content: ""
            )
            messages.append(message)
            
        case "progress":
            let total = data["total_subtasks"] as? Int ?? 0
            let completed = data["completed_subtasks"] as? Int ?? 0
            let current = data["current_subtask"] as? String
            let depth = data["depth"] as? Int ?? data["current_depth"] as? Int ?? 0
            
            if total > 0 {
                progress = ExecutionProgress(
                    total: total,
                    completed: completed,
                    current: current,
                    depth: depth
                )
            }
            
        case "error":
            if let errorMessage = data["message"] as? String {
                // Filter out SSE-specific reconnection errors - these are handled by the reconnection logic
                // Use specific patterns to avoid filtering legitimate agent errors
                let lower = errorMessage.lowercased()
                let isSseReconnectError = lower.contains("stream connection failed") ||
                                          lower.contains("sse connection") ||
                                          lower.contains("event stream") ||
                                          lower == "timed out" ||
                                          lower == "connection reset" ||
                                          lower == "connection closed"

                if !isSseReconnectError {
                    let message = ChatMessage(
                        id: "error-\(Date().timeIntervalSince1970)",
                        type: .error,
                        content: errorMessage
                    )
                    messages.append(message)
                }
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

// MARK: - Scroll Offset Preference Key

private struct ScrollOffsetPreferenceKey: PreferenceKey {
    nonisolated(unsafe) static var defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        value = nextValue()
    }
}

// MARK: - Message Bubble

private struct MessageBubble: View {
    let message: ChatMessage
    var isCopied: Bool = false
    var onCopy: (() -> Void)?
    
    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            if message.isUser {
                Spacer(minLength: 60)
                userBubble
            } else if message.isThinking {
                ThinkingBubble(message: message)
                Spacer(minLength: 60)
            } else if message.isPhase {
                PhaseBubble(message: message)
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
        HStack(alignment: .top, spacing: 8) {
            // Copy button
            if !message.content.isEmpty {
                CopyButton(isCopied: isCopied, onCopy: onCopy)
            }

            VStack(alignment: .trailing, spacing: 4) {
                Text(message.content)
                    .font(.body)
                    .foregroundStyle(.white)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 12)
                    .background(Theme.accent)
                    .clipShape(
                        .rect(
                            topLeadingRadius: 20,
                            bottomLeadingRadius: 20,
                            bottomTrailingRadius: 6,
                            topTrailingRadius: 20
                        )
                    )

                // Timestamp
                Text(message.timestamp, style: .time)
                    .font(.caption2)
                    .foregroundStyle(Theme.textMuted)
            }
        }
    }
    
    private var assistantBubble: some View {
        HStack(alignment: .top, spacing: 8) {
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

                        Text("•")
                            .foregroundStyle(Theme.textMuted)
                        Text(message.timestamp, style: .time)
                            .font(.caption2)
                            .foregroundStyle(Theme.textMuted)
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
            
            // Copy button
            if !message.content.isEmpty {
                CopyButton(isCopied: isCopied, onCopy: onCopy)
            }
        }
    }
}

// MARK: - Copy Button

private struct CopyButton: View {
    let isCopied: Bool
    let onCopy: (() -> Void)?
    
    var body: some View {
        Button {
            onCopy?()
        } label: {
            Image(systemName: isCopied ? "checkmark" : "doc.on.doc")
                .font(.system(size: 12))
                .foregroundStyle(isCopied ? Theme.success : Theme.textMuted)
                .frame(width: 28, height: 28)
                .background(Theme.backgroundSecondary)
                .clipShape(Circle())
        }
        .opacity(0.7)
    }
}

// MARK: - Phase Bubble

private struct PhaseBubble: View {
    let message: ChatMessage
    
    var body: some View {
        if case .phase(let phase, let detail, let agent) = message.type {
            let agentPhase = AgentPhase(rawValue: phase)
            
            HStack(spacing: 12) {
                // Icon with pulse animation
                Image(systemName: agentPhase?.icon ?? "gear")
                    .font(.system(size: 16, weight: .medium))
                    .foregroundStyle(Theme.accent)
                    .symbolEffect(.pulse, options: .repeating)
                    .frame(width: 32, height: 32)
                    .background(Theme.accent.opacity(0.1))
                    .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
                
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text(agentPhase?.label ?? phase.replacingOccurrences(of: "_", with: " ").capitalized)
                            .font(.subheadline.weight(.medium))
                            .foregroundStyle(Theme.accent)
                        
                        if let agent = agent {
                            Text(agent)
                                .font(.caption2.monospaced())
                                .foregroundStyle(Theme.textMuted)
                                .padding(.horizontal, 6)
                                .padding(.vertical, 2)
                                .background(Theme.backgroundTertiary)
                                .clipShape(RoundedRectangle(cornerRadius: 4, style: .continuous))
                        }
                    }
                    
                    if let detail = detail {
                        Text(detail)
                            .font(.caption)
                            .foregroundStyle(Theme.textTertiary)
                    }
                }
                
                Spacer()
                
                // Spinner
                ProgressView()
                    .progressViewStyle(.circular)
                    .scaleEffect(0.7)
                    .tint(Theme.accent.opacity(0.5))
            }
            .padding(12)
            .background(.ultraThinMaterial)
            .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .stroke(Theme.accent.opacity(0.15), lineWidth: 1)
            )
            .transition(.opacity.combined(with: .scale(scale: 0.95)))
        }
    }
}

// MARK: - Thinking Bubble

private struct ThinkingBubble: View {
    let message: ChatMessage
    @State private var isExpanded: Bool = true
    @State private var elapsedSeconds: Int = 0
    @State private var hasAutoCollapsed = false
    
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            // Compact header button
            Button {
                withAnimation(.spring(duration: 0.25)) {
                    isExpanded.toggle()
                }
                HapticService.selectionChanged()
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: "brain")
                        .font(.caption)
                        .foregroundStyle(Theme.accent)
                        .symbolEffect(.pulse, options: message.thinkingDone ? .nonRepeating : .repeating)
                    
                    Text(message.thinkingDone ? "Thought for \(formattedDuration)" : "Thinking for \(formattedDuration)")
                        .font(.caption)
                        .foregroundStyle(Theme.textSecondary)
                    
                    Image(systemName: "chevron.right")
                        .font(.system(size: 10, weight: .medium))
                        .foregroundStyle(Theme.textMuted)
                        .rotationEffect(.degrees(isExpanded ? 90 : 0))
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
                .background(Theme.accent.opacity(0.1))
                .clipShape(Capsule())
            }
            
            // Expandable content
            if isExpanded && !message.content.isEmpty {
                ScrollView {
                    Text(message.content)
                        .font(.caption)
                        .foregroundStyle(Theme.textTertiary)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(maxHeight: 300) // Allow scrolling for long thinking content
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                .background(Color.white.opacity(0.02))
                .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                .overlay(
                    RoundedRectangle(cornerRadius: 12, style: .continuous)
                        .stroke(Theme.border, lineWidth: 0.5)
                )
                .transition(.opacity.combined(with: .scale(scale: 0.95, anchor: .top)))
            } else if isExpanded && message.content.isEmpty {
                Text("Processing...")
                    .font(.caption)
                    .italic()
                    .foregroundStyle(Theme.textMuted)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
            }
        }
        .onAppear {
            startTimer()
        }
        .onChange(of: message.thinkingDone) { _, done in
            if done && !hasAutoCollapsed {
                // Don't auto-collapse for extended thinking (> 30 seconds)
                // User may want to review what the agent was thinking about
                let duration = message.thinkingStartTime.map { Int(Date().timeIntervalSince($0)) } ?? 0
                if duration > 30 {
                    hasAutoCollapsed = true // Mark as handled but don't collapse
                    return
                }
                // Auto-collapse shorter thinking after a delay
                DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) {
                    withAnimation(.spring(duration: 0.25)) {
                        isExpanded = false
                        hasAutoCollapsed = true
                    }
                }
            }
        }
    }
    
    private var formattedDuration: String {
        if elapsedSeconds < 60 {
            return "\(elapsedSeconds)s"
        } else {
            let mins = elapsedSeconds / 60
            let secs = elapsedSeconds % 60
            return secs > 0 ? "\(mins)m \(secs)s" : "\(mins)m"
        }
    }
    
    private func startTimer() {
        guard !message.thinkingDone else {
            // Calculate elapsed from start time
            if let startTime = message.thinkingStartTime {
                elapsedSeconds = Int(Date().timeIntervalSince(startTime))
            }
            return
        }
        
        // Update every second while thinking
        Timer.scheduledTimer(withTimeInterval: 1.0, repeats: true) { timer in
            if message.thinkingDone {
                timer.invalidate()
            } else if let startTime = message.thinkingStartTime {
                elapsedSeconds = Int(Date().timeIntervalSince(startTime))
            } else {
                elapsedSeconds += 1
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

// MARK: - Markdown Text

private struct MarkdownText: View {
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

#Preview {
    NavigationStack {
        ControlView()
    }
}
