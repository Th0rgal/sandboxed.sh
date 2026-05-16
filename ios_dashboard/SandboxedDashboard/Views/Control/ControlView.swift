//
//  ControlView.swift
//  SandboxedDashboard
//
//  Chat interface for the AI agent with real-time streaming
//

import SwiftUI
import os

/// Snapshot of the active codex goal-mode loop, surfaced as a pill above
/// the composer. Status mirrors codex's `thread/goal/updated` payload:
/// `active`, `paused`, `budgetLimited`, `complete`, or `cleared`.
struct GoalPillInfo: Equatable {
    var iteration: Int
    var status: String
    var objective: String
}

struct ControlView: View {
    private static let draftTextKey = "control_draft_text"
    private static let lastMissionIdKey = "control_last_mission_id"

    @State private var messages: [ChatMessage] = []
    @State private var inputText = UserDefaults.standard.string(forKey: ControlView.draftTextKey) ?? ""
    @State private var runState: ControlRunState = .idle
    @State private var queueLength = 0
    @State private var queuedItems: [QueuedMessage] = []
    @State private var showQueueSheet = false
    @State private var currentMission: Mission?
    @State private var viewingMission: Mission?
    @State private var isLoading = true
    @State private var streamTask: Task<Void, Never>?
    @State private var showMissionMenu = false
    /// Monotonic counter — each increment is a request to scroll to the bottom.
    /// Counter rather than a Bool because the conversation `ScrollView` is
    /// conditionally mounted (only when `messages` is non-empty). A Bool flag
    /// can get stuck `true` if it's set while the ScrollView is unmounted —
    /// the resetter inside `.onChange` never runs, and subsequent re-arms to
    /// `true` are no-ops because the value didn't change. A counter always
    /// advances, so `.onChange` fires reliably on every request.
    @State private var scrollToBottomTick = 0
    @State private var progress: ExecutionProgress?
    @State private var isAtBottom = true

    /// Goal-mode pill state. Tracks the latest `iteration`, `status`, and
    /// `objective` for codex `/goal` continuation missions. `nil` when no
    /// goal is active or when the mission has reached a terminal goal
    /// status (`complete`, `cleared`, `budgetLimited`).
    @State private var goalInfo: GoalPillInfo?

    /// Slash-command catalog fetched from /api/library/builtin-commands.
    /// Lazy-loaded on first `/` keypress, refreshed when the backend
    /// changes. The popover above the composer filters this list by the
    /// substring after the leading `/` in `inputText`.
    @State private var slashCommandCatalog: BuiltinCommandsResponse?
    @State private var slashCommandLoading = false
    @State private var copiedMessageId: String?
    @State private var shouldScrollImmediately = false
    @State private var isLoadingHistory = false  // Track when loading historical messages to prevent animated scroll
    @State private var pendingFocusedMessageId: String?

    // Pagination state
    @State private var hasMoreHistory = false
    @State private var isLoadingEarlier = false
    @State private var loadedEventCount = 0  // How many events we've loaded so far

    /// Per-mission high-water mark for `sequence`. When non-nil, reload paths
    /// pass it as `since_seq` to `/events` so the server returns only the tail
    /// that arrived while we were disconnected, not the whole history. Seeded
    /// from the `X-Max-Sequence` response header — backends that don't set the
    /// header leave this `nil` and callers fall back to full reload.
    @State private var missionMaxSeq: [String: Int64] = [:]

    // Cached grouped items (recomputed only when messages change)
    @State private var groupedItems: [GroupedChatItem] = []

    // Draft save debounce
    @State private var draftSaveTask: Task<Void, Never>?

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

    // Thoughts panel state
    @State private var showThoughts = false

    // Tool grouping state - track which groups are expanded
    @State private var expandedToolGroups: Set<String> = []

    // Mission switcher state
    @State private var showMissionSwitcher = false
    @State private var recentMissions: [Mission] = []

    // Desktop stream state
    @State private var showDesktopStream = false
    @State private var desktopDisplayId = ":101"
    private let availableDisplays = [":99", ":100", ":101", ":102"]

    // Worker (child mission) state
    @State private var childMissions: [Mission] = []
    @State private var showWorkerSheet = false

    // Workspace selection state (global)
    private var workspaceState = WorkspaceState.shared
    @State private var showNewMissionSheet = false
    @State private var showSettings = false
    @State private var showAutomations = false

    @FocusState private var isInputFocused: Bool
    @Environment(\.scenePhase) private var scenePhase

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
                // Persistent connection banner. The 9pt wifi-slash glyph in
                // the principal toolbar is too easy to miss when the SSE
                // stream drops; a sticky strip across the top keeps the
                // disconnected state in peripheral vision until reconnect.
                if !connectionState.isConnected {
                    connectionBanner
                        .transition(.move(edge: .top).combined(with: .opacity))
                }

                // Messages
                ZStack(alignment: .bottom) {
                    messagesView

                    // Worker pill overlay for boss missions
                    if !childMissions.isEmpty {
                        WorkerPillView(
                            workers: childMissions,
                            runningWorkers: runningMissions,
                            onTap: {
                                HapticService.lightTap()
                                showWorkerSheet = true
                            }
                        )
                        .padding(.bottom, 12)
                        .transition(.move(edge: .bottom).combined(with: .opacity))
                        .animation(.spring(response: 0.3), value: childMissions.count)
                    }
                }

                // Input area
                inputView
            }
            .animation(.easeInOut(duration: 0.2), value: connectionState.isConnected)
        }
        .navigationTitle(viewingMission?.displayTitle ?? "Control")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) {
                VStack(spacing: 2) {
                    // Use the raw title with `.lineLimit(1)` rather than the
                    // pre-truncated `displayTitle` (which silently slices at
                    // 60 chars with no escape hatch). SwiftUI handles tail
                    // truncation against the actual nav-bar width, and the
                    // context menu lets the user pull up the full string.
                    //
                    // Fall back to "Untitled Mission" — not "Control" — when a
                    // mission is being viewed but its title is empty. Showing
                    // "Control" would falsely imply no mission is active and
                    // diverges from `.navigationTitle` (which uses
                    // `displayTitle`, returning "Untitled Mission").
                    let fullTitle: String = {
                        if let mission = viewingMission {
                            let trimmed = mission.title?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
                            return trimmed.isEmpty ? "Untitled Mission" : trimmed
                        }
                        return "Control"
                    }()
                    Text(fullTitle)
                        .font(.headline)
                        .foregroundStyle(Theme.textPrimary)
                        .lineLimit(1)
                        .truncationMode(.tail)
                        .contextMenu {
                            if viewingMission != nil {
                                Section(fullTitle) {}
                                Button {
                                    UIPasteboard.general.string = fullTitle
                                    HapticService.lightTap()
                                } label: {
                                    Label("Copy title", systemImage: "doc.on.doc")
                                }
                            }
                        }

                    HStack(spacing: 4) {
                        // Connection state moved to a persistent banner above the
                        // conversation; this row always shows backend + run state
                        // so a disconnect doesn't blank out the run-state context.
                        if let mission = viewingMission {
                            let backendColor = missionBackendColor(mission)
                            if let agent = mission.agent, !agent.isEmpty {
                                Image(systemName: missionBackendIcon(mission))
                                    .font(.system(size: 9))
                                    .foregroundStyle(backendColor)
                                Text(agent)
                                    .font(.caption2)
                                    .foregroundStyle(backendColor)
                                Text("•")
                                    .foregroundStyle(Theme.textMuted)
                            }
                        }

                        StatusDot(status: runState.statusType, size: 5)
                        Text(runState.label)
                            .font(.caption2)
                            .foregroundStyle(Theme.textSecondary)

                        if queueLength > 0 {
                            Button {
                                Task { await loadQueueItems() }
                                showQueueSheet = true
                                HapticService.lightTap()
                            } label: {
                                Text("• \(queueLength) queued")
                                    .font(.caption2)
                                    .foregroundStyle(Theme.warning)
                            }
                        }

                        // Progress indicator
                        if let progress, progress.total > 0 {
                            Text("•")
                                .foregroundStyle(Theme.textMuted)
                            Text(progress.displayText)
                                .font(.caption2.weight(.medium))
                                .foregroundStyle(Theme.success)
                        }
                    }
                }
            }
            
            ToolbarItem(placement: .topBarLeading) {
                HStack(spacing: 12) {
                    // Thoughts panel button
                    Button {
                        showThoughts = true
                        HapticService.lightTap()
                    } label: {
                        Image(systemName: "brain")
                            .font(.system(size: 14))
                            .foregroundStyle(
                                messages.contains(where: { $0.isThinking }) ? Theme.accent : Theme.textSecondary
                            )
                    }

                    // Workers button (only visible for boss missions)
                    if !childMissions.isEmpty {
                        Button {
                            showWorkerSheet = true
                            HapticService.lightTap()
                        } label: {
                            HStack(spacing: 3) {
                                Image(systemName: "person.3")
                                    .font(.system(size: 12))
                                Text("\(childMissions.count)")
                                    .font(.caption2.weight(.medium))
                            }
                            .foregroundStyle(Theme.accent)
                        }
                    }
                }
            }

            ToolbarItem(placement: .topBarTrailing) {
                // Mission switcher button
                Button {
                    Task {
                        await loadRecentMissions()
                    }
                    showMissionSwitcher = true
                    HapticService.lightTap()
                } label: {
                    HStack(spacing: 4) {
                        Image(systemName: "square.stack.3d.up")
                            .font(.system(size: 14))
                        if runningMissions.count > 0 {
                            Text("\(runningMissions.count)")
                                .font(.caption2.weight(.medium))
                        }
                    }
                    .foregroundStyle(runningMissions.isEmpty ? Theme.textSecondary : Theme.accent)
                }
            }

            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    Button {
                        Task {
                            await workspaceState.loadWorkspaces()
                            if let options = await getValidatedDefaultAgentOptions() {
                                await createNewMission(options: options)
                            } else {
                                showNewMissionSheet = true
                            }
                        }
                    } label: {
                        Label("New Mission", systemImage: "plus")
                    }

                    // Desktop stream option with display selector
                    Menu {
                        ForEach(availableDisplays, id: \.self) { display in
                            Button {
                                desktopDisplayId = display
                                showDesktopStream = true
                            } label: {
                                HStack {
                                    Text(display)
                                    if display == desktopDisplayId {
                                        Image(systemName: "checkmark")
                                    }
                                }
                            }
                        }
                    } label: {
                        Label("View Desktop (\(desktopDisplayId))", systemImage: "display")
                    }

                    Button {
                        showAutomations = true
                    } label: {
                        Label("View Automations", systemImage: "bolt.badge.clock")
                    }

                    Divider()

                    Button {
                        showSettings = true
                    } label: {
                        Label("Settings", systemImage: "gearshape")
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
            // Cold-start sequencing. Previously every step here was awaited
            // serially — workspaces → loadMission → loadCurrentMission →
            // refreshRunningMissions — which on a slow cellular link stacked
            // up to a dozen sequential RTTs before the user could read or
            // type anything. We now:
            //   1. Kick off the SSE stream first so any live events for the
            //      restored mission start arriving immediately (the stream
            //      doesn't depend on which mission we end up viewing — it
            //      receives all events and the view filters by id).
            //   2. Fan out the three independent "context" fetches —
            //      workspaces, mission history, running missions — using
            //      `async let`. They share zero state at this stage; the
            //      previous serial chain was incidental, not required.
            //   3. Start the running-missions poller last; it's a 3s tick
            //      and starting it before the first refresh is fine.
            startStreaming()

            async let workspacesTask: Void = workspaceState.loadWorkspaces()
            async let runningTask: Void = refreshRunningMissions()
            async let missionTask: Void = loadInitialMission()

            _ = await (workspacesTask, runningTask, missionTask)

            // Auto-show bar if there are multiple running missions
            if runningMissions.count > 1 {
                showRunningMissions = true
            }

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
        .onChange(of: scenePhase) { oldPhase, newPhase in
            if newPhase != .active {
                // Save draft text when leaving foreground
                UserDefaults.standard.set(inputText, forKey: Self.draftTextKey)
            }
            // Reload mission history when app becomes active (similar to web's visibility change handler)
            // This ensures we catch any missed SSE events while the app was in background
            if oldPhase != .active && newPhase == .active {
                Task {
                    if let missionId = viewingMissionId {
                        await reloadMissionFromServer(id: missionId)
                    }
                    await refreshRunningMissions()
                }
            }
        }
        .onChange(of: viewingMissionId) { _, newId in
            UserDefaults.standard.set(newId, forKey: Self.lastMissionIdKey)
        }
        .onChange(of: inputText) { _, newText in
            // Debounced save: persist draft after 1 second of inactivity
            draftSaveTask?.cancel()
            draftSaveTask = Task {
                try? await Task.sleep(for: .seconds(1))
                guard !Task.isCancelled else { return }
                UserDefaults.standard.set(newText, forKey: Self.draftTextKey)
            }
        }
        .onDisappear {
            streamTask?.cancel()
            connectionState = .disconnected
            reconnectAttempt = 0
            pollingTask?.cancel()
            // Save draft immediately on disappear
            UserDefaults.standard.set(inputText, forKey: Self.draftTextKey)
            draftSaveTask?.cancel()
        }
        .sheet(isPresented: $showDesktopStream) {
            DesktopStreamView(displayId: desktopDisplayId)
                .presentationDetents([.medium, .large])
                .presentationDragIndicator(.visible)
                .presentationBackgroundInteraction(.enabled(upThrough: .medium))
        }
        .sheet(isPresented: $showThoughts) {
            ThoughtsSheet(messages: messages)
                .presentationDetents([.medium, .large])
                .presentationDragIndicator(.visible)
                .presentationBackgroundInteraction(.enabled(upThrough: .medium))
        }
        .sheet(isPresented: $showWorkerSheet) {
            WorkerSheetView(workers: childMissions, runningWorkers: runningMissions)
                .presentationDetents([.medium, .large])
                .presentationDragIndicator(.visible)
                .presentationBackgroundInteraction(.enabled(upThrough: .medium))
        }
        .onChange(of: showDesktopStream) { _, isShowing in
            // Auto-hide keyboard when opening the desktop stream
            if isShowing {
                isInputFocused = false
            }
        }
        .sheet(isPresented: $showNewMissionSheet) {
            NewMissionSheet(
                workspaces: workspaceState.workspaces,
                selectedWorkspaceId: Binding(
                    get: { workspaceState.selectedWorkspace?.id },
                    set: { if let id = $0 { workspaceState.selectWorkspace(id: id) } }
                ),
                onCreate: { options in
                    showNewMissionSheet = false
                    Task { await createNewMission(options: options) }
                },
                onCancel: {
                    showNewMissionSheet = false
                }
            )
            .presentationDetents([.fraction(0.9)])
            .presentationDragIndicator(.visible)
        }
        .sheet(isPresented: $showSettings) {
            SettingsView()
        }
        .sheet(isPresented: $showAutomations) {
            AutomationsView(missionId: viewingMission?.id ?? currentMission?.id)
        }
        .sheet(isPresented: $showMissionSwitcher) {
            MissionSwitcherSheet(
                runningMissions: runningMissions,
                recentMissions: recentMissions,
                currentMissionId: currentMission?.id,
                viewingMissionId: viewingMissionId,
                onSelectMission: { missionId in
                    showMissionSwitcher = false
                    Task { await switchToMission(id: missionId) }
                },
                onResumeMission: { missionId in
                    showMissionSwitcher = false
                    Task { await resumeMission(id: missionId) }
                },
                onFollowUpMission: { mission in
                    showMissionSwitcher = false
                    Task { await createFollowUpMission(from: mission) }
                },
                onOpenFailureMission: { missionId in
                    showMissionSwitcher = false
                    Task { await openFailingToolCall(for: missionId) }
                },
                onCancelMission: { missionId in
                    Task { await cancelMission(id: missionId) }
                },
                onCreateNewMission: {
                    showMissionSwitcher = false
                    Task {
                        await workspaceState.loadWorkspaces()
                        if let options = await getValidatedDefaultAgentOptions() {
                            await createNewMission(options: options)
                        } else {
                            showNewMissionSheet = true
                        }
                    }
                },
                onDismiss: {
                    showMissionSwitcher = false
                }
            )
            .presentationDetents([.medium, .large])
            .presentationDragIndicator(.visible)
        }
        .sheet(isPresented: $showQueueSheet) {
            QueueSheet(
                items: queuedItems,
                onRemove: { messageId in
                    Task { await removeFromQueue(messageId: messageId) }
                },
                onClearAll: {
                    Task { await clearQueue() }
                },
                onDismiss: {
                    showQueueSheet = false
                }
            )
            .presentationDetents([.medium, .large])
            .presentationDragIndicator(.visible)
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

    // MARK: - Connection banner

    /// Sticky strip rendered above the conversation when the SSE stream is
    /// down. The toolbar already shows a small wifi-slash glyph, but it's
    /// easy to miss on a long page; this strip stays in peripheral vision.
    private var connectionBanner: some View {
        HStack(spacing: 8) {
            Image(systemName: connectionState.icon)
                .font(.system(size: 11, weight: .semibold))
                .symbolEffect(.pulse, options: .repeating)
            Text(connectionState.label)
                .font(.caption.weight(.medium))
            Spacer()
        }
        .foregroundStyle(Theme.warning)
        .padding(.horizontal, 16)
        .padding(.vertical, 6)
        .background(Theme.warning.opacity(0.12))
        .overlay(alignment: .bottom) {
            Rectangle()
                .fill(Theme.warning.opacity(0.25))
                .frame(height: 0.5)
        }
    }

    // MARK: - Messages
    
    private var messagesView: some View {
        ZStack(alignment: .bottom) {
            // Mount the ScrollView only once we have content. ScrollView's
            // `.defaultScrollAnchor(.bottom)` only takes effect on initial
            // layout — mounting it with empty content would make the anchor a
            // no-op once messages stream in, and we'd have to chase the
            // bottom with explicit scrollTo's. Holding the ScrollView until
            // `messages` is non-empty means its very first layout already has
            // the conversation in it, so the bottom anchor lands the user at
            // the most recent message naturally with no animation, no race.
            if messages.isEmpty {
                if isLoading {
                    LoadingView(message: "Loading conversation...")
                } else if viewingMissionIsRunning {
                    agentWorkingIndicator
                } else {
                    emptyStateView
                }
            } else {
                conversationScrollView
            }
        }
    }

    private var conversationScrollView: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(spacing: 20) {
                    // "Load earlier messages" button when history is truncated
                    if hasMoreHistory {
                        Button {
                            Task { await loadEarlierMessages() }
                        } label: {
                            HStack(spacing: 6) {
                                if isLoadingEarlier {
                                    ProgressView()
                                        .controlSize(.small)
                                        .tint(Theme.accent)
                                } else {
                                    Image(systemName: "arrow.up.circle")
                                }
                                Text("Load earlier messages")
                            }
                            .font(.subheadline.weight(.medium))
                            .foregroundStyle(Theme.accent)
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 12)
                            .background(Theme.accent.opacity(0.08))
                            .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                        }
                        .disabled(isLoadingEarlier)
                    }

                    ForEach(groupedItems) { item in
                        switch item {
                        case .single(let message):
                            MessageBubble(
                                message: message,
                                isCopied: copiedMessageId == message.id,
                                onCopy: { copyMessage(message) }
                            )
                            .id(message.id)
                        case .toolGroup(let groupId, let tools):
                            ToolGroupView(
                                groupId: groupId,
                                tools: tools,
                                expandedGroups: $expandedToolGroups
                            )
                            .id(item.id)
                        }
                    }

                    // Show working indicator after messages when this mission is running but no active streaming item
                    if viewingMissionIsRunning && !hasActiveStreamingItem {
                        agentWorkingIndicator
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
            .defaultScrollAnchor(.bottom)
            .coordinateSpace(name: "scroll")
            .onPreferenceChange(ScrollOffsetPreferenceKey.self) { maxY in
                // Check if we're at the bottom (within 200 points for less flicker)
                isAtBottom = maxY < UIScreen.main.bounds.height + 200
            }
            .onTapGesture {
                // Dismiss keyboard when tapping on messages area
                isInputFocused = false
            }
            .onChange(of: messages.count) { _, _ in
                if let pendingFocusedMessageId {
                    scheduleMessageFocusRetry(proxy: proxy, targetId: pendingFocusedMessageId)
                }
                // Only auto-scroll on message count change if we're at bottom AND not loading historical messages
                // This prevents the jarring animated scroll when loading cached/historical conversations
                if isAtBottom && !isLoadingHistory {
                    scrollToBottom(proxy: proxy)
                }
            }
            .onChange(of: scrollToBottomTick) { _, _ in
                let immediate = shouldScrollImmediately
                shouldScrollImmediately = false
                // Used when the ScrollView is already mounted and the
                // mission's content has been swapped wholesale (mission
                // switch from cache, "load earlier", etc.). The first
                // mount-with-content case relies on `.defaultScrollAnchor`
                // and skips this path entirely. We defer one frame so the
                // LazyVStack has had a tick to lay out the new rows before
                // measuring the anchor.
                Task { @MainActor in
                    try? await Task.sleep(nanoseconds: 16_000_000)
                    scrollToBottom(proxy: proxy, immediate: immediate)
                }
            }
            .onChange(of: pendingFocusedMessageId) { _, targetId in
                guard let targetId else { return }
                scheduleMessageFocusRetry(proxy: proxy, targetId: targetId)
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
    
    private var hasActiveStreamingItem: Bool {
        // Thinking messages don't render in the main content pane any more
        // (they live exclusively in the thoughts sheet), so a live thinking
        // event isn't a "visible streaming item". Without this exclusion the
        // "Agent is working…" indicator would be suppressed while the agent
        // is thinking, leaving the main pane silent.
        messages.contains { msg in
            msg.isPhase || (msg.isToolCall && msg.isActiveToolCall)
        }
    }

    // MARK: - Message Grouping

    /// Groups consecutive tool calls together for collapsed display (like dashboard).
    /// Thinking messages are always elided here — they live in the thoughts sheet
    /// only. Showing them inline duplicated the same content twice on screen.
    private static func buildGroupedItems(from messages: [ChatMessage]) -> [GroupedChatItem] {
        var result: [GroupedChatItem] = []
        var currentToolGroup: [ChatMessage] = []

        func flushToolGroup() {
            guard !currentToolGroup.isEmpty else { return }
            if currentToolGroup.count == 1 {
                result.append(.single(currentToolGroup[0]))
            } else {
                let groupId = currentToolGroup.first?.id ?? UUID().uuidString
                result.append(.toolGroup(groupId: groupId, tools: currentToolGroup))
            }
            currentToolGroup = []
        }

        for message in messages {
            // Thinking renders only in the thoughts sheet, never in the main pane.
            if message.isThinking {
                flushToolGroup()
                continue
            }

            if message.isToolCall && !message.isToolUI {
                // Non-UI tool - add to current group
                currentToolGroup.append(message)
            } else {
                // Other item - flush any pending group first
                flushToolGroup()
                result.append(.single(message))
            }
        }

        // Flush any remaining group
        flushToolGroup()
        return result
    }

    private func recomputeGroupedItems() {
        groupedItems = Self.buildGroupedItems(from: messages)
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
    
    private func scrollToBottom(proxy: ScrollViewProxy, immediate: Bool = false) {
        if immediate {
            // Immediate scroll without animation for loading historical conversations
            proxy.scrollTo(bottomAnchorId, anchor: .bottom)
        } else {
            // Animated scroll for new messages during active conversation
            withAnimation {
                proxy.scrollTo(bottomAnchorId, anchor: .bottom)
            }
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

                Text("Send a message to start a new mission")
                    .font(.subheadline)
                    .foregroundStyle(Theme.textSecondary)
                    .multilineTextAlignment(.center)
                    .lineSpacing(4)
            }

            // Context chips: gives a first-run user visual confirmation of
            // *where* the next message lands and *which* agent will pick it
            // up. Without these, the empty state hides this state behind
            // the toolbar — easy to miss on a fresh install.
            emptyStateContextChips

            Spacer()
            Spacer()
        }
        .padding(.horizontal, 32)
    }

    private var emptyStateContextChips: some View {
        HStack(spacing: 8) {
            if let workspace = workspaceState.selectedWorkspace {
                emptyStateChip(
                    icon: workspace.isDefault ? "macbook" : "shippingbox",
                    label: workspace.isDefault ? "Host" : workspace.name,
                    tint: Theme.accent
                )
            }
            if let agentChip = defaultAgentChipInfo {
                emptyStateChip(
                    icon: agentChip.icon,
                    label: agentChip.label,
                    tint: agentChip.tint
                )
            }
        }
    }

    private func emptyStateChip(icon: String, label: String, tint: Color) -> some View {
        HStack(spacing: 6) {
            Image(systemName: icon)
                .font(.system(size: 11, weight: .semibold))
            Text(label)
                .font(.caption.weight(.medium))
                .lineLimit(1)
        }
        .foregroundStyle(tint)
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
        .background(tint.opacity(0.12))
        .clipShape(Capsule())
        .overlay(
            Capsule().stroke(tint.opacity(0.25), lineWidth: 0.5)
        )
    }

    /// Resolve the saved default agent into a chip, or `nil` if the user
    /// hasn't saved one (in which case the picker fires on first send).
    private var defaultAgentChipInfo: (icon: String, label: String, tint: Color)? {
        guard
            let saved = UserDefaults.standard.string(forKey: "default_agent"),
            !saved.isEmpty,
            let parsed = CombinedAgent.parse(saved)
        else {
            return nil
        }
        return (
            BackendAgentService.icon(for: parsed.backend),
            parsed.agent,
            BackendAgentService.color(for: parsed.backend)
        )
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

    private var hasInput: Bool {
        !inputText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    /// Visible slash commands for the current backend, filtered by the
    /// prefix after the leading `/` in `inputText`. Empty when the input
    /// doesn't start with `/`, when the catalog hasn't loaded yet, or when
    /// nothing matches the filter.
    private var slashSuggestions: [SlashCommand] {
        guard let catalog = slashCommandCatalog else { return [] }
        let trimmed = inputText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.hasPrefix("/") else { return [] }
        // Pull the command name fragment (everything after `/`, before the
        // first whitespace). e.g. "/goal create file" → "goal".
        let afterSlash = String(trimmed.dropFirst())
        let nameFragment: String = {
            if let space = afterSlash.firstIndex(where: { $0.isWhitespace }) {
                return String(afterSlash[..<space])
            }
            return afterSlash
        }()
        // Once the user has typed a space after the command name they're
        // entering args — hide the popover so it doesn't cover the input.
        if afterSlash.contains(where: { $0.isWhitespace }) { return [] }
        let backend = viewingMission?.backend ?? currentMission?.backend
        let pool: [SlashCommand]
        switch backend {
        case "codex": pool = catalog.codex ?? []
        case "claudecode": pool = catalog.claudecode
        case "opencode": pool = catalog.opencode
        default:
            // No mission yet → show every backend's commands so the user
            // can preview what's available.
            pool = catalog.opencode
                + catalog.claudecode
                + (catalog.codex ?? [])
        }
        return pool.filter { $0.matchesPrefix(nameFragment) }
    }

    /// Replace the composer text with `/<name> ` (trailing space ready for
    /// args) when the user picks a suggestion.
    private func applySlashCommand(_ cmd: SlashCommand) {
        inputText = "/\(cmd.name) "
        isInputFocused = true
    }

    /// Lazy-load the slash command catalog the first time the user types
    /// `/`. Best-effort: failures just leave the popover empty.
    private func loadSlashCommandsIfNeeded() {
        guard slashCommandCatalog == nil, !slashCommandLoading else { return }
        slashCommandLoading = true
        Task {
            defer { slashCommandLoading = false }
            do {
                slashCommandCatalog = try await APIService.shared.getBuiltinCommands()
            } catch {
                // Silent failure — popover stays empty until next attempt.
                print("Failed to load slash commands: \(error)")
            }
        }
    }

    /// Goal-mode pill — sits above the composer while a codex `/goal`
    /// continuation loop is active. State is fed by `goal_iteration` /
    /// `goal_status` SSE events; cleared automatically on terminal status.
    @ViewBuilder
    private var goalPill: some View {
        if let goal = goalInfo {
            HStack(spacing: 6) {
                Image(systemName: "target")
                    .font(.system(size: 10, weight: .semibold))
                Text("Goal")
                    .font(.caption2.weight(.semibold))
                Text("·")
                    .foregroundStyle(Theme.accent.opacity(0.6))
                Text(
                    goal.status == "active"
                        ? "iter \(goal.iteration)"
                        : goal.status
                )
                .font(.caption2.weight(.medium))
                if !goal.objective.isEmpty {
                    Text("·")
                        .foregroundStyle(Theme.accent.opacity(0.6))
                    Text(goal.objective)
                        .font(.caption2)
                        .foregroundStyle(Theme.accent.opacity(0.7))
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
            }
            .foregroundStyle(Theme.accent)
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .background(Theme.accent.opacity(0.12))
            .clipShape(Capsule())
            .overlay(
                Capsule().stroke(Theme.accent.opacity(0.3), lineWidth: 0.5)
            )
            .padding(.top, 8)
            .transition(.move(edge: .bottom).combined(with: .opacity))
        }
    }

    private var inputView: some View {
        VStack(spacing: 0) {
            // Slash-command popover. Renders above the composer when the
            // input starts with `/` and we have at least one matching
            // command. Tap-to-insert rewrites the input and refocuses.
            SlashCommandSuggestions(
                commands: slashSuggestions,
                onSelect: applySlashCommand
            )
            .animation(.easeInOut(duration: 0.15), value: slashSuggestions.count)

            goalPill

            // Queue indicator above input when agent is busy with queued messages
            if runState != .idle && queueLength > 0 {
                Button {
                    Task { await loadQueueItems() }
                    showQueueSheet = true
                    HapticService.lightTap()
                } label: {
                    HStack(spacing: 6) {
                        Image(systemName: "clock.badge.questionmark")
                            .font(.caption2)
                        Text("\(queueLength) message\(queueLength == 1 ? "" : "s") queued")
                            .font(.caption2.weight(.medium))
                        Image(systemName: "chevron.right")
                            .font(.system(size: 8, weight: .semibold))
                    }
                    .foregroundStyle(Theme.warning)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 6)
                    .background(Theme.warning.opacity(0.1))
                    .clipShape(Capsule())
                }
                .padding(.top, 8)
                .transition(.move(edge: .bottom).combined(with: .opacity))
            }

            HStack(alignment: .center, spacing: 0) {
                // Stop button - only visible when agent is running
                if runState != .idle {
                    Button {
                        Task { await cancelRun() }
                    } label: {
                        Image(systemName: "stop.fill")
                            .font(.system(size: 14, weight: .semibold))
                            .foregroundStyle(.white)
                            .frame(width: 32, height: 32)
                            .background(Theme.error)
                            .clipShape(Circle())
                    }
                    .padding(.leading, 8)
                    .transition(.scale.combined(with: .opacity))
                }

                // Text input
                TextField(runState != .idle ? "Queue a follow-up..." : "Message the agent...", text: $inputText, axis: .vertical)
                    .textFieldStyle(.plain)
                    .font(.body)
                    .foregroundStyle(Theme.textPrimary)
                    .lineLimit(1...5)
                    .padding(.leading, runState != .idle ? 8 : 16)
                    .padding(.trailing, 8)
                    .padding(.vertical, 12)
                    .focused($isInputFocused)
                    .submitLabel(.send)
                    .onSubmit {
                        sendMessage()
                    }
                    .onChange(of: inputText) { _, newValue in
                        // Lazy-load the slash catalog the first time the user
                        // starts typing a slash command. Avoids a fetch on
                        // every fresh ControlView mount when slashes are rare.
                        if newValue.trimmingCharacters(in: .whitespaces).hasPrefix("/") {
                            loadSlashCommandsIfNeeded()
                        }
                    }

                // Send button - always available when there's text
                Button {
                    sendMessage()
                } label: {
                    Image(systemName: "arrow.up")
                        .font(.system(size: 14, weight: .semibold))
                        .foregroundStyle(hasInput ? .white : Theme.textMuted)
                        .frame(width: 32, height: 32)
                        .background(hasInput ? Theme.accent : Color.clear)
                        .clipShape(Circle())
                        .overlay(
                            Circle()
                                .stroke(!hasInput ? Theme.border : Color.clear, lineWidth: 1)
                        )
                }
                .disabled(!hasInput)
                .padding(.trailing, 8)
            }
            .animation(.easeInOut(duration: 0.15), value: runState)
            .animation(.easeInOut(duration: 0.15), value: hasInput)
            .clipShape(RoundedRectangle(cornerRadius: 24, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 24, style: .continuous)
                    .stroke(Theme.border, lineWidth: 1)
            )
            .padding(.horizontal, 16)
            .padding(.top, 12)
            .padding(.bottom, 16)
        }
        .animation(.easeInOut(duration: 0.2), value: queueLength > 0 && runState != .idle)
    }
    
    // MARK: - Actions

    // MARK: - Mission Caching with LRU Eviction

    // Cache both mission metadata and events for consistent display
    private struct CachedMissionData: Codable {
        let mission: Mission
        let events: [StoredEvent]
        let cachedAt: Date
    }

    private static let maxCachedMissions = 10  // Limit cache size
    /// Legacy key prefix used when mission blobs lived in UserDefaults. Kept
    /// only so the one-time migration below can purge them on first launch
    /// after upgrade — every payload bloats cfprefsd's in-memory plist.
    private static let cachePrefix = "cached_mission_"
    private static let cacheKeysKey = "cached_mission_keys"
    private static let didMigrateCacheKey = "did_migrate_mission_cache_v1"

    /// One-shot migration: previous versions stored mission events as raw
    /// JSON blobs in UserDefaults. Those blobs are loaded into memory by
    /// cfprefsd at process start, costing RAM forever. Read each blob,
    /// rewrite it to disk, then erase the UserDefaults key. Idempotent —
    /// guarded by a separate flag so a clean install doesn't run the loop.
    static func migrateMissionCacheIfNeeded() {
        let defaults = UserDefaults.standard
        guard !defaults.bool(forKey: didMigrateCacheKey) else { return }
        let keys = defaults.stringArray(forKey: cacheKeysKey) ?? []
        for missionId in keys {
            let legacyKey = cachePrefix + missionId
            if let data = defaults.data(forKey: legacyKey) {
                try? writeCachedMissionFile(missionId: missionId, data: data)
                defaults.removeObject(forKey: legacyKey)
            }
        }
        defaults.set(true, forKey: didMigrateCacheKey)
    }

    // Cache mission with events for faster loading and consistent display.
    //
    // Storage moved off `UserDefaults` (which loads its entire backing plist
    // into the cfprefsd daemon and the app process at launch, then writes
    // synchronously on the main thread) to per-mission JSON files in
    // `Caches/`. Writes are dispatched to a background queue so a multi-MB
    // event payload no longer freezes the chat thread while it serialises.
    private func cacheMissionWithEvents(_ mission: Mission, events: [StoredEvent]) {
        let missionId = mission.id
        let cacheData = CachedMissionData(mission: mission, events: events, cachedAt: Date())

        // LRU key list still lives in UserDefaults — it's a tiny string array
        // so it's free, and keeping it there means the LRU survives Caches
        // eviction by iOS (we'll just miss on the orphaned files).
        var cachedKeys = UserDefaults.standard.stringArray(forKey: Self.cacheKeysKey) ?? []
        cachedKeys.removeAll { $0 == missionId }

        var evicted: String?
        if cachedKeys.count >= Self.maxCachedMissions {
            evicted = cachedKeys.first
            cachedKeys.removeFirst()
        }
        cachedKeys.append(missionId)
        UserDefaults.standard.set(cachedKeys, forKey: Self.cacheKeysKey)

        // Encode on @MainActor (CachedMissionData transitively contains
        // `AnyCodable.value: Any`, which isn't Sendable, so we can't ship
        // the struct to a detached task), then hand the raw bytes off for
        // the actual filesystem write. `Data` is Sendable, so the write
        // dispatch is clean. The disk write is the bigger UI-thread hazard
        // on large missions anyway — encoding is bounded CPU; writing is
        // unbounded I/O blocked on the filesystem coordinator.
        let encoded = try? JSONEncoder().encode(cacheData)
        let evictedId = evicted
        Task.detached(priority: .utility) {
            if let evictedId {
                Self.deleteCachedMissionFile(missionId: evictedId)
            }
            if let encoded {
                try? Self.writeCachedMissionFile(missionId: missionId, data: encoded)
            }
        }
    }

    private func loadCachedMissionData(_ missionId: String) -> CachedMissionData? {
        // Synchronous read on the call site (cold start) — the file is
        // bounded by `loadEarlierPageLimit` * StoredEvent size (~few MB
        // worst case) and decoding it on @MainActor is unavoidable here
        // because the caller needs the result immediately to render the
        // first frame. Subsequent caches written in the background.
        guard let url = Self.cacheFileURL(missionId: missionId),
              let data = try? Data(contentsOf: url),
              let cached = try? JSONDecoder().decode(CachedMissionData.self, from: data) else {
            return nil
        }

        if var cachedKeys = UserDefaults.standard.stringArray(forKey: Self.cacheKeysKey) {
            cachedKeys.removeAll { $0 == missionId }
            cachedKeys.append(missionId)
            UserDefaults.standard.set(cachedKeys, forKey: Self.cacheKeysKey)
        }

        return cached
    }

    private func removeMissionFromCache(_ missionId: String) {
        Self.deleteCachedMissionFile(missionId: missionId)

        // Remove from LRU tracking
        if var cachedKeys = UserDefaults.standard.stringArray(forKey: Self.cacheKeysKey) {
            cachedKeys.removeAll { $0 == missionId }
            UserDefaults.standard.set(cachedKeys, forKey: Self.cacheKeysKey)
        }
    }

    /// Cache directory for mission JSON. `.cachesDirectory` is appropriate
    /// here: iOS may evict files under memory pressure, but losing them only
    /// means a cache miss on next open, which costs one network round-trip.
    /// (Compared to `.documentDirectory` which would be backed-up to iCloud
    /// and counted toward the app's storage quota.)
    nonisolated private static func cacheFileURL(missionId: String) -> URL? {
        guard let caches = try? FileManager.default.url(
            for: .cachesDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: true
        ) else { return nil }
        let dir = caches.appendingPathComponent("missions", isDirectory: true)
        // mkdir on first use. Best-effort — if it fails, the write below
        // will fail too and the caller falls through to network.
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        // mission IDs are server-generated UUIDs in practice; sanitise
        // anyway so we never write `../etc/passwd`-style paths.
        let safeId = missionId.replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "..", with: "_")
        return dir.appendingPathComponent("\(safeId).json", isDirectory: false)
    }

    nonisolated private static func writeCachedMissionFile(missionId: String, data: Data) throws {
        guard let url = cacheFileURL(missionId: missionId) else { return }
        // Atomic write — partial files left after a crash would fail to
        // decode on next open and trigger an unnecessary network fetch.
        try data.write(to: url, options: [.atomic])
    }

    nonisolated private static func deleteCachedMissionFile(missionId: String) {
        guard let url = cacheFileURL(missionId: missionId) else { return }
        try? FileManager.default.removeItem(at: url)
    }

    private func applyViewingMission(_ mission: Mission, scrollToBottom: Bool = true) {
        isLoadingHistory = true  // Suppress animated auto-scroll during history load

        viewingMission = mission
        viewingMissionId = mission.id
        hasMoreHistory = false
        loadedEventCount = 0
        messages = mission.history.enumerated().map { index, entry in
            ChatMessage(
                id: "\(mission.id)-\(index)",
                type: entry.isUser ? .user : .assistant(success: true, costCents: 0, costSource: .unknown, model: nil, sharedFiles: nil),
                content: entry.content
            )
        }
        recomputeGroupedItems()

        if scrollToBottom {
            shouldScrollImmediately = true
            scrollToBottomTick += 1
        }
        clearLoadingHistoryAfterRender()
    }

    private static let initialEventLimit = 50

    private func applyViewingMissionWithEvents(_ mission: Mission, events: [StoredEvent], scrollToBottom: Bool = true) {
        isLoadingHistory = true  // Suppress animated auto-scroll during history load

        viewingMission = mission
        viewingMissionId = mission.id

        // Ensure deterministic replay order in case the backend returns unsorted results
        let orderedEvents = events.sorted { lhs, rhs in
            if lhs.sequence != rhs.sequence {
                return lhs.sequence < rhs.sequence
            }
            if lhs.timestamp != rhs.timestamp {
                return lhs.timestamp < rhs.timestamp
            }
            return lhs.id < rhs.id
        }

        // Track total event count for pagination
        loadedEventCount = orderedEvents.count

        // Clear and replay all events to rebuild message history
        messages.removeAll()

        for event in orderedEvents {
            var data: [String: Any] = [:]

            // Add metadata first (lower priority)
            for (key, value) in event.metadata {
                data[key] = value.value
            }

            // Add core fields last (higher priority - these should never be overwritten)
            data["mission_id"] = event.missionId
            data["content"] = event.content

            if let eventId = event.eventId {
                data["id"] = eventId
            }
            if let toolCallId = event.toolCallId {
                data["tool_call_id"] = toolCallId
            }
            if let toolName = event.toolName {
                data["name"] = toolName
            }

            handleStreamEvent(type: event.eventType, data: data, isHistoricalReplay: true)
        }

        // Recompute grouped items once after all events are processed
        recomputeGroupedItems()

        if scrollToBottom {
            shouldScrollImmediately = true
            scrollToBottomTick += 1
        }
        clearLoadingHistoryAfterRender()
    }

    /// Clear `isLoadingHistory` on the next runloop tick. Setting it to `false`
    /// in the same synchronous block where it was set to `true` would coalesce
    /// into a single observed value, defeating the `messages.count` onChange
    /// guard that is supposed to suppress an animated auto-scroll during
    /// content replacement. Deferring lets the count-change handler observe
    /// `isLoadingHistory == true`, after which the explicit scroll path wins.
    private func clearLoadingHistoryAfterRender() {
        Task { @MainActor in
            try? await Task.sleep(nanoseconds: 16_000_000)
            isLoadingHistory = false
        }
    }

    /// Append delta events to the existing conversation without clearing.
    ///
    /// Used by the SSE-reconnect / scene-phase-active resume path: the server
    /// returns only events with `sequence > knownMaxSeq`, so they must be
    /// appended (not replayed-from-empty). Each event is fed through
    /// `handleStreamEvent` as a historical replay — the request includes
    /// `text_delta` events, so without the historical flag the live-only
    /// `upsertStreamingFallbackThought` path would synthesize duplicate
    /// thinking content from already-finalized text. Per-id dedup guards on
    /// `user_message`, `assistant_message`, and `tool_call` further protect
    /// against overlap with events we already rendered live.
    private func applyDeltaEvents(_ events: [StoredEvent]) {
        guard !events.isEmpty else { return }

        let orderedEvents = events.sorted { lhs, rhs in
            if lhs.sequence != rhs.sequence { return lhs.sequence < rhs.sequence }
            if lhs.timestamp != rhs.timestamp { return lhs.timestamp < rhs.timestamp }
            return lhs.id < rhs.id
        }

        for event in orderedEvents {
            var data: [String: Any] = [:]
            for (key, value) in event.metadata {
                data[key] = value.value
            }
            data["mission_id"] = event.missionId
            data["content"] = event.content
            if let eventId = event.eventId { data["id"] = eventId }
            if let toolCallId = event.toolCallId { data["tool_call_id"] = toolCallId }
            if let toolName = event.toolName { data["name"] = toolName }

            handleStreamEvent(type: event.eventType, data: data, isHistoricalReplay: true)
        }

        loadedEventCount += orderedEvents.count
        recomputeGroupedItems()
    }

    /// Decide what to load on cold start. The previous code did
    /// `loadMission(savedId)` then *also* awaited `loadCurrentMission` as
    /// "background context" — but the await was serial, so the user paid
    /// for both round-trips before the UI became interactive. The
    /// "background" fetch is now genuinely backgrounded: kick off the
    /// secondary `loadCurrentMission` in a detached Task so the primary
    /// mission's events render as soon as they arrive.
    private func loadInitialMission() async {
        if let pendingId = nav.consumePendingMission() {
            await loadMission(id: pendingId)
            Task { await self.loadCurrentMission(updateViewing: false) }
        } else if let savedId = UserDefaults.standard.string(forKey: Self.lastMissionIdKey) {
            await loadMission(id: savedId)
            Task { await self.loadCurrentMission(updateViewing: false) }
        } else {
            await loadCurrentMission(updateViewing: true)
        }
    }

    private func loadCurrentMission(updateViewing: Bool) async {
        // Try to load cached version first for immediate display with consistent event-based rendering
        let hasCache: Bool
        if updateViewing, let currentId = currentMission?.id ?? viewingMissionId,
           let cachedData = loadCachedMissionData(currentId) {
            // Use cached events for consistent display (avoids flash when fresh data arrives)
            currentMission = cachedData.mission
            applyViewingMissionWithEvents(cachedData.mission, events: cachedData.events)
            hasCache = true
        } else {
            hasCache = false
        }

        // Only show loading state if we don't have cached data to display
        if !hasCache {
            isLoading = true
        }
        defer { isLoading = false }

        do {
            if let mission = try await api.getCurrentMission() {
                currentMission = mission

                // Fetch events for event-based display
                if updateViewing || viewingMissionId == nil || viewingMissionId == mission.id {
                    do {
                        let eventTypes = ["user_message", "assistant_message", "tool_call", "tool_result", "text_delta", "thinking"]
                        let result = try await api.getMissionEventsWithMeta(id: mission.id, types: eventTypes, limit: Self.initialEventLimit, latest: true)
                        let events = result.events

                        if events.isEmpty {
                            // Clear stale cache when events are empty
                            removeMissionFromCache(mission.id)
                            applyViewingMission(mission)
                        } else {
                            hasMoreHistory = events.count >= Self.initialEventLimit
                            applyViewingMissionWithEvents(mission, events: events)
                            // Seed delta cursor only when the backend advertises support
                            // via X-Max-Sequence — otherwise a delta call would be
                            // ignored and return offset=0 rows.
                            if let maxSeq = result.maxSequence, maxSeq > 0 {
                                missionMaxSeq[mission.id] = maxSeq
                            }
                            // Update cache with fresh data
                            cacheMissionWithEvents(mission, events: events)
                        }
                    } catch {
                        print("Failed to load mission events: \(error)")
                        // If we already displayed cached data, keep it and don't flash to basic view
                        // Only clear cache and fall back if we didn't have cached data to begin with
                        if !hasCache {
                            removeMissionFromCache(mission.id)
                            applyViewingMission(mission)
                        }
                        // Otherwise: keep the cached view displayed, don't cause a flash
                    }
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

        // Clear stale workers from previous mission immediately
        childMissions = []

        // Try to load cached version first for immediate display with consistent event-based rendering
        let hasCache: Bool
        if let cachedData = loadCachedMissionData(id) {
            // Use cached events for consistent display (avoids flash when fresh data arrives)
            applyViewingMissionWithEvents(cachedData.mission, events: cachedData.events)
            hasCache = true
        } else {
            hasCache = false
        }

        // Only show loading state if we don't have cached data to display
        if !hasCache {
            isLoading = true
        }

        do {
            // Fetch mission metadata first (required)
            let mission = try await api.getMission(id: id)

            // Race condition guard: only update if this is still the mission we want
            guard fetchingMissionId == id else {
                return // Another mission was requested, discard this response
            }

            if currentMission?.id == mission.id {
                currentMission = mission
            }

            // Try to fetch full event history (optional - fall back to basic history if it fails)
            do {
                // Fetch all relevant event types including thinking events (matching web dashboard behavior)
                let result = try await api.getMissionEventsWithMeta(id: id, types: historyEventTypes, limit: Self.initialEventLimit, latest: true)
                let events = result.events

                // Race condition guard after the second await
                guard fetchingMissionId == id else {
                    return
                }

                if events.isEmpty {
                    // Clear stale cache when events are empty to prevent visual flashing
                    removeMissionFromCache(mission.id)
                    applyViewingMission(mission)
                } else {
                    hasMoreHistory = events.count >= Self.initialEventLimit
                    applyViewingMissionWithEvents(mission, events: events)
                    if let maxSeq = result.maxSequence, maxSeq > 0 {
                        missionMaxSeq[id] = maxSeq
                    }
                    // Cache the mission with events for next time
                    cacheMissionWithEvents(mission, events: events)
                }
            } catch {
                print("Failed to load mission events (falling back to basic history): \(error)")
                guard fetchingMissionId == id else {
                    return
                }
                // If we already displayed cached data, keep it and don't flash to basic view
                // Only clear cache and fall back if we didn't have cached data to begin with
                if !hasCache {
                    removeMissionFromCache(mission.id)
                    applyViewingMission(mission)
                }
                // Otherwise: keep the cached view displayed, don't cause a flash
            }

            isLoading = false
            HapticService.success()

            // Fetch child (worker) missions in background
            Task {
                if let workers = try? await api.getChildMissions(parentId: id) {
                    guard fetchingMissionId == id else { return }
                    childMissions = workers
                } else {
                    guard fetchingMissionId == id else { return }
                    childMissions = []
                }
            }
        } catch {
            // Race condition guard
            guard fetchingMissionId == id else { return }

            isLoading = false
            childMissions = []
            print("Failed to load mission: \(error)")

            // Revert viewing state to avoid filtering out events
            if let fallback = previousViewingMission ?? currentMission {
                applyViewingMission(fallback, scrollToBottom: false)
            } else {
                viewingMissionId = previousViewingId
            }
        }
    }

    /// Hard cap on the "Load earlier" payload. The previous implementation
    /// passed `types:` only with no `limit`, asking the server for the entire
    /// mission history — on a 50k-event mission that's a multi-MB JSON
    /// download blocking the chat behind a spinner. Pull a single large page
    /// instead; if the user truly needs to scroll past that, they can tap
    /// again. Keeps cold-load worst-case bounded.
    private static let loadEarlierPageLimit = 1000

    // Load earlier messages when user taps "Load earlier" button
    private func loadEarlierMessages() async {
        guard let missionId = viewingMissionId, !isLoadingEarlier else { return }
        isLoadingEarlier = true
        defer { isLoadingEarlier = false }

        do {
            let allEvents = try await api.getMissionEvents(
                id: missionId,
                types: historyEventTypes,
                limit: Self.loadEarlierPageLimit
            )
            guard viewingMissionId == missionId else { return }

            if !allEvents.isEmpty, let mission = viewingMission {
                // Only mark history exhausted if we got fewer rows than the
                // page cap. If the server returned a full page, more may
                // remain — keep the button visible.
                hasMoreHistory = allEvents.count >= Self.loadEarlierPageLimit
                applyViewingMissionWithEvents(mission, events: allEvents, scrollToBottom: false)
                cacheMissionWithEvents(mission, events: allEvents)
            }
        } catch {
            print("Failed to load earlier messages: \(error)")
            HapticService.error()
        }
    }

    /// Hard cap on the number of events per delta page. Mirrors the web client.
    private static let deltaResumePageLimit = 5000

    /// Outcome of a `tryDeltaResume` attempt.
    private enum DeltaResumeOutcome {
        case applied      // backend supports resume, events applied, cursor advanced
        case viewChanged  // user navigated away mid-request; nothing applied, no
                          // fallback should run (the new view will refetch on its own)
        case unsupported  // backend stripped `X-Max-Sequence` — cursor dropped
        case noCursor     // no high-water mark recorded; nothing attempted
        case failed       // network/server error; cursor untouched
    }

    /// Try to fetch and apply only the events that arrived after our recorded
    /// high-water mark for this mission. Shared between `resumeMissionAfterReconnect`
    /// (post SSE reconnect) and `reloadMissionFromServer` (post scene-phase active).
    /// Caller is responsible for the fallback path when this returns anything
    /// other than `.applied`.
    private func tryDeltaResume(missionId id: String) async -> DeltaResumeOutcome {
        guard let knownSeq = missionMaxSeq[id] else { return .noCursor }
        do {
            let result = try await api.getMissionEventsWithMeta(
                id: id,
                types: historyEventTypes,
                limit: Self.deltaResumePageLimit,
                sinceSeq: knownSeq
            )
            guard viewingMissionId == id else { return .viewChanged }
            guard let maxSeq = result.maxSequence else {
                // Backend stripped `X-Max-Sequence` — drop the cursor so the
                // delta path is disabled until the next header-bearing fetch.
                missionMaxSeq.removeValue(forKey: id)
                return .unsupported
            }
            applyDeltaEvents(result.events)
            // If the page was capped by the limit, advance the cursor to the
            // largest sequence we actually saw so the next call resumes from
            // there — otherwise we'd skip rows between this page and the
            // true max. Use `max()` rather than `last`: the API contract is
            // ASC-by-sequence but defensive callers shouldn't trust input
            // ordering.
            let pageMax = result.events.map { $0.sequence }.max() ?? knownSeq
            let cursor = (result.events.count >= Self.deltaResumePageLimit && pageMax < maxSeq) ? pageMax : maxSeq
            missionMaxSeq[id] = cursor
            return .applied
        } catch {
            print("Delta resume failed: \(error)")
            return .failed
        }
    }

    // Reload mission from server without showing loading state or cache.
    // Called when the scene becomes active to catch missed SSE events (mirrors
    // the web's visibility-change handler). Prefers the `since_seq` delta path
    // when supported; falls back to a tail reload only when no high-water mark
    // is recorded for this mission yet. `skipDeltaAttempt=true` is used by
    // callers that just tried delta and got `.failed` — no point hammering
    // the same flaky network for the same answer twice.
    private func reloadMissionFromServer(id: String, skipDeltaAttempt: Bool = false) async {
        guard viewingMissionId == id else { return }

        do {
            let mission = try await api.getMission(id: id)
            guard viewingMissionId == id else { return }

            if currentMission?.id == mission.id {
                currentMission = mission
            }

            if !skipDeltaAttempt {
                switch await tryDeltaResume(missionId: id) {
                case .applied, .viewChanged:
                    return
                case .noCursor, .unsupported, .failed:
                    break  // fall through to full tail reload below
                }
            }

            // Fallback / first-time path: fetch the recent tail. Distinguish
            // a *failed* fetch (network/server error) from a *successful but
            // empty* response. On failure we keep the currently rendered
            // conversation — silently downgrading to `applyViewingMission`
            // (which uses the basic `mission.history` payload) would erase
            // the event-rendered chat under flaky networks. On empty we
            // clear the cache and fall back, since the mission really does
            // have no events.
            let limit = hasMoreHistory ? Self.initialEventLimit : nil
            let latest = hasMoreHistory
            do {
                let result = try await api.getMissionEventsWithMeta(id: id, types: historyEventTypes, limit: limit, latest: latest)
                guard viewingMissionId == id else { return }
                if result.events.isEmpty {
                    removeMissionFromCache(mission.id)
                    applyViewingMission(mission, scrollToBottom: false)
                } else {
                    applyViewingMissionWithEvents(mission, events: result.events, scrollToBottom: false)
                    if let maxSeq = result.maxSequence, maxSeq > 0 {
                        missionMaxSeq[id] = maxSeq
                    }
                    cacheMissionWithEvents(mission, events: result.events)
                }
            } catch {
                // Tail fetch failed — keep the existing rendered conversation.
                // SSE will deliver any new events; next active/visible cycle
                // will retry the fetch.
                print("Tail reload fetch failed, keeping current view: \(error)")
            }
        } catch {
            print("Failed to reload mission from server: \(error)")
        }
    }

    /// Resume a viewing mission after an SSE reconnect. Same delta-first logic
    /// as `reloadMissionFromServer` but without the mission-metadata refetch
    /// (the SSE stream itself will deliver any metadata changes). Keeps the
    /// reconnect catch-up fast even on missions with thousands of events.
    private func resumeMissionAfterReconnect(id: String) async {
        guard viewingMissionId == id else { return }
        switch await tryDeltaResume(missionId: id) {
        case .applied, .viewChanged:
            return
        case .noCursor, .unsupported:
            // Cursor wasn't usable — fall back to a tail reload. Skip its
            // delta retry since the cursor state is what just told us the
            // delta path isn't available right now.
            await reloadMissionFromServer(id: id, skipDeltaAttempt: true)
        case .failed:
            // Transient network/server error — same skip rationale, plus
            // avoid hammering a flaky connection with the identical request.
            await reloadMissionFromServer(id: id, skipDeltaAttempt: true)
        }
    }

    private func createNewMission(options: NewMissionOptions? = nil) async {
        do {
            let mission = try await api.createMission(
                workspaceId: options?.workspaceId,
                title: nil,
                agent: options?.agent,
                modelOverride: options?.modelOverride,
                backend: options?.backend
            )
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

        await resumeMission(id: mission.id)
    }

    private func resumeMission(id: String) async {
        do {
            let resumed = try await api.resumeMission(id: id)
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

    private func followUpPrompt(for mission: Mission) -> String {
        let baseTitle = mission.displayTitle.trimmingCharacters(in: .whitespacesAndNewlines)
        if baseTitle.isEmpty || baseTitle == "Untitled Mission" {
            return "Follow up on this mission with the next concrete implementation steps."
        }
        return "Follow up on \"\(baseTitle)\" and implement the next concrete steps."
    }

    private func createFollowUpMission(from sourceMission: Mission) async {
        do {
            let mission = try await api.createMission(
                workspaceId: sourceMission.workspaceId,
                title: nil,
                agent: sourceMission.agent,
                modelOverride: sourceMission.modelOverride,
                backend: sourceMission.backend
            )
            currentMission = mission
            applyViewingMission(mission, scrollToBottom: false)
            inputText = followUpPrompt(for: sourceMission)
            isInputFocused = true

            // Refresh running missions to keep switcher state in sync.
            await refreshRunningMissions()

            HapticService.success()
        } catch {
            print("Failed to create follow-up mission: \(error)")
            HapticService.error()
        }
    }

    private func normalizeSearchText(_ text: String) -> String {
        let lowered = text.lowercased()
        let scalars = lowered.unicodeScalars.map { scalar -> Character in
            if scalar.properties.isAlphabetic
                || scalar.properties.numericType != nil
                || CharacterSet.whitespacesAndNewlines.contains(scalar)
            {
                return Character(scalar)
            }
            return " "
        }
        return String(scalars)
            .split(whereSeparator: \.isWhitespace)
            .joined(separator: " ")
    }

    private func findMessageIdForEntryIndex(_ entryIndex: Int, snippet: String?) -> String? {
        guard entryIndex >= 0 else { return nil }

        enum HistoryRoleCategory {
            case user
            case assistant
            case toolCall
            case toolResult
            case other
        }

        let roleCategory: (String) -> HistoryRoleCategory = { role in
            switch role {
            case "user":
                return .user
            case "assistant":
                return .assistant
            case "tool", "tool_call":
                return .toolCall
            case "tool_result":
                return .toolResult
            default:
                return .other
            }
        }

        let messageSearchText: (ChatMessage) -> String = { message in
            if message.isToolCall {
                let toolName = message.toolCallName ?? ""
                let argsText = message.toolData?.argsString ?? ""
                let resultText = message.toolData?.resultString ?? ""
                return "\(toolName) \(message.content) \(argsText) \(resultText)"
            }
            return message.content
        }
        let isToolResultMessage: (ChatMessage) -> Bool = { message in
            if let resultText = message.toolData?.resultString,
               !resultText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            {
                return true
            }
            return false
        }
        let roleMatchesMessage: (String, ChatMessage) -> Bool = { role, message in
            switch roleCategory(role) {
            case .user:
                return message.isUser
            case .assistant:
                return message.isAssistant
            case .toolResult:
                return isToolResultMessage(message)
            case .toolCall:
                return message.isToolCall
            case .other:
                return false
            }
        }
        let roleMatchesHistoryCategory: (String, HistoryRoleCategory) -> Bool = { role, category in
            roleCategory(role) == category
        }

        if let history = viewingMission?.history, entryIndex < history.count {
            let entry = history[entryIndex]
            let entryRole = entry.role.lowercased()
            let entryText = normalizeSearchText(entry.content)
            let targetCategory = roleCategory(entryRole)
            let roleOccurrence = history
                .prefix(entryIndex + 1)
                .filter { roleMatchesHistoryCategory($0.role.lowercased(), targetCategory) }
                .count
            let matchingMessages = messages.filter { roleMatchesMessage(entryRole, $0) }
            let targetMessageIndex = max(roleOccurrence - 1, 0)

            if let snippet, !snippet.isEmpty {
                let normalizedSnippet = normalizeSearchText(snippet)
                if !normalizedSnippet.isEmpty {
                    let snippetMatches = matchingMessages.enumerated().filter { _, message in
                        normalizeSearchText(messageSearchText(message)).contains(normalizedSnippet)
                    }
                    if let matched = snippetMatches.min(by: {
                        abs($0.offset - targetMessageIndex) < abs($1.offset - targetMessageIndex)
                    })?.element {
                        return matched.id
                    }
                }
            }
            if !entryText.isEmpty {
                let entryMatches = matchingMessages.enumerated().filter { _, message in
                    normalizeSearchText(messageSearchText(message)).contains(entryText)
                }
                if let matched = entryMatches.min(by: {
                    abs($0.offset - targetMessageIndex) < abs($1.offset - targetMessageIndex)
                })?.element {
                    return matched.id
                }
            }
            if targetMessageIndex < matchingMessages.count {
                return matchingMessages[targetMessageIndex].id
            }
            if let last = matchingMessages.last {
                return last.id
            }
        }

        guard let snippet, !snippet.isEmpty else { return nil }
        let normalizedSnippet = normalizeSearchText(snippet)
        guard !normalizedSnippet.isEmpty else { return nil }

        let best = messages.first { message in
            guard message.isUser || message.isAssistant || message.isToolCall else { return false }
            return normalizeSearchText(messageSearchText(message)).contains(normalizedSnippet)
        }
        return best?.id
    }

    private func scheduleMessageFocusRetry(
        proxy: ScrollViewProxy,
        targetId: String,
        attempt: Int = 0
    ) {
        guard pendingFocusedMessageId == targetId else { return }

        let canFocusNow = messages.contains { $0.id == targetId }
        if canFocusNow {
            withAnimation(.easeInOut(duration: 0.25)) {
                proxy.scrollTo(targetId, anchor: .center)
            }
            // Keep trying a couple more frames so late-mounted rows still get focused.
            if attempt >= 2 {
                pendingFocusedMessageId = nil
                return
            }
        }

        let maxAttempts = 10
        guard attempt < maxAttempts else {
            pendingFocusedMessageId = nil
            return
        }

        DispatchQueue.main.asyncAfter(deadline: .now() + 0.12) {
            self.scheduleMessageFocusRetry(proxy: proxy, targetId: targetId, attempt: attempt + 1)
        }
    }

    private func openFailingToolCall(for missionId: String) async {
        if viewingMissionId != missionId {
            await switchToMission(id: missionId)
        }

        do {
            let results = try await api.searchMissionMoments(
                query: "failing tool call error",
                limit: 1,
                missionId: missionId
            )
            guard let best = results.first else {
                print("No failure moment found for mission \(missionId)")
                HapticService.error()
                return
            }

            if let targetId = findMessageIdForEntryIndex(best.entryIndex, snippet: best.snippet) {
                pendingFocusedMessageId = targetId
                HapticService.selectionChanged()
            } else {
                print("Failed to locate failure moment in loaded history for mission \(missionId)")
                HapticService.error()
            }
        } catch {
            print("Failed to open failing tool call: \(error)")
            HapticService.error()
        }
    }
    
    // MARK: - Default Agent Helper
    
    private func getValidatedDefaultAgentOptions() async -> NewMissionOptions? {
        let skipAgentSelection = UserDefaults.standard.bool(forKey: "skip_agent_selection")
        let defaultAgent = UserDefaults.standard.string(forKey: "default_agent")

        guard skipAgentSelection,
              let savedDefault = defaultAgent,
              !savedDefault.isEmpty,
              let parsed = CombinedAgent.parse(savedDefault) else {
            return nil
        }

        BackendAgentService.invalidateCache()
        let data = await BackendAgentService.loadBackendsAndAgents()

        guard let agents = data.backendAgents[parsed.backend],
              agents.contains(where: { $0.id == parsed.agent }) else {
            return nil
        }

        return NewMissionOptions(
            workspaceId: workspaceState.selectedWorkspace?.id,
            agent: parsed.agent,
            modelOverride: nil,
            backend: parsed.backend
        )
    }
    
    // MARK: - Backend Helpers

    private func missionBackendColor(_ mission: Mission) -> Color {
        BackendAgentService.color(for: mission.backend)
    }

    private func missionBackendIcon(_ mission: Mission) -> String {
        BackendAgentService.icon(for: mission.backend)
    }
    
    private func sendMessage() {
        let content = inputText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !content.isEmpty else { return }

        inputText = ""
        UserDefaults.standard.removeObject(forKey: Self.draftTextKey)
        HapticService.lightTap()

        // Generate temp ID and add message optimistically BEFORE the API call
        // This ensures messages appear in send order, not response order
        let tempId = "temp-\(UUID().uuidString)"
        let tempMessage = ChatMessage(id: tempId, type: .user, content: content)
        messages.append(tempMessage)
        recomputeGroupedItems()
        scrollToBottomTick += 1

        Task { @MainActor in
            do {
                let (messageId, queued) = try await api.sendMessage(content: content)

                // Replace temp ID with server-assigned ID, preserving timestamp
                // This allows SSE handler to correctly deduplicate
                if let index = messages.firstIndex(where: { $0.id == tempId }) {
                    let originalTimestamp = messages[index].timestamp
                    messages[index] = ChatMessage(id: messageId, type: .user, content: content, timestamp: originalTimestamp)
                }

                // Update queue count when message was queued
                if queued {
                    queueLength += 1
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
                recomputeGroupedItems()
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

    // MARK: - Queue Management

    private func loadQueueItems() async {
        do {
            queuedItems = try await api.getQueue()
        } catch {
            print("Failed to load queue: \(error)")
        }
    }

    private func removeFromQueue(messageId: String) async {
        // Optimistic update
        queuedItems.removeAll { $0.id == messageId }
        queueLength = max(0, queueLength - 1)

        do {
            try await api.removeFromQueue(messageId: messageId)
        } catch {
            print("Failed to remove from queue: \(error)")
            // Refresh from server on error to get actual state
            await loadQueueItems()
            queueLength = queuedItems.count
            HapticService.error()
        }
    }

    private func clearQueue() async {
        // Optimistic update
        queuedItems = []
        queueLength = 0
        showQueueSheet = false

        do {
            _ = try await api.clearQueue()
            HapticService.success()
        } catch {
            print("Failed to clear queue: \(error)")
            // Refresh from server on error to get actual state
            await loadQueueItems()
            queueLength = queuedItems.count
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

                _ = await withCheckedContinuation { continuation in
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

                                // Just reconnected — catch up on events we missed while disconnected.
                                // Prefer the delta path (since_seq) when the backend supports it
                                // and we have a high-water mark for this mission. Falls back to a
                                // full reload only if the backend doesn't advertise X-Max-Sequence
                                // or we never recorded a cursor for this mission.
                                if wasReconnecting, let viewingId = self.viewingMissionId {
                                    Task {
                                        await self.resumeMissionAfterReconnect(id: viewingId)
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

        // Also refresh child missions if viewing a boss mission
        if let id = viewingMissionId {
            if let workers = try? await api.getChildMissions(parentId: id) {
                guard viewingMissionId == id else { return }
                childMissions = workers
            }
        }
    }

    private func loadRecentMissions() async {
        do {
            let allMissions = try await api.listMissions()
            // Sort by most recent (updatedAt, ISO8601 strings sort correctly)
            recentMissions = allMissions.sorted { $0.updatedAt > $1.updatedAt }
        } catch {
            print("Failed to load recent missions: \(error)")
        }
    }

    private func updateRecentMission(
        id missionId: String,
        _ mutate: (inout Mission) -> Void
    ) {
        guard let index = recentMissions.firstIndex(where: { $0.id == missionId }) else {
            return
        }
        mutate(&recentMissions[index])
        recentMissions.sort { $0.updatedAt > $1.updatedAt }
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

        // Clear stale workers from previous mission immediately
        childMissions = []

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

            // Update current mission if this is the main mission.
            if currentMission?.id == mission.id {
                currentMission = mission
            }

            // Fetch full event history to avoid partial history rendering.
            // Use the meta variant so the `X-Max-Sequence` header seeds the
            // delta-resume cursor — otherwise missions loaded via the switcher
            // would always fall back to a full tail reload on reconnect.
            if let result = try? await api.getMissionEventsWithMeta(id: id, types: historyEventTypes), !result.events.isEmpty {
                guard fetchingMissionId == id else { return }
                applyViewingMissionWithEvents(mission, events: result.events)
                if let maxSeq = result.maxSequence, maxSeq > 0 {
                    missionMaxSeq[id] = maxSeq
                }
                cacheMissionWithEvents(mission, events: result.events)
            } else {
                guard fetchingMissionId == id else { return }
                removeMissionFromCache(mission.id)
                applyViewingMission(mission)
            }

            isLoading = false
            HapticService.selectionChanged()

            // Fetch child (worker) missions in background
            Task {
                if let workers = try? await api.getChildMissions(parentId: id) {
                    guard fetchingMissionId == id else { return }
                    childMissions = workers
                }
            }
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

    private var historyEventTypes: [String] {
        ["user_message", "assistant_message", "tool_call", "tool_result", "text_delta", "thinking"]
    }

    private var streamingThoughtPrefix: String {
        "stream-thinking-"
    }

    private func isStreamingFallbackThought(_ message: ChatMessage) -> Bool {
        message.id.hasPrefix(streamingThoughtPrefix)
    }

    private func finalizeActiveThinkingMessages() {
        for index in messages.indices {
            guard messages[index].isThinking, !messages[index].thinkingDone else {
                continue
            }

            let existing = messages[index]
            let startTime = existing.thinkingStartTime ?? existing.timestamp
            messages[index] = ChatMessage(
                id: existing.id,
                type: .thinking(done: true, startTime: startTime),
                content: existing.content,
                toolUI: existing.toolUI,
                toolData: existing.toolData,
                timestamp: existing.timestamp
            )
        }
    }

    private func upsertStreamingFallbackThought(content: String, done: Bool) {
        let trimmed = content.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }

        messages.removeAll { $0.isPhase }

        if let activeRealThought = messages.last(where: {
            $0.isThinking && !$0.thinkingDone && !isStreamingFallbackThought($0)
        }), !activeRealThought.content.isEmpty {
            return
        }

        if let index = messages.lastIndex(where: {
            $0.isThinking && !$0.thinkingDone && isStreamingFallbackThought($0)
        }) {
            let existing = messages[index]
            let startTime = existing.thinkingStartTime ?? existing.timestamp
            let mergedContent: String
            if content.hasPrefix(existing.content) {
                mergedContent = content
            } else {
                mergedContent = existing.content + content
            }
            messages[index] = ChatMessage(
                id: existing.id,
                type: .thinking(done: done, startTime: startTime),
                content: mergedContent,
                toolUI: existing.toolUI,
                toolData: existing.toolData,
                timestamp: existing.timestamp
            )
        } else {
            // Append a UUID-suffixed id so concurrent fallback thoughts can't collide
            // and crash the Thoughts sheet's `ForEach`. The "stream-thinking-" prefix
            // is preserved because `isStreamingFallbackThought` still checks it.
            messages.append(
                ChatMessage(
                    id: "\(streamingThoughtPrefix)\(UUID().uuidString)",
                    type: .thinking(done: done, startTime: Date()),
                    content: content
                )
            )
        }
    }
    
    private func handleStreamEvent(type: String, data: [String: Any], isHistoricalReplay: Bool = false) {
        // Filter events by mission_id - only show events for the mission we're viewing
        // This prevents cross-mission contamination when parallel missions are running
        let eventMissionId = data["mission_id"] as? String
        let viewingId = viewingMissionId
        let currentId = currentMission?.id

        // Allow status and mission-level metadata events from any mission (for global state).
        // All other events must match the mission we're viewing.
        let isGlobalEvent = type == "status"
            || type == "mission_status_changed"
            || type == "mission_title_changed"
            || type == "mission_metadata_updated"
        if !isGlobalEvent {
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
            } else if let vId = viewingId, let cId = currentId, vId != cId {
                // Event has NO mission_id (from main session)
                // Skip if we're viewing a different (parallel) mission
                // Note: We only skip if BOTH viewingId and currentId are set and different
                // If currentId is nil (not loaded yet), we accept the event
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
                // Status for main session - only apply if viewing the current (main) mission,
                // no specific mission, or currentId hasn't loaded yet (to match event filter
                // logic and avoid desktop stream staying open when status=idle comes during loading)
                shouldApply = viewingId == nil || viewingId == currentId || currentId == nil
            }

            if shouldApply {
                if let state = data["state"] as? String {
                    let newState = ControlRunState(rawValue: state) ?? .idle
                    runState = newState

                    // Clear progress and auto-close desktop stream when idle
                    if newState == .idle {
                        finalizeActiveThinkingMessages()
                        progress = nil
                        // Auto-close desktop stream when agent finishes
                        showDesktopStream = false
                    }
                }
                if let queue = data["queue_len"] as? Int {
                    queueLength = queue
                }
            }
            
        case "user_message":
            if let content = data["content"] as? String,
               let id = data["id"] as? String {
                finalizeActiveThinkingMessages()
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
                // Skip if already present — historical replay or delta resume
                // can re-deliver an assistant_message we already saw via SSE.
                guard !messages.contains(where: { $0.id == id }) else { break }
                let success = data["success"] as? Bool ?? true
                let costObj = data["cost"] as? [String: Any]
                let costCents = data["cost_cents"] as? Int
                    ?? costObj?["amount_cents"] as? Int
                    ?? 0
                let costSource = (data["cost_source"] as? String ?? costObj?["source"] as? String)
                    .flatMap(CostSource.init(rawValue:)) ?? .unknown
                let model = data["model"] as? String

                // Parse shared_files if present
                var sharedFiles: [SharedFile]? = nil
                if let filesArray = data["shared_files"] as? [[String: Any]] {
                    sharedFiles = filesArray.compactMap { fileData -> SharedFile? in
                        guard let name = fileData["name"] as? String,
                              let url = fileData["url"] as? String,
                              let contentType = fileData["content_type"] as? String,
                              let kindString = fileData["kind"] as? String,
                              let kind = SharedFileKind(rawValue: kindString) else {
                            return nil
                        }
                        let sizeBytes = fileData["size_bytes"] as? Int
                        return SharedFile(name: name, url: url, contentType: contentType, sizeBytes: sizeBytes, kind: kind)
                    }
                }

                finalizeActiveThinkingMessages()
                messages.removeAll { $0.isPhase }

                // Mark any remaining active tool calls as completed
                markActiveToolCallsAsCompleted(withState: .success)

                let message = ChatMessage(
                    id: id,
                    type: .assistant(success: success, costCents: costCents, costSource: costSource, model: model, sharedFiles: sharedFiles),
                    content: content
                )
                messages.append(message)
            }

        case "text_delta":
            if !isHistoricalReplay, let content = data["content"] as? String {
                upsertStreamingFallbackThought(content: content, done: false)
            }

        case "goal_iteration":
            // Goal-mode iteration marker — increment the counter shown in
            // the pill above the composer. Backend dedupes by turn id, so
            // we trust the value as authoritative.
            let iteration = data["iteration"] as? Int ?? 0
            let objective = data["objective"] as? String ?? goalInfo?.objective ?? ""
            goalInfo = GoalPillInfo(
                iteration: iteration,
                status: goalInfo?.status ?? "active",
                objective: objective
            )

        case "goal_status":
            // Goal status transitioned. Terminal statuses clear the pill;
            // active/paused keep it visible with the new label.
            let status = data["status"] as? String ?? ""
            let objective = data["objective"] as? String ?? goalInfo?.objective ?? ""
            switch status {
            case "complete", "cleared", "budgetLimited":
                goalInfo = nil
            default:
                goalInfo = GoalPillInfo(
                    iteration: goalInfo?.iteration ?? 0,
                    status: status,
                    objective: objective
                )
            }
            
        case "thinking":
            let content = data["content"] as? String ?? ""
            let done = data["done"] as? Bool ?? false
            let trimmed = content.trimmingCharacters(in: .whitespacesAndNewlines)

            if trimmed.isEmpty {
                if done {
                    finalizeActiveThinkingMessages()
                }
                break
            }

            // Skip if we've already seen this server-supplied event id —
            // delta resume can re-deliver completed thinking events we already
            // appended, and the active-message fast path won't catch them
            // because the existing one is already `done: true`. Run this
            // *before* stripping phase messages, otherwise a duplicate-event
            // break would silently clear a still-relevant `agent_phase`
            // indicator without adding any new content.
            let eventId = data["id"] as? String
            if let eventId, messages.contains(where: { $0.id == eventId }) {
                break
            }

            // Remove phase items now that we know we're committing this event.
            messages.removeAll { $0.isPhase }

            // Find existing active thinking message or create new
            if let index = messages.lastIndex(where: { $0.isThinking && !$0.thinkingDone }) {
                let existing = messages[index]
                let existingStartTime = existing.thinkingStartTime ?? existing.timestamp
                messages[index] = ChatMessage(
                    id: existing.id,
                    type: .thinking(done: done, startTime: existingStartTime),
                    content: content,
                    toolUI: existing.toolUI,
                    toolData: existing.toolData,
                    timestamp: existing.timestamp
                )
            } else {
                // Create new thinking message - whether done or not.
                // Handles the case where we receive a completed thought without seeing it
                // active first (joining mid-thought or reconnecting).
                //
                // Prefer the server-supplied event id when available; otherwise fall back
                // to a UUID. A wall-clock-second id can collide during history replay
                // (many thinking events landing in the same instant), which then crashes
                // the Thoughts sheet's `ForEach` with a duplicate-id assertion.
                let messageId = eventId ?? "thinking-\(UUID().uuidString)"
                let message = ChatMessage(
                    id: messageId,
                    type: .thinking(done: done, startTime: Date()),
                    content: content
                )
                messages.append(message)
            }

        case "agent_phase":
            let phase = data["phase"] as? String ?? ""
            let detail = data["detail"] as? String
            let agent = data["agent"] as? String

            // Remove existing phase messages
            messages.removeAll { $0.isPhase }

            // Add new phase message. UUID-suffixed id so back-to-back phase events in the
            // same instant cannot collide and crash a `ForEach`.
            let message = ChatMessage(
                id: "phase-\(UUID().uuidString)",
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
                finalizeActiveThinkingMessages()
                // Filter out SSE-specific reconnection errors - these are handled by the reconnection logic
                // Use specific patterns to avoid filtering legitimate agent errors
                let lower = errorMessage.lowercased()
                let isSseReconnectError = lower.contains("stream connection failed") ||
                                          lower.contains("sse connection") ||
                                          lower.contains("event stream") ||
                                          lower.contains("stream idle") ||
                                          lower.contains("stream rejected by server") ||
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
                // Skip if already present — historical replay or delta resume
                // can re-deliver a tool_call we already saw via SSE. The
                // per-row id depends on the path (toolUI vs toolCall), so
                // check both forms.
                if messages.contains(where: { $0.id == toolCallId || $0.id == "tool-\(toolCallId)" }) {
                    break
                }
                finalizeActiveThinkingMessages()
                // Parse UI tool calls
                if let toolUI = ToolUIContent.parse(name: name, args: args) {
                    let message = ChatMessage(
                        id: toolCallId,
                        type: .toolUI(name: name),
                        content: "",
                        toolUI: toolUI
                    )
                    messages.append(message)
                } else {
                    // Mark any previous active tool calls as completed (success by default, will update if error)
                    markActiveToolCallsAsCompleted(withState: .success)

                    // Create tool call data for tracking
                    let toolData = ToolCallData(
                        toolCallId: toolCallId,
                        name: name,
                        args: args,
                        startTime: Date(),
                        endTime: nil,
                        result: nil,
                        state: .running
                    )

                    let message = ChatMessage(
                        id: "tool-\(toolCallId)",
                        type: .toolCall(name: name, isActive: true),
                        content: "",
                        toolData: toolData
                    )
                    messages.append(message)
                }
            }

        case "tool_result":
            let result = data["result"]
            let name = data["name"] as? String ?? ""

            // Update the matching tool call message if we have a tool_call_id
            if let toolCallId = data["tool_call_id"] as? String {
                // Find the matching tool call message and update it
                if let index = messages.firstIndex(where: { $0.id == "tool-\(toolCallId)" }) {
                    if var toolData = messages[index].toolData {
                        toolData.endTime = Date()
                        toolData.result = result

                        // Determine state based on result
                        if let resultDict = result as? [String: Any] {
                            if resultDict["status"] as? String == "cancelled" {
                                toolData.state = .cancelled
                            } else if toolData.isErrorResult {
                                toolData.state = .error
                            } else {
                                toolData.state = .success
                            }
                        } else if toolData.isErrorResult {
                            toolData.state = .error
                        } else {
                            toolData.state = .success
                        }

                        // Update the type to mark as not active
                        messages[index] = ChatMessage(
                            id: messages[index].id,
                            type: .toolCall(name: toolData.name, isActive: false),
                            content: messages[index].content,
                            toolData: toolData,
                            timestamp: messages[index].timestamp
                        )
                    }
                }
            }

            // Extract display ID from desktop_start_session tool result (doesn't require tool_call_id)
            if name == "desktop_start_session" || name == "desktop_desktop_start_session" ||
               name.contains("desktop_start_session") {
                // Handle result as either a dictionary or a JSON string
                var resultDict: [String: Any]? = result as? [String: Any]
                if resultDict == nil, let resultString = result as? String,
                   let jsonData = resultString.data(using: .utf8),
                   let parsed = try? JSONSerialization.jsonObject(with: jsonData) as? [String: Any] {
                    resultDict = parsed
                }
                if let display = resultDict?["display"] as? String {
                    desktopDisplayId = display
                    // Auto-open desktop stream when session starts (live only)
                    if !isHistoricalReplay {
                        showDesktopStream = true
                    }
                }
            }

        case "mission_status_changed":
            // Handle mission status changes (e.g., completed, failed, interrupted)
            if let statusStr = data["status"] as? String,
               let missionId = data["mission_id"] as? String {
                let newStatus = MissionStatus(rawValue: statusStr) ?? .unknown

                // If mission is no longer active AND it's the currently viewed mission,
                // mark all pending tools as cancelled
                if newStatus != .active && viewingMissionId == missionId {
                    finalizeActiveThinkingMessages()
                    markActiveToolCallsAsCompleted(withState: .cancelled)
                }

                // Update the viewing mission status if it matches
                if viewingMissionId == missionId {
                    viewingMission?.status = newStatus
                }

                // Update the current mission status if it matches
                if currentMission?.id == missionId {
                    currentMission?.status = newStatus
                }

                updateRecentMission(id: missionId) { mission in
                    mission.status = newStatus
                }

                // Refresh running missions list (live only)
                if !isHistoricalReplay {
                    Task { await refreshRunningMissions() }
                }
            }

        case "mission_title_changed":
            // Handle title updates (e.g., from LLM auto-title generation)
            if let missionId = data["mission_id"] as? String,
               let title = data["title"] as? String {
                // Update the viewing mission title if it matches
                if viewingMissionId == missionId {
                    viewingMission?.title = title
                }

                // Update the current mission title if it matches
                if currentMission?.id == missionId {
                    currentMission?.title = title
                }

                updateRecentMission(id: missionId) { mission in
                    mission.title = title
                }

                // Refresh running missions list so the bar picks up the new title
                if !isHistoricalReplay {
                    Task { await refreshRunningMissions() }
                }
            }

        case "mission_metadata_updated":
            if let missionId = data["mission_id"] as? String {
                let hasTitle = data.keys.contains("title")
                let hasShortDescription = data.keys.contains("short_description")
                let hasMetadataUpdatedAt = data.keys.contains("metadata_updated_at")
                let hasUpdatedAt = data.keys.contains("updated_at")
                let hasMetadataSource = data.keys.contains("metadata_source")
                let hasMetadataModel = data.keys.contains("metadata_model")
                let hasMetadataVersion = data.keys.contains("metadata_version")
                let title = data["title"] as? String
                let shortDescription = data["short_description"] as? String
                let metadataUpdatedAt = data["metadata_updated_at"] as? String
                let updatedAt = data["updated_at"] as? String
                let metadataSource = data["metadata_source"] as? String
                let metadataModel = data["metadata_model"] as? String
                let metadataVersion = data["metadata_version"] as? String

                if viewingMissionId == missionId {
                    if hasTitle { viewingMission?.title = title }
                    if hasShortDescription { viewingMission?.shortDescription = shortDescription }
                    if hasMetadataUpdatedAt { viewingMission?.metadataUpdatedAt = metadataUpdatedAt }
                    if hasUpdatedAt, let updatedAt { viewingMission?.updatedAt = updatedAt }
                    if hasMetadataSource { viewingMission?.metadataSource = metadataSource }
                    if hasMetadataModel { viewingMission?.metadataModel = metadataModel }
                    if hasMetadataVersion { viewingMission?.metadataVersion = metadataVersion }
                }

                if currentMission?.id == missionId {
                    if hasTitle { currentMission?.title = title }
                    if hasShortDescription { currentMission?.shortDescription = shortDescription }
                    if hasMetadataUpdatedAt { currentMission?.metadataUpdatedAt = metadataUpdatedAt }
                    if hasUpdatedAt, let updatedAt { currentMission?.updatedAt = updatedAt }
                    if hasMetadataSource { currentMission?.metadataSource = metadataSource }
                    if hasMetadataModel { currentMission?.metadataModel = metadataModel }
                    if hasMetadataVersion { currentMission?.metadataVersion = metadataVersion }
                }

                updateRecentMission(id: missionId) { mission in
                    if hasTitle { mission.title = title }
                    if hasShortDescription { mission.shortDescription = shortDescription }
                    if hasMetadataUpdatedAt { mission.metadataUpdatedAt = metadataUpdatedAt }
                    if hasUpdatedAt, let updatedAt { mission.updatedAt = updatedAt }
                    if hasMetadataSource { mission.metadataSource = metadataSource }
                    if hasMetadataModel { mission.metadataModel = metadataModel }
                    if hasMetadataVersion { mission.metadataVersion = metadataVersion }
                }

                if !isHistoricalReplay {
                    Task { await refreshRunningMissions() }
                }
            }

        case "fido_sign_request":
            FidoApprovalState.shared.handleSignRequest(data)

        default:
            break
        }

        // Recompute grouped items for live events (history replay calls recomputeGroupedItems() once at the end)
        if !isHistoricalReplay {
            recomputeGroupedItems()
        }
    }

    /// Marks all active tool calls as completed with the given state.
    /// - Parameter state: The final state to set for active tool calls (e.g., .success, .cancelled)
    private func markActiveToolCallsAsCompleted(withState state: ToolCallState) {
        for i in messages.indices {
            if messages[i].isToolCall && messages[i].isActiveToolCall {
                if var toolData = messages[i].toolData {
                    toolData.endTime = Date()
                    if toolData.result == nil || state == .cancelled {
                        toolData.state = state
                    }
                    messages[i].toolData = toolData
                }
                if let name = messages[i].toolCallName {
                    messages[i] = ChatMessage(
                        id: messages[i].id,
                        type: .toolCall(name: name, isActive: false),
                        content: messages[i].content,
                        toolData: messages[i].toolData,
                        timestamp: messages[i].timestamp
                    )
                }
            }
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
            } else if message.isToolCall {
                ToolCallBubble(message: message)
                Spacer(minLength: 60)
            } else if message.isToolUI {
                toolUIBubble
                Spacer(minLength: 40)
            } else {
                // Assistant messages now use full width
                assistantBubble
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
                    .contextMenu {
                        Button {
                            onCopy?()
                        } label: {
                            Label("Copy", systemImage: "doc.on.doc")
                        }
                    }

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
                if case .assistant(let success, _, _, _, _) = message.type {
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
                            // Cost + source as one calm chip: "$4.22 actual" — the
                            // ALL-CAPS pill version of "ACTUAL" was visually shouting
                            // louder than the cost itself.
                            HStack(spacing: 4) {
                                Text(cost)
                                    .font(.caption2.monospaced())
                                    .foregroundStyle(message.costIsEstimated ? Theme.textSecondary : Theme.success)
                                if let badge = message.costSourceLabel {
                                    Text(badge.lowercased())
                                        .font(.caption2)
                                        .foregroundStyle(Theme.textMuted)
                                }
                            }
                        }

                        Text("•")
                            .foregroundStyle(Theme.textMuted)
                        Text(message.timestamp, style: .time)
                            .font(.caption2)
                            .foregroundStyle(Theme.textMuted)
                    }
                }

                MarkdownView(message.content)
                    .frame(maxWidth: .infinity, alignment: .leading)
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
                    .contextMenu {
                        Button {
                            onCopy?()
                        } label: {
                            Label("Copy", systemImage: "doc.on.doc")
                        }
                    }

                // Render shared files
                if let files = message.sharedFiles, !files.isEmpty {
                    VStack(alignment: .leading, spacing: 8) {
                        ForEach(files) { file in
                            SharedFileCardView(file: file)
                        }
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)

            // Copy button
            if !message.content.isEmpty {
                CopyButton(isCopied: isCopied, onCopy: onCopy)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

// MARK: - Shared File Card View

private struct SharedFileCardView: View {
    let file: SharedFile
    @Environment(\.openURL) private var openURL
    @State private var imageData: Data?
    @State private var isLoadingImage = false
    @State private var imageLoadFailed = false

    private var fullURL: URL? {
        // If URL is relative, prepend the base URL
        if file.url.hasPrefix("/") {
            let baseURL = APIService.shared.baseURL
            return URL(string: baseURL + file.url)
        }
        return URL(string: file.url)
    }

    var body: some View {
        if file.isImage {
            imageCard
        } else {
            downloadCard
        }
    }

    private var imageCard: some View {
        VStack(alignment: .leading, spacing: 0) {
            // Image preview with authentication support
            Group {
                if isLoadingImage {
                    ProgressView()
                        .frame(maxWidth: .infinity, minHeight: 120)
                        .background(Theme.backgroundSecondary)
                } else if let data = imageData, let uiImage = UIImage(data: data) {
                    Image(uiImage: uiImage)
                        .resizable()
                        .aspectRatio(contentMode: .fit)
                        .frame(maxWidth: .infinity, maxHeight: 300)
                } else if imageLoadFailed {
                    Image(systemName: "photo")
                        .font(.title)
                        .foregroundStyle(Theme.textMuted)
                        .frame(maxWidth: .infinity, minHeight: 80)
                        .background(Theme.backgroundSecondary)
                } else {
                    ProgressView()
                        .frame(maxWidth: .infinity, minHeight: 120)
                        .background(Theme.backgroundSecondary)
                }
            }
            .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
            .task {
                await loadImage()
            }

            // File info bar
            HStack(spacing: 6) {
                Image(systemName: file.kind.iconName)
                    .font(.caption2)
                    .foregroundStyle(Theme.textMuted)

                Text(file.name)
                    .font(.caption2)
                    .foregroundStyle(Theme.textSecondary)
                    .lineLimit(1)

                Spacer()

                if let size = file.formattedSize {
                    Text(size)
                        .font(.caption2)
                        .foregroundStyle(Theme.textMuted)
                }

                Button {
                    if let url = fullURL {
                        openURL(url)
                    }
                } label: {
                    Image(systemName: "arrow.up.right.square")
                        .font(.caption2)
                        .foregroundStyle(Theme.accent)
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(Theme.backgroundSecondary)
        }
        .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .stroke(Theme.border, lineWidth: 0.5)
        )
    }

    private var downloadCard: some View {
        Button {
            if let url = fullURL {
                openURL(url)
            }
        } label: {
            HStack(spacing: 12) {
                // File type icon
                Image(systemName: file.kind.iconName)
                    .font(.title3)
                    .foregroundStyle(Theme.accent)
                    .frame(width: 40, height: 40)
                    .background(Theme.accent.opacity(0.1))
                    .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))

                // File info
                VStack(alignment: .leading, spacing: 2) {
                    Text(file.name)
                        .font(.subheadline.weight(.medium))
                        .foregroundStyle(Theme.textPrimary)
                        .lineLimit(1)

                    HStack(spacing: 4) {
                        Text(file.contentType)
                            .font(.caption2)
                            .foregroundStyle(Theme.textMuted)
                            .lineLimit(1)

                        if let size = file.formattedSize {
                            Text("•")
                                .foregroundStyle(Theme.textMuted)
                            Text(size)
                                .font(.caption2)
                                .foregroundStyle(Theme.textMuted)
                        }
                    }
                }

                Spacer()

                // Download indicator
                Image(systemName: "arrow.down.circle")
                    .font(.title3)
                    .foregroundStyle(Theme.textMuted)
            }
            .padding(12)
            .background(.ultraThinMaterial)
            .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .stroke(Theme.border, lineWidth: 0.5)
            )
        }
        .buttonStyle(.plain)
    }

    private func loadImage() async {
        guard let url = fullURL, !isLoadingImage else {
            // If URL is nil (malformed), mark as failed to prevent infinite loading
            if fullURL == nil {
                await MainActor.run {
                    self.imageLoadFailed = true
                    self.isLoadingImage = false
                }
            }
            return
        }

        isLoadingImage = true
        imageLoadFailed = false

        do {
            var request = URLRequest(url: url)
            // Bound the per-image fetch to the same window as JSON requests so
            // a stalled image host can't leave the cell spinning behind the
            // 60s URLSession default.
            request.timeoutInterval = APIService.requestTimeout

            // Add authentication token if available
            if let token = APIService.shared.authToken {
                request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
            }

            let (data, response) = try await URLSession.shared.data(for: request)

            // Check response status
            if let httpResponse = response as? HTTPURLResponse {
                if httpResponse.statusCode == 200 {
                    // Validate that the data is actually parseable as an image
                    if UIImage(data: data) != nil {
                        await MainActor.run {
                            self.imageData = data
                        }
                    } else {
                        // Data is not a valid image
                        await MainActor.run {
                            self.imageLoadFailed = true
                        }
                    }
                } else {
                    await MainActor.run {
                        self.imageLoadFailed = true
                    }
                }
            } else {
                // Non-HTTP response (or failed cast) shouldn't leave the spinner running
                await MainActor.run {
                    self.imageLoadFailed = true
                }
            }
        } catch {
            print("Failed to load image: \(error)")
            await MainActor.run {
                self.imageLoadFailed = true
            }
        }

        await MainActor.run {
            isLoadingImage = false
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
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(isCopied ? Theme.success : Theme.textSecondary)
                .frame(width: 28, height: 28)
                .background(Theme.backgroundSecondary)
                .clipShape(Circle())
                .overlay(
                    Circle().stroke(Theme.border, lineWidth: 0.5)
                )
        }
        .accessibilityLabel(isCopied ? "Copied" : "Copy message")
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
                                .lineLimit(1)
                                .truncationMode(.tail)
                                .padding(.horizontal, 6)
                                .padding(.vertical, 2)
                                .background(Theme.backgroundTertiary)
                                .clipShape(RoundedRectangle(cornerRadius: 4, style: .continuous))
                        }

                        Text("•")
                            .foregroundStyle(Theme.textMuted)
                            .font(.caption2)
                        Text(message.timestamp, style: .time)
                            .font(.caption2)
                            .foregroundStyle(Theme.textMuted)
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

// MARK: - Tool Call Bubble (Enhanced)

struct ToolCallBubble: View {
    let message: ChatMessage
    @State private var isExpanded = false
    @State private var elapsedSeconds: Int = 0
    @State private var timerTask: Task<Void, Never>?

    private var toolData: ToolCallData? {
        message.toolData
    }

    private var isRunning: Bool {
        toolData?.state == .running
    }

    private var stateColor: Color {
        guard let state = toolData?.state else {
            return message.isActiveToolCall ? Theme.warning : Theme.textMuted
        }
        switch state {
        case .running: return Theme.warning
        case .success: return Theme.success
        case .error: return Theme.error
        case .cancelled: return Theme.warning
        }
    }

    private var stateIcon: String {
        guard let state = toolData?.state else {
            return message.isActiveToolCall ? "circle.fill" : "checkmark.circle.fill"
        }
        switch state {
        case .running: return "circle.fill"
        case .success: return "checkmark.circle.fill"
        case .error: return "xmark.circle.fill"
        case .cancelled: return "xmark.circle.fill"
        }
    }

    var body: some View {
        if let name = message.toolCallName {
            VStack(alignment: .leading, spacing: 0) {
                // Compact header button
                Button {
                    withAnimation(.spring(duration: 0.25)) {
                        isExpanded.toggle()
                    }
                    HapticService.selectionChanged()
                } label: {
                    HStack(spacing: 6) {
                        // Tool icon
                        Image(systemName: toolIcon(for: name))
                            .font(.system(size: 11, weight: .medium))
                            .foregroundStyle(stateColor)
                            .frame(width: 18, height: 18)
                            .background(stateColor.opacity(0.15))
                            .clipShape(RoundedRectangle(cornerRadius: 4, style: .continuous))

                        // Tool name
                        Text(name)
                            .font(.caption.monospaced())
                            .foregroundStyle(Theme.accent)
                            .lineLimit(1)

                        // Args preview
                        if let preview = toolData?.argsPreview, !preview.isEmpty {
                            Text("(\(preview))")
                                .font(.caption2)
                                .foregroundStyle(Theme.textMuted)
                                .lineLimit(1)
                                .truncationMode(.tail)
                        }

                        Spacer()

                        // Duration
                        if let data = toolData {
                            Text(isRunning ? "\(formattedElapsed)..." : data.durationFormatted)
                                .font(.caption2.monospacedDigit())
                                .foregroundStyle(Theme.textMuted)
                        }

                        // State indicator
                        if isRunning {
                            ProgressView()
                                .progressViewStyle(.circular)
                                .scaleEffect(0.5)
                                .tint(stateColor)
                        } else {
                            Image(systemName: stateIcon)
                                .font(.system(size: 12))
                                .foregroundStyle(stateColor)
                        }

                        // Chevron
                        Image(systemName: "chevron.right")
                            .font(.system(size: 9, weight: .medium))
                            .foregroundStyle(Theme.textMuted)
                            .rotationEffect(.degrees(isExpanded ? 90 : 0))
                    }
                    .padding(.horizontal, 10)
                    .padding(.vertical, 6)
                    .background(stateColor.opacity(0.05))
                    .clipShape(Capsule())
                    .overlay(
                        Capsule()
                            .stroke(stateColor.opacity(0.2), lineWidth: 1)
                    )
                }
                .buttonStyle(.plain)

                // Expandable content
                if isExpanded {
                    VStack(alignment: .leading, spacing: 10) {
                        // Arguments section
                        if let data = toolData, !data.args.isEmpty {
                            VStack(alignment: .leading, spacing: 4) {
                                Text("Arguments")
                                    .font(.caption2)
                                    .fontWeight(.medium)
                                    .foregroundStyle(Theme.textMuted)
                                    .textCase(.uppercase)

                                ScrollView(.horizontal, showsIndicators: false) {
                                    Text(data.argsString)
                                        .font(.caption.monospaced())
                                        .foregroundStyle(Theme.textSecondary)
                                        .padding(8)
                                        .background(Theme.backgroundTertiary.opacity(0.5))
                                        .clipShape(RoundedRectangle(cornerRadius: 6, style: .continuous))
                                }
                                .frame(maxHeight: 120)
                            }
                        }

                        // Result section
                        if let data = toolData, let resultStr = data.resultString {
                            VStack(alignment: .leading, spacing: 4) {
                                Text(data.isErrorResult ? "Error" : "Result")
                                    .font(.caption2)
                                    .fontWeight(.medium)
                                    .foregroundStyle(data.isErrorResult ? Theme.error : Theme.success)
                                    .textCase(.uppercase)

                                ScrollView(.horizontal, showsIndicators: false) {
                                    Text(resultStr)
                                        .font(.caption.monospaced())
                                        .foregroundStyle(data.isErrorResult ? Theme.error : Theme.textSecondary)
                                        .padding(8)
                                        .background((data.isErrorResult ? Theme.error : Theme.backgroundTertiary).opacity(0.1))
                                        .clipShape(RoundedRectangle(cornerRadius: 6, style: .continuous))
                                }
                                .frame(maxHeight: 120)
                            }
                        }

                        // Still running indicator
                        if isRunning {
                            HStack(spacing: 6) {
                                ProgressView()
                                    .progressViewStyle(.circular)
                                    .scaleEffect(0.5)
                                    .tint(Theme.warning)
                                Text("Running for \(formattedElapsed)...")
                                    .font(.caption2)
                                    .foregroundStyle(Theme.warning)
                            }
                        }
                    }
                    .padding(.top, 8)
                    .padding(.horizontal, 4)
                    .transition(.opacity.combined(with: .move(edge: .top)))
                }
            }
            .animation(.spring(duration: 0.25), value: isExpanded)
            .onAppear {
                if isRunning {
                    startTimer()
                }
            }
            .onDisappear {
                timerTask?.cancel()
            }
            .onChange(of: isRunning) { _, running in
                if running {
                    startTimer()
                } else {
                    timerTask?.cancel()
                }
            }
        }
    }

    private var formattedElapsed: String {
        formatDurationString(elapsedSeconds)
    }

    private func startTimer() {
        timerTask?.cancel()
        elapsedSeconds = Int(toolData?.duration ?? 0)
        timerTask = Task { @MainActor in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(1))
                if !Task.isCancelled {
                    elapsedSeconds = Int(toolData?.duration ?? 0)
                }
            }
        }
    }

    private func toolIcon(for name: String) -> String {
        let lower = name.lowercased()
        if lower.contains("bash") || lower.contains("shell") || lower.contains("terminal") || lower.contains("exec") {
            return "terminal"
        } else if lower.contains("read") || lower.contains("file") || lower.contains("write") {
            return "doc.text"
        } else if lower.contains("search") || lower.contains("grep") || lower.contains("find") || lower.contains("glob") {
            return "magnifyingglass"
        } else if lower.contains("browser") || lower.contains("web") || lower.contains("http") || lower.contains("fetch") {
            return "globe"
        } else if lower.contains("edit") || lower.contains("patch") || lower.contains("notebook") {
            return "chevron.left.forwardslash.chevron.right"
        } else if lower.contains("task") || lower.contains("agent") || lower.contains("subagent") {
            return "person.2"
        } else if lower.contains("desktop") || lower.contains("screenshot") {
            return "display"
        } else if lower.contains("todo") {
            return "checklist"
        } else {
            return "wrench"
        }
    }
}

// MARK: - Thinking Bubble

private struct ThinkingBubble: View {
    let message: ChatMessage
    @State private var isExpanded: Bool = true
    @State private var elapsedSeconds: Int = 0
    @State private var hasAutoCollapsed = false
    @State private var timerTask: Task<Void, Never>?
    
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

                    Text("•")
                        .foregroundStyle(Theme.textMuted)
                        .font(.caption2)
                    Text(message.timestamp, style: .time)
                        .font(.caption2)
                        .foregroundStyle(Theme.textMuted)

                    Spacer()

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
                    // Inline a blinking caret while streaming so the user can
                    // distinguish in-flight tokens from a settled thought.
                    // Without this, a paused stream looks identical to a
                    // completed one.
                    (Text(message.content) + (message.thinkingDone ? Text("") : Text(" ▍").foregroundColor(Theme.accent)))
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
        .onDisappear {
            timerTask?.cancel()
            timerTask = nil
        }
        .onChange(of: message.thinkingDone) { _, done in
            if done {
                timerTask?.cancel()
                timerTask = nil

                if let startTime = message.thinkingStartTime {
                    elapsedSeconds = Int(Date().timeIntervalSince(startTime))
                }
            }

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
        formatDurationString(elapsedSeconds)
    }
    
    private func startTimer() {
        timerTask?.cancel()
        timerTask = nil

        guard !message.thinkingDone else {
            // Calculate elapsed from start time
            if let startTime = message.thinkingStartTime {
                elapsedSeconds = Int(Date().timeIntervalSince(startTime))
            }
            return
        }

        // Update every second while thinking
        timerTask = Task { @MainActor in
            while !Task.isCancelled {
                if let startTime = message.thinkingStartTime {
                    elapsedSeconds = Int(Date().timeIntervalSince(startTime))
                } else {
                    elapsedSeconds += 1
                }
                try? await Task.sleep(nanoseconds: 1_000_000_000)
            }
        }
    }
}


// MARK: - Thoughts Sheet

private struct ThoughtsSheet: View {
    let messages: [ChatMessage]
    @Environment(\.dismiss) private var dismiss

    /// All thinking messages
    private var thinkingMessages: [ChatMessage] {
        messages.filter { $0.isThinking }
    }

    /// Active (in-progress) thoughts
    private var activeThoughts: [ChatMessage] {
        thinkingMessages.filter { !$0.thinkingDone }
    }

    /// Completed thoughts, deduplicated by content
    private var completedThoughts: [ChatMessage] {
        var seen = Set<String>()
        return thinkingMessages.filter { msg in
            guard msg.thinkingDone else { return false }
            let trimmed = msg.content.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !trimmed.isEmpty else { return false }
            guard !seen.contains(trimmed) else { return false }
            seen.insert(trimmed)
            return true
        }
    }

    private var hasActiveThinking: Bool {
        !activeThoughts.isEmpty
    }

    /// Count aligned with what is actually rendered in the sheet.
    private var visibleThoughtCount: Int {
        activeThoughts.count + completedThoughts.count
    }

    private var hasVisibleThoughts: Bool {
        visibleThoughtCount > 0
    }

    var body: some View {
        NavigationStack {
            Group {
                if !hasVisibleThoughts {
                    ContentUnavailableView(
                        "No Thoughts Yet",
                        systemImage: "brain",
                        description: Text("Agent thoughts will appear here during execution.")
                    )
                } else {
                    ScrollView {
                        LazyVStack(spacing: 14) {
                            if !activeThoughts.isEmpty {
                                ThoughtSection(title: "Thinking Now", icon: "brain") {
                                    ForEach(activeThoughts) { msg in
                                        ThoughtTimelineRow(message: msg, emphasize: true)
                                    }
                                }
                            }

                            if !completedThoughts.isEmpty {
                                ThoughtSection(title: "Recent Thoughts", icon: "clock.arrow.circlepath") {
                                    ForEach(completedThoughts.reversed()) { msg in
                                        ThoughtTimelineRow(message: msg, emphasize: false)
                                    }
                                }
                            }
                        }
                        .padding()
                    }
                }
            }
            .navigationTitle(hasActiveThinking ? "Thinking" : "Thoughts")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    HStack(spacing: 4) {
                        if hasActiveThinking {
                            Image(systemName: "brain")
                                .font(.caption)
                                .foregroundStyle(Theme.accent)
                                .symbolEffect(.pulse, options: .repeating)
                        }
                        Text("\(visibleThoughtCount)")
                            .font(.subheadline.monospacedDigit())
                            .foregroundStyle(Theme.textMuted)
                    }
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }
                }
            }
        }
    }
}

private struct ThoughtSection<Content: View>: View {
    let title: String
    let icon: String
    @ViewBuilder let content: () -> Content

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 6) {
                Image(systemName: icon)
                    .font(.caption)
                    .foregroundStyle(Theme.textSecondary)
                Text(title)
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(Theme.textSecondary)
            }

            LazyVStack(spacing: 10) {
                content()
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct ThoughtTimelineRow: View {
    let message: ChatMessage
    let emphasize: Bool
    @State private var isExpanded: Bool
    @State private var elapsedSeconds: Int = 0
    @State private var timerTask: Task<Void, Never>?

    init(message: ChatMessage, emphasize: Bool) {
        self.message = message
        self.emphasize = emphasize
        _isExpanded = State(initialValue: emphasize)
    }

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            VStack(spacing: 0) {
                Circle()
                    .fill(emphasize ? Theme.accent : Theme.textMuted)
                    .frame(width: 8, height: 8)
                Rectangle()
                    .fill(Theme.border)
                    .frame(width: 1)
            }

            VStack(alignment: .leading, spacing: 6) {
                Button {
                    withAnimation(.spring(duration: 0.2)) {
                        isExpanded.toggle()
                    }
                } label: {
                    HStack(spacing: 6) {
                        Image(systemName: "brain")
                            .font(.caption)
                            .foregroundStyle(message.thinkingDone ? Theme.textMuted : Theme.accent)
                            .symbolEffect(.pulse, options: message.thinkingDone ? .nonRepeating : .repeating)

                        Text(message.thinkingDone ? "Thought for \(formatDurationString(elapsedSeconds))" : "Thinking for \(formatDurationString(elapsedSeconds))")
                            .font(.caption.weight(.semibold))
                            .foregroundStyle(Theme.textSecondary)

                        Spacer()

                        Image(systemName: "chevron.right")
                            .font(.system(size: 10, weight: .medium))
                            .foregroundStyle(Theme.textMuted)
                            .rotationEffect(.degrees(isExpanded ? 90 : 0))
                    }
                }

                if isExpanded && !message.content.isEmpty {
                    Text(message.content)
                        .font(.caption)
                        .foregroundStyle(Theme.textSecondary)
                        .textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .padding(10)
            .background(Theme.backgroundSecondary.opacity(emphasize ? 1 : 0.8))
            .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
        }
        .onAppear {
            startTimer()
        }
        .onDisappear {
            timerTask?.cancel()
            timerTask = nil
        }
        .onChange(of: message.thinkingDone) { _, done in
            if done {
                timerTask?.cancel()
                timerTask = nil
                if let startTime = message.thinkingStartTime {
                    elapsedSeconds = Int(Date().timeIntervalSince(startTime))
                }
            }
        }
    }

    private func startTimer() {
        timerTask?.cancel()
        timerTask = nil

        guard !message.thinkingDone else {
            if let startTime = message.thinkingStartTime {
                elapsedSeconds = Int(Date().timeIntervalSince(startTime))
            }
            return
        }

        timerTask = Task { @MainActor in
            while !Task.isCancelled {
                if let startTime = message.thinkingStartTime {
                    elapsedSeconds = Int(Date().timeIntervalSince(startTime))
                }
                try? await Task.sleep(nanoseconds: 1_000_000_000)
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

// MARK: - Grouped Chat Item

/// Represents either a single message or a group of consecutive tool calls
enum GroupedChatItem: Identifiable {
    case single(ChatMessage)
    case toolGroup(groupId: String, tools: [ChatMessage])

    var id: String {
        switch self {
        case .single(let message):
            return message.id
        case .toolGroup(let groupId, _):
            return "group-\(groupId)"
        }
    }
}

// MARK: - Tool Group View

/// Displays a group of tool calls with expand/collapse functionality
private struct ToolGroupView: View {
    let groupId: String
    let tools: [ChatMessage]
    @Binding var expandedGroups: Set<String>

    private var isExpanded: Bool {
        expandedGroups.contains(groupId)
    }

    private var hiddenCount: Int {
        tools.count - 1
    }

    private var lastTool: ChatMessage? {
        tools.last
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            // Expand/collapse button
            if hiddenCount > 0 {
                Button {
                    withAnimation(.spring(duration: 0.25)) {
                        if isExpanded {
                            expandedGroups.remove(groupId)
                        } else {
                            expandedGroups.insert(groupId)
                        }
                    }
                    HapticService.selectionChanged()
                } label: {
                    HStack(spacing: 6) {
                        Image(systemName: isExpanded ? "chevron.up" : "chevron.down")
                            .font(.system(size: 10, weight: .medium))
                            .foregroundStyle(Theme.textMuted)

                        Text(isExpanded ? "Hide \(hiddenCount) previous tool\(hiddenCount > 1 ? "s" : "")" : "Show \(hiddenCount) previous tool\(hiddenCount > 1 ? "s" : "")")
                            .font(.caption2)
                            .foregroundStyle(Theme.textMuted)
                    }
                    .padding(.horizontal, 10)
                    .padding(.vertical, 6)
                    .background(Theme.backgroundSecondary.opacity(0.5))
                    .clipShape(Capsule())
                }
                .buttonStyle(.plain)
            }

            // Show all tools if expanded, otherwise just the last one
            if isExpanded {
                ForEach(tools) { tool in
                    ToolCallBubble(message: tool)
                }
            } else if let last = lastTool {
                ToolCallBubble(message: last)
            }
        }
    }
}

// MARK: - Mission Switcher Sheet

private enum MissionQuickAction: Hashable {
    case resume
    case `continue`
    case retry
    case openFailure
    case followUp

    var label: String {
        switch self {
        case .resume: return "Resume"
        case .continue: return "Continue"
        case .retry: return "Retry"
        case .openFailure: return "Open Failure"
        case .followUp: return "Follow-up"
        }
    }

    var icon: String {
        switch self {
        case .resume, .continue: return "play.circle.fill"
        case .retry: return "arrow.clockwise.circle.fill"
        case .openFailure: return "wrench.and.screwdriver.fill"
        case .followUp: return "plus.bubble.fill"
        }
    }
}

/// Sheet for switching between missions (like dashboard's Cmd+K)
private struct MissionSwitcherSheet: View {
    let runningMissions: [RunningMissionInfo]
    let recentMissions: [Mission]
    let currentMissionId: String?
    let viewingMissionId: String?
    let onSelectMission: (String) -> Void
    let onResumeMission: (String) -> Void
    let onFollowUpMission: (Mission) -> Void
    let onOpenFailureMission: (String) -> Void
    let onCancelMission: (String) -> Void
    let onCreateNewMission: () -> Void
    let onDismiss: () -> Void

    @State private var searchText = ""
    @State private var backendSearchTask: Task<Void, Never>?
    @State private var backendSearchQuery = ""
    @State private var backendSearchResults: [MissionSearchResult] = []
    @State private var isBackendSearchLoading = false

    private let backendSearchDebounceNanos: UInt64 = 250_000_000

    private var normalizedSearchQuery: String {
        normalizeMetadataText(searchText)
    }

    private var runningMissionIds: Set<String> {
        Set(runningMissions.map { $0.missionId })
    }

    private func preferredMissionForDuplicateId(_ lhs: Mission, _ rhs: Mission) -> Mission {
        let lhsUpdated = lhs.updatedDate ?? .distantPast
        let rhsUpdated = rhs.updatedDate ?? .distantPast
        return rhsUpdated >= lhsUpdated ? rhs : lhs
    }

    private var missionById: [String: Mission] {
        Dictionary(
            recentMissions.map { ($0.id, $0) },
            uniquingKeysWith: preferredMissionForDuplicateId
        )
    }

    private var filteredRunning: [RunningMissionInfo] {
        if normalizedSearchQuery.isEmpty {
            return runningMissions
        }
        return runningMissions
            .compactMap { info -> (RunningMissionInfo, Double)? in
                let score = runningMissionSearchScore(
                    info,
                    query: normalizedSearchQuery,
                    linkedMission: missionById[info.missionId]
                )
                return score > 0 ? (info, score) : nil
            }
            .sorted { lhs, rhs in
                if lhs.1 == rhs.1 {
                    let lhsUpdated = missionById[lhs.0.missionId]?.updatedDate ?? .distantPast
                    let rhsUpdated = missionById[rhs.0.missionId]?.updatedDate ?? .distantPast
                    if lhsUpdated != rhsUpdated {
                        return lhsUpdated > rhsUpdated
                    }
                    return lhs.0.missionId < rhs.0.missionId
                }
                return lhs.1 > rhs.1
            }
            .map(\.0)
    }

    private var filteredRecent: [Mission] {
        let nonRunning = recentMissions.filter { !runningMissionIds.contains($0.id) }
        if normalizedSearchQuery.isEmpty {
            return nonRunning
        }

        let localMatches: [Mission] = nonRunning
            .compactMap { mission -> (Mission, Double)? in
                let score = missionSearchRelevanceScore(mission, query: normalizedSearchQuery)
                return score > 0 ? (mission, score) : nil
            }
            .sorted { lhs, rhs in
                if lhs.1 == rhs.1 {
                    return (lhs.0.updatedDate ?? .distantPast) > (rhs.0.updatedDate ?? .distantPast)
                }
                return lhs.1 > rhs.1
            }
            .map(\.0)

        if backendSearchQuery == normalizedSearchQuery {
            let byId = Dictionary(
                nonRunning.map { ($0.id, $0) },
                uniquingKeysWith: preferredMissionForDuplicateId
            )
            var merged: [Mission] = []
            var seen = Set<String>()

            for result in backendSearchResults {
                let mission = byId[result.mission.id] ?? result.mission
                guard !runningMissionIds.contains(mission.id) else { continue }
                if seen.insert(mission.id).inserted {
                    merged.append(mission)
                }
            }

            for mission in localMatches {
                if seen.insert(mission.id).inserted {
                    merged.append(mission)
                }
            }

            return merged
        }

        return localMatches
    }

    private var activeOrPendingMissions: [Mission] {
        filteredRecent.filter { $0.status == .active || $0.status == .pending }
    }

    private var completedMissions: [Mission] {
        filteredRecent.filter { $0.status == .completed }
    }

    private var failedMissions: [Mission] {
        filteredRecent.filter { $0.status == .failed || $0.status == .notFeasible }
    }

    private var interruptedMissions: [Mission] {
        filteredRecent.filter { $0.status == .interrupted || $0.status == .blocked || $0.status == .unknown }
    }

    @ViewBuilder
    private func missionSection(_ title: String, missions: [Mission]) -> some View {
        if !missions.isEmpty {
            Section(title) {
                ForEach(missions) { mission in
                    MissionRow(
                        missionId: mission.id,
                        displayName: missionDisplayName(for: mission),
                        title: mission.displayTitle,
                        shortDescription: missionCardDescription(for: mission),
                        backend: mission.backend,
                        status: mission.status,
                        isRunning: false,
                        runningState: nil,
                        isViewing: viewingMissionId == mission.id,
                        quickActions: missionQuickActions(for: mission),
                        onSelect: { onSelectMission(mission.id) },
                        onQuickAction: { action in
                            handleQuickAction(action, for: mission)
                        },
                        onCancel: nil
                    )
                }
            }
        }
    }

    var body: some View {
        NavigationStack {
            List {
                // Create new mission button
                Section {
                    Button {
                        onCreateNewMission()
                    } label: {
                        Label("Create New Mission", systemImage: "plus.circle.fill")
                            .foregroundStyle(Theme.accent)
                    }
                }

                // Running missions
                if !filteredRunning.isEmpty {
                    Section("Running") {
                        ForEach(filteredRunning, id: \.missionId) { info in
                            let mission = missionById[info.missionId]
                            MissionRow(
                                missionId: info.missionId,
                                displayName: mission.map { missionDisplayName(for: $0) },
                                title: mission?.displayTitle ?? info.title,
                                shortDescription: mission.flatMap { missionCardDescription(for: $0) },
                                backend: mission?.backend,
                                status: .active,
                                isRunning: true,
                                runningState: info.state,
                                isViewing: viewingMissionId == info.missionId,
                                quickActions: [.followUp],
                                onSelect: { onSelectMission(info.missionId) },
                                onQuickAction: { action in
                                    handleRunningQuickAction(
                                        action,
                                        missionId: info.missionId,
                                        mission: mission
                                    )
                                },
                                onCancel: { onCancelMission(info.missionId) }
                            )
                        }
                    }
                }

                missionSection("Active & Pending", missions: activeOrPendingMissions)
                missionSection("Completed", missions: completedMissions)
                missionSection("Failed", missions: failedMissions)
                missionSection("Interrupted", missions: interruptedMissions)

                if isBackendSearchLoading && !normalizedSearchQuery.isEmpty {
                    Section {
                        HStack(spacing: 8) {
                            ProgressView()
                                .scaleEffect(0.8)
                            Text("Searching missions...")
                                .font(.caption)
                                .foregroundStyle(Theme.textMuted)
                        }
                    }
                }

                if filteredRunning.isEmpty && filteredRecent.isEmpty && !normalizedSearchQuery.isEmpty {
                    ContentUnavailableView(
                        "No Missions Found",
                        systemImage: "magnifyingglass",
                        description: Text("No missions match '\(searchText)'")
                    )
                }
            }
            .searchable(text: $searchText, prompt: "Search missions...")
            .onChange(of: searchText) { _, newValue in
                scheduleBackendSearch(for: newValue)
            }
            .onAppear {
                scheduleBackendSearch(for: searchText)
            }
            .onDisappear {
                backendSearchTask?.cancel()
                backendSearchTask = nil
            }
            .navigationTitle("Switch Mission")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { onDismiss() }
                }
            }
        }
    }

    private func scheduleBackendSearch(for rawQuery: String) {
        backendSearchTask?.cancel()
        backendSearchTask = nil

        let normalizedQuery = normalizeMetadataText(rawQuery)
        guard !normalizedQuery.isEmpty else {
            backendSearchQuery = ""
            backendSearchResults = []
            isBackendSearchLoading = false
            return
        }

        isBackendSearchLoading = true
        backendSearchTask = Task {
            try? await Task.sleep(nanoseconds: backendSearchDebounceNanos)
            guard !Task.isCancelled else { return }

            do {
                let results = try await APIService.shared.searchMissions(query: normalizedQuery, limit: 50)
                guard !Task.isCancelled else { return }

                await MainActor.run {
                    if normalizeMetadataText(searchText) == normalizedQuery {
                        backendSearchQuery = normalizedQuery
                        backendSearchResults = results
                        isBackendSearchLoading = false
                    }
                }
            } catch {
                guard !Task.isCancelled else { return }
                await MainActor.run {
                    if normalizeMetadataText(searchText) == normalizedQuery {
                        backendSearchQuery = ""
                        backendSearchResults = []
                        isBackendSearchLoading = false
                    }
                }
            }
        }
    }

    private func normalizeMetadataText(_ text: String) -> String {
        let lowered = text.lowercased()
        let scalars = lowered.unicodeScalars.map { scalar -> Character in
            if scalar.properties.isAlphabetic
                || scalar.properties.numericType != nil
                || CharacterSet.whitespacesAndNewlines.contains(scalar)
            {
                return Character(scalar)
            }
            return " "
        }
        return String(scalars)
            .split(whereSeparator: \.isWhitespace)
            .joined(separator: " ")
    }

    private let searchStopwords: Set<String> = [
        "a", "an", "and", "at", "did", "do", "does", "for", "from", "how",
        "i", "in", "is", "it", "me", "my", "of", "on", "or", "our", "please",
        "show", "that", "the", "this", "to", "us", "was", "we", "what", "when",
        "where", "which", "who", "why", "with", "you", "your",
    ]

    private struct SearchQueryTerms {
        let normalizedQuery: String
        let normalizedCoreQuery: String
        let queryGroups: [[String]]
        let phraseQueries: [String]
    }

    private func buildSearchQueryTerms(_ query: String) -> SearchQueryTerms? {
        let normalizedQuery = normalizeMetadataText(query)
        if normalizedQuery.isEmpty { return nil }

        let queryTokens = normalizedQuery.split(separator: " ").map(String.init)
        if queryTokens.isEmpty { return nil }

        let filteredTokens = queryTokens.filter { !searchStopwords.contains($0) }
        let effectiveTokens = filteredTokens.isEmpty ? queryTokens : filteredTokens
        let normalizedCoreQuery = effectiveTokens.joined(separator: " ")

        let queryGroups = effectiveTokens
            .map(expandQueryGroup)
            .filter { !$0.isEmpty }
        if queryGroups.isEmpty { return nil }

        var phraseQueries = Set<String>()
        phraseQueries.insert(normalizedCoreQuery)
        for token in effectiveTokens {
            for phrase in phraseExpansions(for: token) {
                let normalizedPhrase = normalizeMetadataText(phrase)
                if !normalizedPhrase.isEmpty {
                    phraseQueries.insert(normalizedPhrase)
                }
            }
        }

        return SearchQueryTerms(
            normalizedQuery: normalizedQuery,
            normalizedCoreQuery: normalizedCoreQuery,
            queryGroups: queryGroups,
            phraseQueries: Array(phraseQueries)
        )
    }

    private func missionWorkspaceLabel(for mission: Mission) -> String? {
        guard let workspaceName = mission.workspaceName?.trimmingCharacters(in: .whitespacesAndNewlines),
              !workspaceName.isEmpty else {
            return nil
        }
        return workspaceName
    }

    private func missionDisplayName(for mission: Mission) -> String {
        let shortId = String(mission.id.prefix(8)).uppercased()
        if let workspaceLabel = missionWorkspaceLabel(for: mission) {
            return "\(workspaceLabel) · \(shortId)"
        }
        return shortId
    }

    private func hasMeaningfulExtraTokens(baseText: String, candidateText: String) -> Bool {
        let base = normalizeMetadataText(baseText)
        let candidate = normalizeMetadataText(candidateText)
        if candidate.isEmpty { return false }
        if base.isEmpty { return true }

        let baseTokens = Set(base.split(separator: " ").map(String.init))
        let candidateTokens = candidate.split(separator: " ").map(String.init)
        return candidateTokens.contains(where: { !baseTokens.contains($0) })
    }

    private func missionCardDescription(for mission: Mission) -> String? {
        guard let shortDescription = mission.shortDescription?.trimmingCharacters(in: .whitespacesAndNewlines),
              !shortDescription.isEmpty else {
            return nil
        }
        let title = mission.displayTitle.trimmingCharacters(in: .whitespacesAndNewlines)
        if !title.isEmpty && !hasMeaningfulExtraTokens(baseText: title, candidateText: shortDescription) {
            return nil
        }
        return shortDescription.count > 100 ? String(shortDescription.prefix(100)) + "..." : shortDescription
    }

    private func expandQueryGroup(token: String) -> [String] {
        let synonyms: [String: [String]] = [
            "api": ["endpoint", "http", "rest", "rpc"],
            "auth": ["login", "signin", "oauth", "credential", "credentials"],
            "blocked": ["stalled", "waiting"],
            "bug": ["issue", "error", "fix", "problem"],
            "cd": ["deploy", "release", "rollout", "ship"],
            "ci": ["pipeline", "build", "integration", "tests"],
            "crash": ["panic", "exception", "failure"],
            "db": ["database", "sql", "sqlite", "postgres"],
            "deploy": ["release", "rollout", "ship"],
            "error": ["bug", "issue", "failure"],
            "failed": ["error", "failure"],
            "fix": ["bug", "issue", "error", "repair"],
            "issue": ["bug", "error", "problem", "fix"],
            "login": ["auth", "signin", "oauth", "credentials"],
            "performance": ["perf", "slow", "latency", "optimize"],
            "perf": ["performance", "slow", "latency", "optimize"],
            "release": ["deploy", "rollout", "ship"],
            "sid": ["session", "id", "sessionid", "cookie", "token"],
            "signin": ["login", "auth", "oauth", "credentials"],
            "slow": ["performance", "latency", "timeout", "stall"],
            "sso": ["signin", "login", "auth", "oauth"],
            "stalled": ["blocked", "waiting", "timeout"],
            "timeout": ["slow", "latency", "stalled", "hang"],
            "ui": ["ux", "interface", "frontend"],
            "ux": ["ui", "interface", "frontend"],
        ]

        let normalized = normalizeMetadataText(token)
        if normalized.isEmpty { return [] }

        var group = Set<String>([normalized])
        for synonym in synonyms[normalized] ?? [] {
            let normalizedSynonym = normalizeMetadataText(synonym)
            if !normalizedSynonym.isEmpty {
                group.insert(normalizedSynonym)
            }
        }
        return Array(group)
    }

    private func phraseExpansions(for token: String) -> [String] {
        let normalized = normalizeMetadataText(token)
        let expansions: [String: [String]] = [
            "cd": ["continuous deployment"],
            "ci": ["continuous integration"],
            "sid": ["session id"],
            "sso": ["single sign on"],
        ]
        return expansions[normalized] ?? []
    }

    private func tokenMatchStrength(token: String, candidate: String) -> Double {
        if token == candidate { return 1.0 }

        let asciiCandidate = candidate.range(of: "^[a-z0-9]+$", options: .regularExpression) != nil
        if token.hasPrefix(candidate) && (!asciiCandidate || candidate.count >= 3) {
            return 0.7
        }
        if asciiCandidate && token.count >= 5 && candidate.hasPrefix(token) && candidate.count - token.count <= 2 {
            return 0.65
        }
        if candidate.count >= 4 && token.contains(candidate) {
            return 0.45
        }
        return 0
    }

    private func tokenSet(from text: String) -> Set<String> {
        let normalized = normalizeMetadataText(text)
        if normalized.isEmpty { return [] }
        return Set(normalized.split(separator: " ").map(String.init))
    }

    private func groupMatchStrength(_ group: [String], in tokenSet: Set<String>) -> Double {
        var best = 0.0
        for candidate in group where !candidate.isEmpty {
            for token in tokenSet {
                let strength = tokenMatchStrength(token: token, candidate: candidate)
                best = max(best, strength)
                if best >= 1 { return best }
            }
        }
        return best
    }

    private func missionSearchRelevanceScore(_ mission: Mission, query: String) -> Double {
        guard let queryTerms = buildSearchQueryTerms(query) else { return 0 }
        let phraseQueries = queryTerms.phraseQueries.isEmpty
            ? [queryTerms.normalizedCoreQuery.isEmpty ? queryTerms.normalizedQuery : queryTerms.normalizedCoreQuery]
            : queryTerms.phraseQueries

        let displayName = missionDisplayName(for: mission)
        let title = mission.displayTitle
        let shortDescription = mission.shortDescription ?? ""
        let backend = mission.backend ?? ""
        let status = mission.status.displayLabel
        let combined = "\(displayName) \(mission.id) \(title) \(shortDescription) \(backend) \(status)"
        let normalizedCombined = normalizeMetadataText(combined)
        if normalizedCombined.isEmpty { return 0 }

        let fields: [(weight: Double, tokens: Set<String>)] = [
            (5, tokenSet(from: displayName)),
            (8, tokenSet(from: title)),
            (7, tokenSet(from: shortDescription)),
            (3, tokenSet(from: backend)),
            (2, tokenSet(from: status)),
            (1, tokenSet(from: combined)),
        ]

        var score = 0.0
        for group in queryTerms.queryGroups {
            var bestGroupScore = 0.0
            for field in fields {
                let strength = groupMatchStrength(group, in: field.tokens)
                if strength > 0 {
                    bestGroupScore = max(bestGroupScore, strength * field.weight)
                }
            }
            if bestGroupScore <= 0 { return 0 }
            score += bestGroupScore
        }

        let phraseTargets: [(text: String, boost: Double)] = [
            (normalizeMetadataText(title), 14),
            (normalizeMetadataText(shortDescription), 12),
            (normalizeMetadataText(displayName), 8),
            (normalizeMetadataText(combined), 5),
        ]
        for target in phraseTargets where !target.text.isEmpty {
            if phraseQueries.contains(where: { phraseQuery in
                !phraseQuery.isEmpty && target.text.contains(phraseQuery)
            }) {
                score += target.boost
            }
        }

        return score
    }

    private func runningMissionSearchScore(
        _ mission: RunningMissionInfo,
        query: String,
        linkedMission: Mission?
    ) -> Double {
        guard let queryTerms = buildSearchQueryTerms(query) else { return 0 }
        let phraseQueries = queryTerms.phraseQueries.isEmpty
            ? [queryTerms.normalizedCoreQuery.isEmpty ? queryTerms.normalizedQuery : queryTerms.normalizedCoreQuery]
            : queryTerms.phraseQueries

        let title = mission.title ?? ""
        let combined = "\(mission.missionId) \(title) \(mission.state)"
        let candidateTokens = tokenSet(from: combined)
        if candidateTokens.isEmpty { return 0 }

        var score = 0.0
        for group in queryTerms.queryGroups {
            let strength = groupMatchStrength(group, in: candidateTokens)
            if strength <= 0 { return 0 }
            score += strength * 4.0
        }
        if phraseQueries.contains(where: { phraseQuery in
            !phraseQuery.isEmpty && normalizeMetadataText(combined).contains(phraseQuery)
        }) {
            score += 6
        }

        let metadataScore = linkedMission.map { missionSearchRelevanceScore($0, query: query) } ?? 0
        return max(score, metadataScore)
    }

    private func missionQuickActions(for mission: Mission, isRunning: Bool = false) -> [MissionQuickAction] {
        if isRunning {
            return [.followUp]
        }

        var actions: [MissionQuickAction] = []
        if mission.status == .failed {
            actions.append(.openFailure)
        }
        if mission.resumable {
            switch mission.status {
            case .interrupted:
                actions.append(.resume)
            case .blocked:
                actions.append(.continue)
            case .failed, .notFeasible:
                actions.append(.retry)
            default:
                break
            }
        }
        if mission.status != .active {
            actions.append(.followUp)
        }
        return actions
    }

    private func handleQuickAction(_ action: MissionQuickAction, for mission: Mission) {
        switch action {
        case .resume, .continue, .retry:
            onResumeMission(mission.id)
        case .openFailure:
            onOpenFailureMission(mission.id)
        case .followUp:
            onFollowUpMission(mission)
        }
    }

    private func handleRunningQuickAction(
        _ action: MissionQuickAction,
        missionId: String,
        mission: Mission?
    ) {
        if let mission {
            handleQuickAction(action, for: mission)
            return
        }
        guard action == .followUp else { return }

        Task {
            do {
                let hydratedMission = try await APIService.shared.getMission(id: missionId)
                await MainActor.run {
                    onFollowUpMission(hydratedMission)
                }
            } catch {
                // If mission hydration fails, keep the sheet responsive and skip the action.
                print("Failed to load mission for follow-up action: \(error)")
            }
        }
    }
}

// MARK: - Mission Row

private struct MissionRow: View {
    let missionId: String
    let displayName: String?
    let title: String?
    let shortDescription: String?
    let backend: String?
    let status: MissionStatus
    let isRunning: Bool
    let runningState: String?
    let isViewing: Bool
    let quickActions: [MissionQuickAction]
    let onSelect: () -> Void
    let onQuickAction: ((MissionQuickAction) -> Void)?
    let onCancel: (() -> Void)?

    private var shortId: String {
        String(missionId.prefix(8))
    }

    private var statusColor: Color {
        if isRunning {
            return Theme.success
        }
        switch status {
        case .pending: return Theme.warning
        case .active: return Theme.success
        case .completed: return Theme.textMuted
        case .failed: return Theme.error
        case .interrupted, .blocked: return Theme.warning
        case .notFeasible: return Theme.error
        case .unknown: return Theme.textMuted
        }
    }

    private var statusIcon: String {
        if isRunning {
            return "play.circle.fill"
        }
        switch status {
        case .pending: return "clock.fill"
        case .active: return "play.circle.fill"
        case .completed: return "checkmark.circle.fill"
        case .failed: return "xmark.circle.fill"
        case .interrupted: return "pause.circle.fill"
        case .blocked: return "exclamationmark.triangle.fill"
        case .notFeasible: return "questionmark.circle.fill"
        case .unknown: return "questionmark.circle.fill"
        }
    }

    /// Whether the supplied `displayName` is just the uppercased short id.
    /// `missionDisplayName(for:)` always returns at least the uppercased
    /// 8-char short id (with an optional `"<workspace> · "` prefix), so we
    /// can't detect the bare-id case by checking for nil/empty — we have to
    /// compare against the actual short id. When it matches we suppress the
    /// secondary line: the title above already carries the meaning.
    private var displayLabelIsShortId: Bool {
        guard let trimmed = displayName?.trimmingCharacters(in: .whitespacesAndNewlines),
              !trimmed.isEmpty
        else { return true }
        return trimmed.caseInsensitiveCompare(shortId) == .orderedSame
    }

    /// "<description> · <backend>" collapsed onto one line so we don't stack
    /// four lineLimit-1 captions in a narrow row.
    private var secondaryMetadataLine: String? {
        var parts: [String] = []
        if let shortDescription = shortDescription?.trimmingCharacters(in: .whitespacesAndNewlines),
           !shortDescription.isEmpty {
            parts.append(shortDescription)
        }
        if let backend = backend?.trimmingCharacters(in: .whitespacesAndNewlines),
           !backend.isEmpty {
            parts.append(backend)
        }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }

    private var trailingStatusPill: some View {
        Group {
            if isRunning, let state = runningState {
                Text(state)
                    .font(.caption2)
                    .foregroundStyle(Theme.textMuted)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 4)
                    .background(Theme.backgroundSecondary)
                    .clipShape(Capsule())
            } else {
                Text(status.displayLabel)
                    .font(.caption2)
                    .foregroundStyle(statusColor)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 4)
                    .background(statusColor.opacity(0.1))
                    .clipShape(Capsule())
            }
        }
    }

    var body: some View {
        Button {
            onSelect()
            HapticService.selectionChanged()
        } label: {
            HStack(spacing: 12) {
                Image(systemName: statusIcon)
                    .font(.system(size: 18))
                    .foregroundStyle(statusColor)
                    .symbolEffect(.pulse, options: (isRunning && runningState == "running") ? .repeating : .nonRepeating)
                    .frame(width: 24, height: 24)

                VStack(alignment: .leading, spacing: 2) {
                    // Title (or short id when there is no title) is the primary
                    // line. The viewing checkmark sits right next to it.
                    HStack(spacing: 6) {
                        Text(title?.isEmpty == false ? title! : shortId)
                            .font(.subheadline.weight(.medium))
                            .foregroundStyle(Theme.textPrimary)
                            .lineLimit(1)

                        if isViewing {
                            Image(systemName: "checkmark.circle.fill")
                                .font(.caption)
                                .foregroundStyle(Theme.accent)
                        }
                    }

                    // Secondary line: optional human display name when distinct
                    // from the short id, plus collapsed description+backend.
                    if !displayLabelIsShortId,
                       let displayName = displayName?.trimmingCharacters(in: .whitespacesAndNewlines),
                       !displayName.isEmpty,
                       displayName != title {
                        Text(displayName)
                            .font(.caption.monospaced())
                            .foregroundStyle(Theme.textSecondary)
                            .lineLimit(1)
                    }

                    if let secondaryMetadataLine {
                        Text(secondaryMetadataLine)
                            .font(.caption2)
                            .foregroundStyle(Theme.textMuted)
                            .lineLimit(1)
                            .truncationMode(.tail)
                    }
                }

                Spacer(minLength: 8)

                trailingStatusPill
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .swipeActions(edge: .trailing, allowsFullSwipe: false) {
            // Cancel ships first so it occupies the leftmost (closest)
            // trailing slot when the user swipes — matches Mail's destructive
            // affordance placement.
            if let onCancel {
                Button(role: .destructive) {
                    onCancel()
                    HapticService.lightTap()
                } label: {
                    Label("Cancel", systemImage: "xmark.circle.fill")
                }
            }
            if let onQuickAction {
                ForEach(quickActions, id: \.self) { action in
                    Button {
                        onQuickAction(action)
                        HapticService.lightTap()
                    } label: {
                        Label(action.label, systemImage: action.icon)
                    }
                    .tint(Theme.accent)
                }
            }
        }
        .contextMenu {
            // Long-press fallback: keeps every action discoverable for
            // accessibility and for users who don't know about swipes.
            if let onQuickAction {
                ForEach(quickActions, id: \.self) { action in
                    Button {
                        onQuickAction(action)
                    } label: {
                        Label(action.label, systemImage: action.icon)
                    }
                }
            }
            if let onCancel {
                Button(role: .destructive) {
                    onCancel()
                } label: {
                    Label("Cancel Mission", systemImage: "xmark.circle.fill")
                }
            }
        }
    }
}

#Preview {
    NavigationStack {
        ControlView()
    }
}
