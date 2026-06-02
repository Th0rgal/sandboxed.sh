//
//  ControlView.swift
//  SandboxedDashboard
//
//  Chat interface for the AI agent with real-time streaming
//

import SwiftUI
import os

private enum ScrollAnchorState {
    case pinned
    case detached
    case programmatic
}

/// Snapshot of the active codex goal-mode loop, surfaced as a pill above
/// the composer. Status mirrors codex's `thread/goal/updated` payload:
/// `active`, `paused`, `budgetLimited`, `complete`, or `cleared`.
struct GoalPillInfo: Equatable {
    var iteration: Int
    var status: String
    var objective: String
}

private struct BufferedStreamEvent: @unchecked Sendable {
    let type: String
    let data: [String: AnyCodable]
}

private struct ControlDiagnosticsOverlay: View {
    let missionId: String?
    let transport: String
    let streamScope: String
    let maxSequence: Int64?
    let cacheHit: Bool?
    let mergeCount: Int
    let renderCount: Int
    let droppedEvents: Int
    let streamDiagnostics: [APIService.ControlStreamDiagnostic]
    let performanceRecords: [ControlPerformanceRecord]
    let hotRenderCounts: [(name: String, count: Int)]

    var body: some View {
        let latestStream = streamDiagnostics.last
        let latestError = streamDiagnostics.reversed().first { $0.phase == .error }
        VStack(alignment: .leading, spacing: 3) {
            Text("control diagnostics")
                .font(.caption2.weight(.semibold))
                .foregroundStyle(Theme.textMuted)
            diagnosticRow("mission", missionId.map { String($0.prefix(8)) } ?? "none")
            diagnosticRow("transport", "\(latestStream?.transport.rawValue ?? transport.lowercased()) · \(streamScope)")
            diagnosticRow("stream", latestStream?.phase.rawValue ?? "?")
            if let latestError {
                diagnosticRow(
                    "last err",
                    latestError.status.map { "http \($0)" } ?? String((latestError.error ?? "?").prefix(18)),
                    warning: true
                )
            }
            diagnosticRow("max seq", maxSequence.map(String.init) ?? "?")
            diagnosticRow("cache", cacheHit.map { $0 ? "hit" : "miss" } ?? "?")
            diagnosticRow("merge", "\(mergeCount) ev")
            diagnosticRow("render", "\(renderCount) rows")
            diagnosticRow("drops", "\(droppedEvents)", warning: droppedEvents > 0)
            if let slowest = performanceRecords.max(by: { $0.durationMilliseconds < $1.durationMilliseconds }) {
                Divider()
                    .overlay(Theme.border)
                diagnosticRow("slowest", slowest.name)
                diagnosticRow("time", slowest.compactDuration, warning: slowest.durationMilliseconds >= 16)
            }
            if !hotRenderCounts.isEmpty {
                Divider()
                    .overlay(Theme.border)
                ForEach(hotRenderCounts.prefix(3), id: \.name) { item in
                    diagnosticRow(item.name, "\(item.count)x", warning: item.count > renderCount * 2 && renderCount > 0)
                }
            }
        }
        .font(.caption2.monospacedDigit())
        .padding(8)
        .background(.black.opacity(0.78), in: RoundedRectangle(cornerRadius: 8))
        .overlay(
            RoundedRectangle(cornerRadius: 8)
                .stroke(Theme.border.opacity(0.7), lineWidth: 0.5)
        )
        .foregroundStyle(Theme.textSecondary)
    }

    private func diagnosticRow(_ label: String, _ value: String, warning: Bool = false) -> some View {
        HStack(spacing: 8) {
            Text(label)
                .foregroundStyle(Theme.textMuted)
            Spacer(minLength: 12)
            Text(value)
                .foregroundStyle(warning ? Theme.warning : Theme.textPrimary)
        }
        .frame(width: 150, alignment: .leading)
    }
}

struct ControlView: View {
    private struct DraftCacheEntry: Codable {
        var text: String
        var updatedAt: Date
    }

    private struct PendingSendEntry: Codable, Equatable {
        let id: String
        let missionId: String?
        let content: String
        let createdAt: Date
    }

    private static let draftTextKey = "control_draft_text"
    private static let missionDraftsKey = "control_mission_drafts_v1"
    private static let lastMissionIdKey = "control_last_mission_id"
    private static let pendingSendsKey = "control_pending_sends_v1"
    private static let maxDraftCacheBytes = 64 * 1_024
    private static let maxDraftCacheEntries = 50
    private static let maxStreamDiagnostics = 80

    @State private var messages: [ChatMessage] = []
    @State private var inputText = ControlView.loadGlobalDraftText()
    @State private var runState: ControlRunState = .idle
    @State private var queueLength = 0
    /// Number of POST /api/control/message calls currently in flight. Caps
    /// the send button: with idempotency keys, retries are safe but
    /// rapid re-taps of the same physical button on a slow connection
    /// would still spam the server with concurrent distinct sends.
    @State private var pendingSendCount = 0
    @State private var queuedItems: [QueuedMessage] = []
    @State private var showQueueSheet = false
    @State private var currentMission: Mission?
    @State private var viewingMission: Mission?
    @State private var isLoading = true
    @State private var streamTask: Task<Void, Never>?
    @State private var streamGeneration = 0
    @State private var latestStreamSeq: Int64?
    @State private var streamDiagnostics: [APIService.ControlStreamDiagnostic] = []
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
    @State private var scrollAnchorState: ScrollAnchorState = .pinned

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
    @State private var controlCacheHit: Bool?
    /// True when the cached conversation is being shown because a snapshot
    /// fetch failed. Surfaces a "Cached · Tap to refresh" pill above the
    /// conversation so a user staring at an old chat understands why it
    /// isn't updating (Wave 4 fix 5.11).
    @State private var controlCacheStale = false
    @State private var controlMergeCount = 0
    @State private var controlDroppedEvents = 0
    @State private var controlPerformanceRecords: [ControlPerformanceRecord] = []
    @State private var controlHotRenderCounts: [(name: String, count: Int)] = []
    @AppStorage("control_debug_perf") private var showControlDiagnostics = false

    /// Per-mission high-water mark for `sequence`. When non-nil, reload paths
    /// pass it as `since_seq` to `/events` so the server returns only the tail
    /// that arrived while we were disconnected, not the whole history. Seeded
    /// from the `X-Max-Sequence` response header — backends that don't set the
    /// header leave this `nil` and callers fall back to full reload.
    @State private var missionMaxSeq: [String: Int64] = [:]
    @State private var missionMinSeq: [String: Int64] = [:]
    @State private var missionEventCache: [String: [StoredEvent]] = [:]

    // Cached grouped items (recomputed only when messages change)
    @State private var groupedItems: [GroupedChatItem] = []

    // Draft save debounce
    @State private var draftSaveTask: Task<Void, Never>?

    // Connection state for SSE stream - starts as disconnected until first event received
    @State private var connectionState: ConnectionState = .disconnected
    @State private var reconnectAttempt = 0
    /// Reachability monitor that combines NWPathMonitor + /api/health probes.
    /// The aggregated `state` is merged with the SSE state below into
    /// `effectiveConnectionState` and drives the banner. This is what fixes
    /// the "everything is hanging but the banner says Connected" bug from
    /// the bad-network audit — the SSE alone can't tell the difference
    /// between "no events because the agent is quiet" and "no events because
    /// the path is dead but the socket happens to still be open".
    @State private var networkMonitor = NetworkMonitor()

    // Parallel missions state
    @State private var runningMissions: [RunningMissionInfo] = []
    @State private var viewingMissionId: String?
    @State private var showRunningMissions = false
    @State private var pollingTask: Task<Void, Never>?

    // Track pending fetch to prevent race conditions
    @State private var fetchingMissionId: String?

    // Thoughts panel state
    @State private var showThoughts = false
    @State private var textOpBuffers: [String: String] = [:]

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

    private var diagnostics: ControlPerformanceDiagnostics {
        ControlPerformanceDiagnostics.shared
    }
    
    var body: some View {
        bodyContent
            .navigationTitle(viewingMission?.displayTitle ?? "Control")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .principal) { AnyView(principalToolbarContent) }
                ToolbarItem(placement: .topBarLeading) { AnyView(leadingToolbarContent) }
                ToolbarItem(placement: .topBarTrailing) { AnyView(missionSwitcherToolbarButton) }
                ToolbarItem(placement: .topBarTrailing) { AnyView(overflowMenuToolbarItem) }
            }
        .task { await coldStart() }
        .onChange(of: nav.pendingMissionId) { _, newId in
            handlePendingMissionId(newId)
        }
        .onChange(of: currentMission?.id) { _, newId in
            syncViewingMissionFromCurrent(newId: newId)
        }
        .onChange(of: scenePhase) { oldPhase, newPhase in
            handleScenePhaseChange(from: oldPhase, to: newPhase)
        }
        .onChange(of: viewingMissionId) { oldId, newId in
            handleViewingMissionChange(from: oldId, to: newId)
        }
        .onChange(of: inputText) { _, newText in
            scheduleDraftSave(newText)
        }
        .onChange(of: showControlDiagnostics) { _, enabled in
            if enabled {
                diagnostics.reset()
                updateControlPerformanceSnapshot()
            }
        }
        .onDisappear {
            networkMonitor.noteStreamIdle()
            streamTask?.cancel()
            connectionState = .disconnected
            reconnectAttempt = 0
            pollingTask?.cancel()
            networkMonitor.stop()
            // Save draft immediately on disappear
            saveCurrentDraft(inputText)
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
            missionSwitcherSheetContent
        }
        .sheet(isPresented: $showQueueSheet) {
            QueueSheet(
                items: queuedItems,
                // Synchronous removes so swipe-to-delete shrinks the row in
                // the same render frame as the gesture, even on a slow
                // network (the API call is fire-and-forget; the optimistic
                // update is rolled back on failure). The previous Task
                // wrapper deferred the mutation by one runloop tick, which
                // SwiftUI's `List.onDelete` interprets as "data source
                // didn't shrink" — the row snapped back before
                // disappearing.
                onRemove: { messageId in
                    removeFromQueueOptimistic(messageId: messageId)
                },
                onClearAll: {
                    clearQueueOptimistic()
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

    /// The toolbar's full content, declared as a `@ToolbarContentBuilder`
    /// method so SwiftUI resolves it as one opaque ToolbarContent rather
    /// than re-typing every item through the View body's modifier chain.
    @ToolbarContentBuilder
    private var toolbarItems: some ToolbarContent {
        ToolbarItem(placement: .principal) { principalToolbarContent }
        ToolbarItem(placement: .topBarLeading) { leadingToolbarContent }
        ToolbarItem(placement: .topBarTrailing) { missionSwitcherToolbarButton }
        ToolbarItem(placement: .topBarTrailing) { overflowMenuToolbarItem }
    }

    /// Mission switcher sheet body. Extracted from `.sheet` closure so the
    /// View body modifier chain stays under the type-checker budget.
    @ViewBuilder
    private var missionSwitcherSheetContent: some View {
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

    /// Leading toolbar item: thoughts panel button + workers button.
    @ViewBuilder
    private var leadingToolbarContent: some View {
        HStack(spacing: 12) {
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

    /// Trailing toolbar: mission switcher button.
    @ViewBuilder
    private var missionSwitcherToolbarButton: some View {
        Button {
            Task { await loadRecentMissions() }
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

    /// Trailing toolbar: overflow `...` menu with all the actions.
    @ViewBuilder
    private var overflowMenuToolbarItem: some View {
        Menu {
            overflowMenuContent
        } label: {
            Image(systemName: "ellipsis.circle")
                .font(.body)
        }
    }

    /// Contents of the overflow menu — pulled out separately so the menu's
    /// label closure stays tiny.
    @ViewBuilder
    private var overflowMenuContent: some View {
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

        Toggle(isOn: $showControlDiagnostics) {
            Label("Control Diagnostics", systemImage: "gauge.with.dots.needle.bottom.50percent")
        }

        if let mission = viewingMission {
            Divider()
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
    }

    /// Principal toolbar content — title + status row. Extracted to a
    /// computed property so the View body stays under the SwiftUI
    /// type-checker complexity budget.
    private var principalToolbarContent: some View {
        let fullTitle: String = {
            if let mission = viewingMission {
                let trimmed = mission.title?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
                return trimmed.isEmpty ? "Untitled Mission" : trimmed
            }
            return "Control"
        }()
        return VStack(spacing: 2) {
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
            principalStatusRow
        }
    }

    /// The agent · run state · queue · progress row under the title.
    private var principalStatusRow: some View {
        HStack(spacing: 4) {
            if let mission = viewingMission,
               let agent = mission.agent,
               !agent.isEmpty {
                let backendColor = missionBackendColor(mission)
                Image(systemName: missionBackendIcon(mission))
                    .font(.system(size: 9))
                    .foregroundStyle(backendColor)
                Text(agent)
                    .font(.caption2)
                    .foregroundStyle(backendColor)
                Text("•")
                    .foregroundStyle(Theme.textMuted)
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

            if let progress, progress.total > 0 {
                Text("•")
                    .foregroundStyle(Theme.textMuted)
                Text(progress.displayText)
                    .font(.caption2.weight(.medium))
                    .foregroundStyle(Theme.success)
            }
        }
    }

    /// Top-level body ZStack — opaque single-View so the long modifier
    /// chain on `body` doesn't blow the SwiftUI type-checker budget.
    private var bodyContent: some View {
        ZStack {
            Theme.backgroundPrimary.ignoresSafeArea()
            backgroundGlows
            mainContentStack
            diagnosticsOverlay
        }
    }

    @ViewBuilder
    private var diagnosticsOverlay: some View {
        if showControlDiagnostics {
            ControlDiagnosticsOverlay(
                missionId: viewingMissionId,
                transport: "SSE",
                streamScope: viewingMissionId == nil ? "global" : "mission",
                maxSequence: viewingMissionId.flatMap { missionMaxSeq[$0] },
                cacheHit: controlCacheHit,
                mergeCount: controlMergeCount,
                renderCount: groupedItems.count,
                droppedEvents: controlDroppedEvents,
                streamDiagnostics: streamDiagnostics,
                performanceRecords: controlPerformanceRecords,
                hotRenderCounts: controlHotRenderCounts
            )
            .padding(.top, 8)
            .padding(.trailing, 8)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topTrailing)
            .allowsHitTesting(false)
        }
    }

    private func updateControlPerformanceSnapshot() {
        guard showControlDiagnostics else { return }
        controlPerformanceRecords = diagnostics.recentRecords
        controlHotRenderCounts = diagnostics.hotRenderCounts
    }

    /// Main vertical stack: connection banner, optional stale-cache pill,
    /// the conversation, the input row. Extracted from `body` to keep the
    /// top-level type-checker scope small.
    private var mainContentStack: some View {
        MainContentStack(
            showBanner: networkMonitor.state.showsBanner,
            bannerView: ConnectionBannerView(state: networkMonitor.state),
            showStaleCachePill: controlCacheStale,
            staleCachePill: staleCachePill,
            messagesView: messagesView,
            workerPill: workerPillOrNil,
            inputView: inputView
        )
    }

    @ViewBuilder
    private var workerPillOrNil: some View {
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

    /// Cold-start sequencing. Previously every step here was awaited
    /// serially — workspaces → loadMission → loadCurrentMission →
    /// refreshRunningMissions — which on a slow cellular link stacked up to
    /// a dozen sequential RTTs before the user could read or type anything.
    /// Now: SSE first (live events arrive immediately), then the three
    /// independent context fetches in parallel via `async let`, then start
    /// the running-missions poller.
    private func coldStart() async {
        networkMonitor.start()
        startStreaming()

        async let workspacesTask: Void = workspaceState.loadWorkspaces()
        async let runningTask: Bool = refreshRunningMissions()
        async let missionTask: Void = loadInitialMission()

        _ = await (workspacesTask, runningTask, missionTask)

        if runningMissions.count > 1 {
            showRunningMissions = true
        }
        startPollingRunningMissions()
    }

    /// Handle navigation from History while Control is already visible.
    private func handlePendingMissionId(_ newId: String?) {
        guard let missionId = newId else { return }
        nav.pendingMissionId = nil
        Task { await loadMission(id: missionId) }
    }

    /// On change of `currentMission?.id`: if no mission is being viewed yet,
    /// apply the new current as the viewing mission. Extracted from the
    /// View body for type-checker complexity.
    private func syncViewingMissionFromCurrent(newId: String?) {
        guard viewingMissionId == nil,
              let id = newId,
              let mission = currentMission,
              mission.id == id else { return }
        applyViewingMission(mission)
    }

    /// Handle the user switching missions. Saves the previous mission's
    /// draft, restores the new mission's draft, tears down and re-starts
    /// the SSE stream. Extracted from the View body so the SwiftUI
    /// type-checker doesn't time out on the long modifier chain.
    private func handleViewingMissionChange(from oldId: String?, to newId: String?) {
        saveDraft(inputText, missionId: oldId)
        UserDefaults.standard.set(newId, forKey: Self.lastMissionIdKey)
        inputText = loadDraft(missionId: newId)
        networkMonitor.noteStreamIdle()
        streamTask?.cancel()
        connectionState = .disconnected
        reconnectAttempt = 0
        latestStreamSeq = nil
        startStreaming()
    }

    /// Debounced draft autosave (1s after last keystroke).
    private func scheduleDraftSave(_ newText: String) {
        draftSaveTask?.cancel()
        draftSaveTask = Task {
            try? await Task.sleep(for: .seconds(1))
            guard !Task.isCancelled else { return }
            await MainActor.run {
                saveCurrentDraft(newText)
            }
        }
    }

    /// Handle scenePhase transitions. Extracted from the View body so the
    /// SwiftUI type-checker doesn't OOM on the long modifier chain.
    private func handleScenePhaseChange(from oldPhase: ScenePhase, to newPhase: ScenePhase) {
        if newPhase != .active {
            networkMonitor.setSceneActive(false)
            // Save draft text when leaving foreground. This is the
            // synchronous flush path — the debounced .onChange in
            // inputText handles the steady-state case, but
            // backgrounding/inactivity needs an immediate write because
            // iOS may kill the process before the next debounced tick
            // would have fired (Wave 4 fix 5.8).
            saveCurrentDraft(inputText)
        }
        guard oldPhase != .active && newPhase == .active else { return }
        networkMonitor.setSceneActive(true)
        // If the SSE is already reconnecting (or its inactivity watchdog
        // is about to fire), the reconnect path will call
        // resumeMissionAfterReconnect — duplicating that work here causes
        // overlapping event-range merges and visible flicker on a slow
        // link. Skip the explicit reload when SSE is mid-recovery
        // (Wave 4 fix 5.12).
        let sseIsRecovering: Bool
        switch connectionState {
        case .reconnecting: sseIsRecovering = true
        default: sseIsRecovering = false
        }
        let streamIsTerminalBadState: Bool
        switch connectionState {
        case .authExpired, .invalidConfiguration:
            streamIsTerminalBadState = true
        default:
            streamIsTerminalBadState = false
        }
        Task {
            guard !streamIsTerminalBadState else { return }
            if !sseIsRecovering, let missionId = viewingMissionId {
                if !shouldSkipForegroundReload(missionId: missionId) {
                    await reloadMissionFromServer(id: missionId)
                }
            }
            await refreshRunningMissions()
        }
    }

    private func shouldSkipForegroundReload(missionId: String) -> Bool {
        guard networkMonitor.isStreamFresh(maxAge: NetworkMonitor.staleAfter * 2),
              let latestStreamSeq,
              let knownSeq = missionMaxSeq[missionId] else {
            return false
        }
        return latestStreamSeq <= knownSeq
    }

    private func recordStreamDiagnostic(_ diagnostic: APIService.ControlStreamDiagnostic) {
        streamDiagnostics.append(diagnostic)
        if streamDiagnostics.count > Self.maxStreamDiagnostics {
            streamDiagnostics.removeFirst(streamDiagnostics.count - Self.maxStreamDiagnostics)
        }
    }

    /// "Cached · Tap to refresh" pill rendered when a snapshot fetch failed
    /// and we're still showing previously-cached events (Wave 4 fix 5.11).
    private var staleCachePill: some View {
        Button {
            Task { await refreshViewingMissionSnapshot() }
        } label: {
            HStack(spacing: 8) {
                Image(systemName: "clock.arrow.circlepath")
                    .font(.system(size: 11, weight: .semibold))
                Text("Cached · Tap to refresh")
                    .font(.caption.weight(.medium))
                Spacer()
            }
            .foregroundStyle(Theme.textSecondary)
            .padding(.horizontal, 16)
            .padding(.vertical, 6)
            .background(Theme.textSecondary.opacity(0.10))
        }
        .buttonStyle(.plain)
    }

    /// Sticky strip rendered above the conversation when reachability is
    /// anything other than healthy. The toolbar already shows a small
    /// wifi-slash glyph, but it's easy to miss on a long page; this strip
    /// stays in peripheral vision.
    private var connectionBanner: some View {
        ConnectionBannerView(state: networkMonitor.state)
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
                    // Skeleton bubbles instead of a bare spinner: gives the
                    // user something shaped like the result while the
                    // snapshot round-trip is in flight, so the cold-open
                    // path doesn't feel like a hang.
                    ShimmerConversation()
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

    /// Workspace + mission ids that inline rich-tag images need to fetch
    /// from `/api/fs/download`. Threaded down via SwiftUI environment so
    /// every MarkdownView in the scroll picks it up without an extra
    /// parameter at the call site.
    private var inlineImageContext: MissionFileContext {
        let mission = viewingMission ?? currentMission
        return MissionFileContext(
            workspaceId: mission?.workspaceId,
            missionId: mission?.id
        )
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

                    ConversationRowsView(
                        groupedItems: groupedItems,
                        copiedMessageId: copiedMessageId,
                        expandedToolGroups: $expandedToolGroups,
                        onCopy: copyMessage,
                        onRetry: retryFailedMessage
                    )

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
                let pinned = maxY < UIScreen.main.bounds.height + 200
                if scrollAnchorState != .programmatic {
                    scrollAnchorState = pinned ? .pinned : .detached
                }
                isAtBottom = scrollAnchorState == .pinned
            }
            .onTapGesture {
                // Dismiss keyboard when tapping on messages area
                isInputFocused = false
            }
            .onChange(of: messages.count) { _, _ in
                if let pendingFocusedMessageId {
                    scheduleMessageFocusRetry(proxy: proxy, targetId: pendingFocusedMessageId)
                }
                // Only auto-scroll while explicitly pinned and not loading historical messages.
                // This prevents the jarring animated scroll when loading cached/historical conversations
                if scrollAnchorState == .pinned && !isLoadingHistory {
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
                if scrollAnchorState == .detached && !messages.isEmpty {
                    Button {
                        scrollToBottom(proxy: proxy)
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
            .environment(\.missionFileContext, inlineImageContext)
            .environment(\.controlPerformanceDiagnosticsEnabled, showControlDiagnostics)
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
        groupedItems = diagnostics.measure(
            "control.group_messages",
            detail: viewingMissionId ?? "none",
            count: messages.count
        ) {
            Self.buildGroupedItems(from: messages)
        }
        updateControlPerformanceSnapshot()
    }

    /// Check if the currently viewed mission is running (not just any mission)
    private var viewingMissionIsRunning: Bool {
        guard let viewingId = viewingMissionId else {
            // No specific mission being viewed - fall back to global state
            if let currentMission, missionIsTerminalForInlineThinking(currentMission) {
                return false
            }
            return runState != .idle
        }
        if let viewingMission, viewingMission.id == viewingId, missionIsTerminalForInlineThinking(viewingMission) {
            return false
        }
        // Check if this specific mission is in the running missions list
        guard let missionInfo = runningMissions.first(where: { $0.missionId == viewingId }) else {
            return false
        }
        return missionInfo.state == "running" || missionInfo.state == "waiting_for_tool"
    }

    private func missionIsTerminalForInlineThinking(_ mission: Mission) -> Bool {
        mission.status.statusType == .completed || mission.status.statusType == .failed
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
        scrollAnchorState = .programmatic
        if immediate {
            // Immediate scroll without animation for loading historical conversations
            proxy.scrollTo(bottomAnchorId, anchor: .bottom)
        } else {
            // Animated scroll for new messages during active conversation
            withAnimation {
                proxy.scrollTo(bottomAnchorId, anchor: .bottom)
            }
        }
        Task { @MainActor in
            try? await Task.sleep(for: .milliseconds(immediate ? 16 : 300))
            scrollAnchorState = .pinned
            isAtBottom = true
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

    /// True when the send button should accept taps: non-empty input and no
    /// in-flight send. Capping concurrent sends to 1 prevents rapid re-taps
    /// from spamming the server with distinct messages while the network is
    /// slow; with idempotency keys the *same* message would be deduped, but
    /// distinct sequential text inputs would not be.
    private var canSend: Bool {
        hasInput && pendingSendCount == 0
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
        case "grok": pool = catalog.grok ?? []
        default:
            // No mission yet → show every backend's commands so the user
            // can preview what's available.
            pool = catalog.opencode
                + catalog.claudecode
                + (catalog.codex ?? [])
                + (catalog.grok ?? [])
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

    private var latestVisibleThought: ChatMessage? {
        guard viewingMissionIsRunning else { return nil }
        return messages.last { message in
            guard message.isThinking else { return false }
            guard !message.thinkingDone else { return false }
            return !message.content.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
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

            if runState != .idle, let thought = latestVisibleThought {
                InlineThinkingSurface(message: thought) {
                    showThoughts = true
                    HapticService.lightTap()
                }
                .padding(.horizontal, 12)
                .padding(.top, 8)
                .transition(.move(edge: .bottom).combined(with: .opacity))
                .accessibilityIdentifier("control-inline-thinking")
            }

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

                // Send button - disabled while a send is in flight to prevent
                // double-sends on a slow link. The in-flight count drives the
                // spinner; the optimistic bubble itself shows the pending
                // state, so this is just a hand-rest safety net.
                Button {
                    sendMessage()
                } label: {
                    Group {
                        if pendingSendCount > 0 {
                            ProgressView()
                                .controlSize(.mini)
                                .tint(.white)
                        } else {
                            Image(systemName: "arrow.up")
                                .font(.system(size: 14, weight: .semibold))
                                .foregroundStyle(canSend ? .white : Theme.textMuted)
                        }
                    }
                    .frame(width: 32, height: 32)
                    .background(canSend ? Theme.accent : (pendingSendCount > 0 ? Theme.accent.opacity(0.6) : Color.clear))
                    .clipShape(Circle())
                    .overlay(
                        Circle()
                            .stroke(!canSend && pendingSendCount == 0 ? Theme.border : Color.clear, lineWidth: 1)
                    )
                }
                .disabled(!canSend)
                .padding(.trailing, 8)
            }
            .animation(.easeInOut(duration: 0.15), value: runState)
            .animation(.easeInOut(duration: 0.15), value: pendingSendCount > 0)
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

    // MARK: - Composer Draft Cache

    private static func loadGlobalDraftText() -> String {
        UserDefaults.standard.string(forKey: draftTextKey) ?? ""
    }

    private func loadDraft(missionId: String?) -> String {
        guard let missionId else {
            return Self.loadGlobalDraftText()
        }

        if let entry = Self.loadDraftCache()[missionId] {
            return entry.text
        }

        // One-way compatibility path for the previous global draft. The old
        // value is claimed by the first mission opened after upgrade, then
        // removed so it cannot leak into every other mission tab.
        let legacy = Self.loadGlobalDraftText()
        if !legacy.isEmpty {
            saveDraft(legacy, missionId: missionId)
            UserDefaults.standard.removeObject(forKey: Self.draftTextKey)
        }
        return legacy
    }

    private func saveCurrentDraft(_ text: String) {
        saveDraft(text, missionId: viewingMissionId)
    }

    private func saveDraft(_ text: String, missionId: String?) {
        guard let missionId else {
            if text.isEmpty {
                UserDefaults.standard.removeObject(forKey: Self.draftTextKey)
            } else {
                UserDefaults.standard.set(text, forKey: Self.draftTextKey)
            }
            return
        }

        var drafts = Self.loadDraftCache()
        if text.isEmpty {
            drafts.removeValue(forKey: missionId)
        } else {
            drafts[missionId] = DraftCacheEntry(text: text, updatedAt: Date())
        }
        Self.storeDraftCache(drafts)
        UserDefaults.standard.removeObject(forKey: Self.draftTextKey)
    }

    private func removeCurrentDraft() {
        saveDraft("", missionId: viewingMissionId)
    }

    private static func loadDraftCache() -> [String: DraftCacheEntry] {
        guard let data = UserDefaults.standard.data(forKey: missionDraftsKey),
              let drafts = try? JSONDecoder().decode([String: DraftCacheEntry].self, from: data) else {
            return [:]
        }
        return drafts
    }

    private static func storeDraftCache(_ drafts: [String: DraftCacheEntry]) {
        var pruned = drafts
        while pruned.count > maxDraftCacheEntries {
            guard let oldest = pruned.min(by: { $0.value.updatedAt < $1.value.updatedAt })?.key else { break }
            pruned.removeValue(forKey: oldest)
        }

        var encoded = try? JSONEncoder().encode(pruned)
        while let data = encoded, data.count > maxDraftCacheBytes, !pruned.isEmpty {
            guard let oldest = pruned.min(by: { $0.value.updatedAt < $1.value.updatedAt })?.key else { break }
            pruned.removeValue(forKey: oldest)
            encoded = try? JSONEncoder().encode(pruned)
        }

        if let encoded, !pruned.isEmpty {
            UserDefaults.standard.set(encoded, forKey: missionDraftsKey)
        } else {
            UserDefaults.standard.removeObject(forKey: missionDraftsKey)
        }
    }

    // MARK: - Pending Send Cache

    private static func loadPendingSends() -> [PendingSendEntry] {
        guard let data = UserDefaults.standard.data(forKey: pendingSendsKey),
              let entries = try? JSONDecoder().decode([PendingSendEntry].self, from: data) else {
            return []
        }
        return entries
    }

    private static func storePendingSends(_ entries: [PendingSendEntry]) {
        if let data = try? JSONEncoder().encode(entries), !entries.isEmpty {
            UserDefaults.standard.set(data, forKey: pendingSendsKey)
        } else {
            UserDefaults.standard.removeObject(forKey: pendingSendsKey)
        }
    }

    private func rememberPendingSend(id: String, missionId: String?, content: String) {
        var entries = Self.loadPendingSends()
        entries.removeAll { $0.id == id }
        entries.append(PendingSendEntry(id: id, missionId: missionId, content: content, createdAt: Date()))
        Self.storePendingSends(entries)
    }

    private func forgetPendingSend(id: String) {
        var entries = Self.loadPendingSends()
        entries.removeAll { $0.id == id }
        Self.storePendingSends(entries)
    }

    private func restorePendingSends(for missionId: String?) {
        let entries = Self.loadPendingSends().filter { $0.missionId == missionId }
        guard !entries.isEmpty else { return }
        for entry in entries where !messages.contains(where: { $0.id == entry.id }) {
            messages.append(ChatMessage(
                id: entry.id,
                type: .user,
                content: entry.content,
                sendState: .failed(reason: "Not sent before the app closed")
            ))
        }
        recomputeGroupedItems()
    }

    // Cache both mission metadata and events for consistent display
    private struct CachedMissionData: Codable {
        let mission: Mission
        let events: [StoredEvent]
        let cachedAt: Date
    }

    private struct CachedMissionDataBox: @unchecked Sendable {
        let value: CachedMissionData
    }

    private static let maxCachedMissions = 10  // Limit cache size
    private static let maxCachedEventsPerMission = 1_500
    private static let maxSynchronousCacheBytes = 256 * 1_024
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
        let cacheData = CachedMissionData(
            mission: mission,
            events: Self.trimEventsForCache(Self.compactEventsForCache(events)),
            cachedAt: Date()
        )

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

    private static func compactEventsForCache(_ events: [StoredEvent]) -> [StoredEvent] {
        let sorted = events.sorted { lhs, rhs in
            if lhs.sequence != rhs.sequence { return lhs.sequence < rhs.sequence }
            if lhs.timestamp != rhs.timestamp { return lhs.timestamp < rhs.timestamp }
            return lhs.id < rhs.id
        }

        var compacted: [StoredEvent] = []
        var thinkingFirst: StoredEvent?
        var thinkingLatest: StoredEvent?
        var thinkingContent = ""

        func flushThinking() {
            guard let first = thinkingFirst, let latest = thinkingLatest else { return }
            compacted.append(
                StoredEvent(
                    id: first.id,
                    missionId: first.missionId,
                    sequence: first.sequence,
                    eventType: first.eventType,
                    timestamp: latest.timestamp,
                    eventId: first.eventId,
                    toolCallId: first.toolCallId,
                    toolName: first.toolName,
                    content: thinkingContent,
                    metadata: first.metadata.merging(latest.metadata) { _, latest in latest }
                )
            )
            thinkingFirst = nil
            thinkingLatest = nil
            thinkingContent = ""
        }

        for event in sorted {
            guard event.eventType == "thinking" else {
                flushThinking()
                compacted.append(event)
                continue
            }

            let content = event.content
            let done = event.metadata["done"]?.value as? Bool == true
            guard let _ = thinkingFirst else {
                thinkingFirst = event
                thinkingLatest = event
                thinkingContent = content
                if done { flushThinking() }
                continue
            }

            if !Self.isStreamContinuation(content, previous: thinkingContent) {
                flushThinking()
                thinkingFirst = event
                thinkingLatest = event
                thinkingContent = content
                if done { flushThinking() }
                continue
            }

            thinkingLatest = event
            if content.count > thinkingContent.count {
                thinkingContent = content
            }
            if done { flushThinking() }
        }

        flushThinking()
        return compacted
    }

    private static func trimEventsForCache(_ events: [StoredEvent]) -> [StoredEvent] {
        guard events.count > maxCachedEventsPerMission else { return events }

        let protectedTypes: Set<String> = ["thinking", "text_delta", "text_op"]
        let protectedEvents = events.filter { protectedTypes.contains($0.eventType) }
        let protectedSlice = protectedEvents.count > maxCachedEventsPerMission
            ? Array(protectedEvents.suffix(maxCachedEventsPerMission))
            : protectedEvents
        var keep = Set<Int64>(protectedSlice.map(\.sequence))

        var remaining = maxCachedEventsPerMission - keep.count
        if remaining > 0 {
            for event in events.reversed() where !keep.contains(event.sequence) {
                keep.insert(event.sequence)
                remaining -= 1
                if remaining == 0 { break }
            }
        }

        return events.filter { keep.contains($0.sequence) }
    }

    private static func isStreamContinuation(_ content: String, previous: String) -> Bool {
        if previous.isEmpty || content.isEmpty { return true }
        if content.hasPrefix(previous) || previous.hasPrefix(content) { return true }
        return content.commonPrefix(with: previous).count >= min(content.count, previous.count) / 2
    }

    private func loadCachedMissionData(_ missionId: String) -> CachedMissionData? {
        guard let url = Self.cacheFileURL(missionId: missionId),
              Self.cacheFileSize(url: url) <= Self.maxSynchronousCacheBytes,
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

    private func loadCachedMissionDataAsync(_ missionId: String) async -> CachedMissionData? {
        guard let url = Self.cacheFileURL(missionId: missionId) else { return nil }
        let box = try? await Task.detached(priority: .userInitiated) {
            let data = try Data(contentsOf: url)
            let cached = try JSONDecoder().decode(CachedMissionData.self, from: data)
            return CachedMissionDataBox(value: cached)
        }.value
        guard let cached = box?.value else { return nil }

        if var cachedKeys = UserDefaults.standard.stringArray(forKey: Self.cacheKeysKey) {
            cachedKeys.removeAll { $0 == missionId }
            cachedKeys.append(missionId)
            UserDefaults.standard.set(cachedKeys, forKey: Self.cacheKeysKey)
        }
        return cached
    }

    private func scheduleLargeCachedMissionLoad(id: String) {
        guard let url = Self.cacheFileURL(missionId: id),
              Self.cacheFileSize(url: url) > Self.maxSynchronousCacheBytes else { return }
        Task {
            guard let cached = await loadCachedMissionDataAsync(id) else { return }
            guard viewingMissionId == id else { return }
            let cachedMaxSeq = cached.events.compactMap(\.sequence).max() ?? 0
            if let currentMaxSeq = missionMaxSeq[id], currentMaxSeq >= cachedMaxSeq {
                return
            }
            controlCacheHit = true
            controlCacheStale = false
            applyViewingMissionWithEvents(cached.mission, events: cached.events)
        }
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

    nonisolated private static func cacheFileSize(url: URL) -> Int {
        (try? url.resourceValues(forKeys: [.fileSizeKey]))?.fileSize ?? 0
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
        goalInfo = mission.goalMode
            ? GoalPillInfo(iteration: goalInfo?.iteration ?? 0, status: "active", objective: mission.goalObjective ?? "")
            : nil
        hasMoreHistory = false
        loadedEventCount = 0
        controlMergeCount = mission.history.count
        messages = mission.history.enumerated().map { index, entry in
            ChatMessage(
                id: "\(mission.id)-\(index)",
                type: entry.isUser ? .user : .assistant(success: true, costCents: 0, costSource: .unknown, model: nil, sharedFiles: nil),
                content: entry.content
            )
        }
        restorePendingSends(for: mission.id)
        recomputeGroupedItems()

        if scrollToBottom {
            shouldScrollImmediately = true
            scrollToBottomTick += 1
        }
        clearLoadingHistoryAfterRender()
    }

    private func replayedGoalInfo(for mission: Mission, events: [StoredEvent]) -> GoalPillInfo? {
        guard mission.goalMode else { return nil }

        var replayed: GoalPillInfo? = GoalPillInfo(
            iteration: 0,
            status: "active",
            objective: mission.goalObjective ?? ""
        )

        for event in events {
            switch event.eventType {
            case "goal_iteration":
                let metadata = event.metadata.mapValues(\.value)
                let iteration = intValue(metadata["iteration"]) ?? replayed?.iteration ?? 0
                let eventObjective = event.content.trimmingCharacters(in: .whitespacesAndNewlines)
                let objective =
                    metadata["objective"] as? String ??
                    (!eventObjective.isEmpty ? eventObjective : nil) ??
                    replayed?.objective ??
                    mission.goalObjective ??
                    ""
                replayed = GoalPillInfo(
                    iteration: iteration,
                    status: replayed?.status ?? "active",
                    objective: objective
                )

            case "goal_status":
                let metadata = event.metadata.mapValues(\.value)
                let status = metadata["status"] as? String ?? event.content
                let objective =
                    metadata["objective"] as? String ??
                    replayed?.objective ??
                    mission.goalObjective ??
                    ""
                switch status {
                case "complete", "cleared", "budgetLimited":
                    replayed = nil
                default:
                    replayed = GoalPillInfo(
                        iteration: replayed?.iteration ?? 0,
                        status: status,
                        objective: objective
                    )
                }

            default:
                continue
            }
        }

        return replayed
    }

    private func intValue(_ value: Any?) -> Int? {
        if let int = value as? Int { return int }
        if let int64 = value as? Int64 { return Int(int64) }
        if let double = value as? Double { return Int(double) }
        if let string = value as? String { return Int(string) }
        return nil
    }

    private func rememberMissionEvents(_ events: [StoredEvent], for missionId: String) -> [StoredEvent] {
        let orderedEvents = events.sorted { lhs, rhs in
            if lhs.sequence != rhs.sequence {
                return lhs.sequence < rhs.sequence
            }
            if lhs.timestamp != rhs.timestamp {
                return lhs.timestamp < rhs.timestamp
            }
            return lhs.id < rhs.id
        }

        missionEventCache[missionId] = orderedEvents
        if let minSequence = orderedEvents.first?.sequence {
            missionMinSeq[missionId] = minSequence
        } else {
            missionMinSeq.removeValue(forKey: missionId)
        }
        if let maxSequence = orderedEvents.last?.sequence {
            missionMaxSeq[missionId] = max(missionMaxSeq[missionId] ?? 0, maxSequence)
        }
        return orderedEvents
    }

    private func mergeMissionEvents(_ incoming: [StoredEvent], for missionId: String) -> [StoredEvent] {
        var bySequence: [Int64: StoredEvent] = [:]
        for event in missionEventCache[missionId] ?? [] {
            bySequence[event.sequence] = event
        }
        for event in incoming {
            bySequence[event.sequence] = event
        }
        return rememberMissionEvents(Array(bySequence.values), for: missionId)
    }

    private func applyViewingMissionWithEvents(_ mission: Mission, events: [StoredEvent], scrollToBottom: Bool = true) {
        diagnostics.measure(
            "control.apply_snapshot",
            detail: mission.id,
            count: events.count
        ) {
            isLoadingHistory = true  // Suppress animated auto-scroll during history load

            viewingMission = mission
            viewingMissionId = mission.id

            // Ensure deterministic replay order in case the backend returns unsorted results
            let orderedEvents = diagnostics.measure(
                "control.sort_remember_events",
                detail: mission.id,
                count: events.count
            ) {
                rememberMissionEvents(events, for: mission.id)
            }

            // Track total event count for pagination
            loadedEventCount = orderedEvents.count
            controlMergeCount = orderedEvents.count
            goalInfo = replayedGoalInfo(for: mission, events: orderedEvents)

            // Clear and replay all events to rebuild message history
            messages.removeAll(keepingCapacity: true)

            diagnostics.measure(
                "control.replay_events",
                detail: mission.id,
                count: orderedEvents.count
            ) {
                let replay = ChatHistoryReducer.reduceWithState(events: orderedEvents, mission: mission)
                messages = replay.messages
                textOpBuffers = replay.textOpBuffers
            }
            restorePendingSends(for: mission.id)

            // Recompute grouped items once after all events are processed
            recomputeGroupedItems()
        }

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

        diagnostics.measure(
            "control.apply_delta",
            detail: viewingMissionId ?? "none",
            count: events.count
        ) {
            let orderedEvents = events.sorted { lhs, rhs in
                if lhs.sequence != rhs.sequence { return lhs.sequence < rhs.sequence }
                if lhs.timestamp != rhs.timestamp { return lhs.timestamp < rhs.timestamp }
                return lhs.id < rhs.id
            }

            for event in orderedEvents {
                handleStreamEvent(type: event.eventType, data: eventDataForReplay(event), isHistoricalReplay: true)
            }

            loadedEventCount += orderedEvents.count
            controlMergeCount = orderedEvents.count
            recomputeGroupedItems()
        }
    }

    private func eventDataForReplay(_ event: StoredEvent) -> [String: Any] {
        var data: [String: Any] = [:]
        for (key, value) in event.metadata {
            data[key] = value.value
        }
        data["mission_id"] = event.missionId
        data["content"] = event.content
        if event.eventType == "text_op",
           let jsonData = event.content.data(using: .utf8),
           let ops = try? JSONSerialization.jsonObject(with: jsonData) {
            data["ops"] = ops
        }
        if let eventId = event.eventId { data["id"] = eventId }
        if let toolCallId = event.toolCallId { data["tool_call_id"] = toolCallId }
        if let toolName = event.toolName { data["name"] = toolName }
        return data
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
            controlCacheHit = true
        } else {
            hasCache = false
            if updateViewing {
                controlCacheHit = false
                if let currentId = currentMission?.id ?? viewingMissionId {
                    scheduleLargeCachedMissionLoad(id: currentId)
                }
            }
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
                        let snapshot = try await diagnostics.measureAsync(
                            "control.fetch_current_snapshot",
                            detail: mission.id
                        ) {
                            try await api.getMissionSnapshot(id: mission.id)
                        }
                        let events = snapshot.events
                        controlCacheHit = hasCache

                        if events.isEmpty {
                            // Clear stale cache when events are empty
                            removeMissionFromCache(mission.id)
                            applyViewingMission(mission)
                        } else {
                            hasMoreHistory = snapshot.totalEvents > events.count
                            applyViewingMissionWithEvents(snapshot.mission, events: events)
                            if snapshot.latestSequence > 0 { missionMaxSeq[mission.id] = snapshot.latestSequence }
                            childMissions = snapshot.childMissions
                            // Update cache with fresh data
                            cacheMissionWithEvents(snapshot.mission, events: events)
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
        defer {
            if fetchingMissionId == id {
                fetchingMissionId = nil
            }
        }
        let previousViewingMission = viewingMission
        let previousViewingId = viewingMissionId
        viewingMissionId = id

        // Fire-and-forget: tell the backend the user opened this mission.
        // First call (per AwaitingUser round) starts the 1h ack grace timer
        // and paints the "opened" dot in Finished; later calls are no-ops.
        Task { [api] in
            _ = try? await api.markMissionOpened(id: id)
        }

        // Clear stale workers from previous mission immediately
        childMissions = []

        // Try to load cached version first for immediate display with consistent event-based rendering
        let hasCache: Bool
        if let cachedData = loadCachedMissionData(id) {
            // Use cached events for consistent display (avoids flash when fresh data arrives)
            applyViewingMissionWithEvents(cachedData.mission, events: cachedData.events)
            hasCache = true
            controlCacheHit = true
        } else {
            hasCache = false
            controlCacheHit = false
            scheduleLargeCachedMissionLoad(id: id)
        }

        // Only show loading state if we don't have cached data to display
        if !hasCache {
            isLoading = true
        }

        do {
            let snapshot = try await diagnostics.measureAsync(
                "control.fetch_snapshot",
                detail: id
            ) {
                try await api.getMissionSnapshot(id: id)
            }
            let mission = snapshot.mission

            // Race condition guard: only update if this is still the mission we want
            guard fetchingMissionId == id else {
                return // Another mission was requested, discard this response
            }

            if currentMission?.id == mission.id {
                currentMission = mission
            }

            let events = snapshot.events
            controlCacheHit = hasCache
            if events.isEmpty {
                // Clear stale cache when events are empty to prevent visual flashing
                removeMissionFromCache(mission.id)
                applyViewingMission(mission)
            } else {
                hasMoreHistory = snapshot.totalEvents > events.count
                applyViewingMissionWithEvents(mission, events: events)
                if snapshot.latestSequence > 0 {
                    missionMaxSeq[id] = snapshot.latestSequence
                }
                childMissions = snapshot.childMissions
                cacheMissionWithEvents(mission, events: events)
            }

            isLoading = false
            HapticService.success()
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
            let olderEvents = try await diagnostics.measureAsync(
                "control.fetch_earlier",
                detail: missionId
            ) {
                try await api.getMissionEvents(
                    id: missionId,
                    types: historyEventTypes,
                    limit: Self.loadEarlierPageLimit,
                    beforeSeq: missionMinSeq[missionId]
                )
            }
            guard viewingMissionId == missionId else { return }

            if !olderEvents.isEmpty, let mission = viewingMission {
                // Only mark history exhausted if we got fewer rows than the
                // page cap. If the server returned a full page, more may
                // remain — keep the button visible.
                hasMoreHistory = olderEvents.count >= Self.loadEarlierPageLimit
                let allEvents = mergeMissionEvents(olderEvents, for: missionId)
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
        case noCursor     // no high-water mark recorded; nothing attempted
        case failed       // network/server error; cursor untouched
    }

    /// Try to fetch and apply only the events that arrived after our recorded
    /// high-water mark for this mission. Shared between `resumeMissionAfterReconnect`
    /// (post SSE reconnect) and `reloadMissionFromServer` (post scene-phase active).
    /// Caller is responsible for the fallback path when this returns anything
    /// other than `.applied`.
    /// Safety cap on the number of pages a single delta-resume will drain
    /// (50 * 5000 = 250k events). Prevents an unbounded loop on a pathological
    /// mission while still catching up multi-day backlogs in one pass.
    static let deltaResumeMaxPages = 50

    /// Result of draining the delta backlog: the events fetched and the
    /// sequence the cursor should advance to.
    struct DeltaDrain {
        let events: [StoredEvent]
        let finalCursor: Int64
        let pages: Int
    }

    /// Pure, dependency-free paginator: keep calling `fetchPage(cursor)` until a
    /// page comes back short (fully caught up), the page reaches the server's
    /// reported max sequence, the page makes no forward progress (defensive
    /// guard), or we hit `maxPages`. Returns every event drained plus the cursor
    /// to persist.
    ///
    /// This loop is the fix for the staleness bug: the SSE stream does NOT
    /// replay events missed while disconnected (the server ignores `since_seq`
    /// on the stream), so this delta fetch is the only gap-filler. The previous
    /// code fetched a single page, leaving a multi-day backlog permanently
    /// behind (the tail froze on an old message). Extracted as `static` so it's
    /// unit-testable without a live `APIService`.
    static func drainDelta(
        from initialCursor: Int64,
        pageLimit: Int,
        maxPages: Int,
        fetchPage: (Int64) async throws -> (events: [StoredEvent], maxSeq: Int64)
    ) async rethrows -> DeltaDrain {
        var cursor = initialCursor
        var collected: [StoredEvent] = []
        var pages = 0
        while pages < maxPages {
            let (events, maxSeq) = try await fetchPage(cursor)
            collected.append(contentsOf: events)
            pages += 1
            let pageMax = events.map { $0.sequence }.max() ?? cursor
            let fullPage = events.count >= pageLimit
            if !fullPage || pageMax >= maxSeq || pageMax <= cursor {
                return DeltaDrain(events: collected, finalCursor: max(maxSeq, pageMax), pages: pages)
            }
            cursor = pageMax
        }
        return DeltaDrain(events: collected, finalCursor: cursor, pages: pages)
    }

    private func tryDeltaResume(missionId id: String) async -> DeltaResumeOutcome {
        guard let initialCursor = missionMaxSeq[id] else { return .noCursor }
        let drain: DeltaDrain
        do {
            drain = try await Self.drainDelta(
                from: initialCursor,
                pageLimit: Self.deltaResumePageLimit,
                maxPages: Self.deltaResumeMaxPages
            ) { cursor in
                let result = try await diagnostics.measureAsync(
                    "control.fetch_delta",
                    detail: id
                ) {
                    try await api.getMissionEventsWithMeta(
                        id: id,
                        types: historyEventTypes,
                        limit: Self.deltaResumePageLimit,
                        sinceSeq: cursor
                    )
                }
                return (result.events, result.maxSequence ?? cursor)
            }
        } catch {
            print("Delta resume failed: \(error)")
            return .failed
        }
        // Apply only if the user is still viewing this mission (a long drain may
        // have outlived the selection).
        guard viewingMissionId == id else { return .viewChanged }
        if !drain.events.isEmpty {
            _ = mergeMissionEvents(drain.events, for: id)
            applyDeltaEvents(drain.events)
        }
        missionMaxSeq[id] = max(drain.finalCursor, missionMaxSeq[id] ?? initialCursor)
        return .applied
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
                case .noCursor, .failed:
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
            do {
                let snapshot = try await diagnostics.measureAsync(
                    "control.fetch_reload_snapshot",
                    detail: id
                ) {
                    try await api.getMissionSnapshot(id: id)
                }
                guard viewingMissionId == id else { return }
                if snapshot.events.isEmpty {
                    removeMissionFromCache(mission.id)
                    applyViewingMission(snapshot.mission, scrollToBottom: false)
                } else {
                    hasMoreHistory = snapshot.totalEvents > snapshot.events.count
                    applyViewingMissionWithEvents(snapshot.mission, events: snapshot.events, scrollToBottom: false)
                    if snapshot.latestSequence > 0 { missionMaxSeq[id] = snapshot.latestSequence }
                    childMissions = snapshot.childMissions
                    cacheMissionWithEvents(snapshot.mission, events: snapshot.events)
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
        case .noCursor:
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
        let previousStatus = mission.status
        // Flip the status pill instantly on the menu tap; roll back on
        // failure so the badge tracks the server's true state.
        viewingMission?.status = status
        if currentMission?.id == mission.id {
            currentMission?.status = status
        }
        do {
            try await api.setMissionStatus(id: mission.id, status: status)
            HapticService.success()
        } catch {
            print("Failed to set status: \(error)")
            viewingMission?.status = previousStatus
            if currentMission?.id == mission.id {
                currentMission?.status = previousStatus
            }
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
        removeCurrentDraft()
        HapticService.lightTap()

        // The client-generated UUID doubles as the optimistic bubble id, the
        // server's message id, and the idempotency key. Because the backend
        // honours `client_message_id` (see control.rs:ControlMessageRequest),
        // a retry of a POST whose response was lost is safe — the server
        // returns the cached response with the same id, and the SSE delivery
        // dedupes by id below. No temp-prefix juggling needed.
        let clientMessageId = UUID().uuidString
        let pendingMissionId = viewingMissionId
        let tempMessage = ChatMessage(
            id: clientMessageId,
            type: .user,
            content: content,
            sendState: .pending
        )
        messages.append(tempMessage)
        rememberPendingSend(id: clientMessageId, missionId: pendingMissionId, content: content)
        recomputeGroupedItems()
        scrollToBottomTick += 1
        pendingSendCount += 1

        Task { @MainActor in
            defer { pendingSendCount = max(0, pendingSendCount - 1) }
            do {
                let (messageId, queued) = try await api.sendMessageWithRetry(
                    content: content,
                    clientMessageId: clientMessageId,
                    missionId: pendingMissionId
                )

                // With idempotency, `messageId == clientMessageId` by server
                // contract. Just clear the pending flag; no row rewrite needed.
                // Defensive: also handle the unlikely case where SSE already
                // replaced the row with the same id (it would still be at the
                // same index because we use the id, not the temp- prefix).
                if let index = messages.firstIndex(where: { $0.id == clientMessageId || $0.id == messageId }) {
                    messages[index].sendState = .sent
                }
                forgetPendingSend(id: clientMessageId)

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
                // Mark the bubble as failed and surface a retry affordance.
                // Do NOT remove it — preserving the user's intent on screen
                // is more important than tidiness, and the SSE may still
                // deliver the underlying message later (in which case the
                // SSE handler resolves the row by id).
                let reason = (error as? LocalizedError)?.errorDescription
                    ?? (error as? URLError)?.localizedDescription
                    ?? "Send failed"
                if let index = messages.firstIndex(where: { $0.id == clientMessageId }) {
                    messages[index].sendState = .failed(reason: reason)
                }
                recomputeGroupedItems()
                HapticService.error()
            }
        }
    }

    /// Re-send a previously-failed user message. Reuses the original id as
    /// the idempotency key — so even if the original POST actually reached
    /// the server (and we only failed to receive the response), this retry
    /// is a no-op on the backend.
    private func retryFailedMessage(_ message: ChatMessage) {
        guard message.isUser, message.sendState.isFailed else { return }
        let id = message.id
        let content = message.content
        let pendingMissionId = viewingMissionId

        if let index = messages.firstIndex(where: { $0.id == id }) {
            messages[index].sendState = .pending
        }
        recomputeGroupedItems()
        HapticService.lightTap()
        pendingSendCount += 1

        Task { @MainActor in
            defer { pendingSendCount = max(0, pendingSendCount - 1) }
            do {
                let (_, queued) = try await api.sendMessageWithRetry(
                    content: content,
                    clientMessageId: id,
                    missionId: pendingMissionId
                )
                if let index = messages.firstIndex(where: { $0.id == id }) {
                    messages[index].sendState = .sent
                }
                forgetPendingSend(id: id)
                if queued { queueLength += 1 }
            } catch {
                let reason = (error as? LocalizedError)?.errorDescription
                    ?? (error as? URLError)?.localizedDescription
                    ?? "Send failed"
                if let index = messages.firstIndex(where: { $0.id == id }) {
                    messages[index].sendState = .failed(reason: reason)
                }
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

    /// Synchronous optimistic remove — runs *during* the swipe gesture so
    /// SwiftUI's `List.onDelete` animation has the new (smaller) data
    /// source on the same render frame. Network call is fire-and-forget
    /// in a detached `Task`. The previous implementation wrapped the whole
    /// thing in `Task { await … }`, which deferred the array mutation by
    /// one runloop tick — on a slow connection the row visibly snapped
    /// back to its original position before re-disappearing.
    private func removeFromQueueOptimistic(messageId: String) {
        withAnimation(.easeOut(duration: 0.2)) {
            queuedItems.removeAll { $0.id == messageId }
            queueLength = max(0, queueLength - 1)
        }
        Task {
            do {
                try await api.removeFromQueue(messageId: messageId)
            } catch {
                print("Failed to remove from queue: \(error)")
                // Reconcile with the server on error so the row reappears
                // if the delete actually didn't take effect.
                await loadQueueItems()
                queueLength = queuedItems.count
                HapticService.error()
            }
        }
    }

    /// Synchronous optimistic clear — same rationale as
    /// `removeFromQueueOptimistic`.
    private func clearQueueOptimistic() {
        withAnimation(.easeOut(duration: 0.2)) {
            queuedItems = []
            queueLength = 0
        }
        showQueueSheet = false
        Task {
            do {
                _ = try await api.clearQueue()
                HapticService.success()
            } catch {
                print("Failed to clear queue: \(error)")
                // Reconcile with the server on error so any items that
                // did not actually clear reappear.
                await loadQueueItems()
                queueLength = queuedItems.count
                HapticService.error()
            }
        }
    }

    private func startStreaming() {
        streamGeneration += 1
        let generation = streamGeneration
        streamTask?.cancel()
        let missionFilter = viewingMissionId
        streamTask = Task {
            // Exponential backoff with jitter: 1s, 2s, 4s, 8s, 16s, max 30s.
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
                let stopRetrying = OSAllocatedUnfairLock(initialState: false)
                let pendingEvents = OSAllocatedUnfairLock(initialState: [BufferedStreamEvent]())
                let flushScheduled = OSAllocatedUnfairLock(initialState: false)

                func scheduleEventFlush() {
                    let shouldSchedule = flushScheduled.withLock { scheduled in
                        if scheduled { return false }
                        scheduled = true
                        return true
                    }
                    guard shouldSchedule else { return }

                    Task {
                        try? await Task.sleep(for: .milliseconds(16))
                        let batch = pendingEvents.withLock { events in
                            let batch = events
                            events.removeAll(keepingCapacity: true)
                            return batch
                        }
                        flushScheduled.withLock { $0 = false }
                        guard !batch.isEmpty else { return }

                        await MainActor.run {
                            guard generation == self.streamGeneration else { return }
                            let hasLiveSignal = batch.contains { event in
                                event.type != "error"
                                    && event.type != "connected"
                                    && event.type != "parseError"
                            }
                            let wasReconnecting = !self.connectionState.isConnected && self.reconnectAttempt > 0
                            if hasLiveSignal && !self.connectionState.isConnected {
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
                            // Tell the reachability monitor we're hearing
                            // from the server. This is the truthy signal that
                            // beats both NWPathMonitor heuristics and a
                            // half-open-socket false-positive.
                            self.networkMonitor.noteStreamActivity()
                            for event in batch {
                                let rawData = event.data.mapValues { $0.value }
                                if event.type == "heartbeat" {
                                    if let seq = rawData["seq"] as? Int64 {
                                        self.latestStreamSeq = seq
                                    } else if let seq = rawData["seq"] as? Int {
                                        self.latestStreamSeq = Int64(seq)
                                    }
                                    continue
                                }
                                if event.type == "connected" {
                                    self.networkMonitor.noteStreamConnected()
                                    continue
                                }
                                self.handleStreamEvent(
                                    type: event.type,
                                    data: rawData
                                )
                            }
                        }
                    }
                }

                let currentSinceSeq = await MainActor.run {
                    missionFilter.flatMap { self.missionMaxSeq[$0] }
                }
                _ = await withCheckedContinuation { continuation in
                    let innerTask = api.streamControl(
                        missionId: missionFilter,
                        sinceSeq: currentSinceSeq,
                        preferWebSocket: true,
                        generation: generation,
                        onDiagnostic: { diagnostic in
                            Task { @MainActor in
                                guard generation == self.streamGeneration else { return }
                                self.recordStreamDiagnostic(diagnostic)
                            }
                        }
                    ) { eventType, data in
                        guard generation == self.streamGeneration else { return }
                        if eventType == "error" {
                            if let status = data["status"] as? Int, status == 401 {
                                stopRetrying.withLock { $0 = true }
                                Task { @MainActor in
                                    guard generation == self.streamGeneration else { return }
                                    api.markSessionExpired()
                                    self.connectionState = .authExpired
                                    self.networkMonitor.noteStreamAuthExpired()
                                }
                            } else if (data["reason"] as? String) == "invalid_configuration" {
                                stopRetrying.withLock { $0 = true }
                                Task { @MainActor in
                                    guard generation == self.streamGeneration else { return }
                                    self.connectionState = .invalidConfiguration
                                    self.networkMonitor.noteStreamInvalidConfiguration()
                                }
                            }
                        }
                        // Only server-sourced events count for backoff reset.
                        // `connected` is a synthetic the client emits when it
                        // opens the stream — counting it would let a server
                        // that immediately closes still reset our backoff to
                        // 1s every cycle, causing a reconnect storm.
                        // `parseError` doesn't prove the server is healthy
                        // either; it usually means something else is broken.
                        let isServerSourced = eventType != "error"
                            && eventType != "connected"
                            && eventType != "parseError"
                        if isServerSourced {
                            receivedSuccessfulEvent.withLock { $0 = true }
                        }
                        let codableData = data.mapValues { AnyCodable($0) }
                        pendingEvents.withLock { events in
                            events.append(
                                BufferedStreamEvent(
                                    type: eventType,
                                    data: codableData
                                )
                            )
                        }
                        scheduleEventFlush()
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
                if stopRetrying.withLock({ $0 }) { break }

                // Update state to reconnecting
                await MainActor.run {
                    guard generation == self.streamGeneration else { return }
                    reconnectAttempt += 1
                    connectionState = .reconnecting(attempt: reconnectAttempt)
                    networkMonitor.noteStreamReconnecting(attempt: reconnectAttempt)
                }

                while !Task.isCancelled {
                    let pathIsUp = await MainActor.run { self.networkMonitor.pathSatisfied }
                    if pathIsUp { break }
                    try? await Task.sleep(for: .seconds(1))
                }

                // Wait before reconnecting. Jitter prevents every suspended
                // client from hammering the backend at the same cadence after
                // a network flap.
                let jitteredBackoff = Double(currentBackoff) * Double.random(in: 0.8...1.3)
                try? await Task.sleep(for: .seconds(jitteredBackoff))
                currentBackoff = min(currentBackoff * 2, maxBackoff)

                // Check cancellation again after sleep
                guard !Task.isCancelled else { break }
            }
        }
    }
    
    // MARK: - Parallel Missions
    
    /// Returns true when the call completed (regardless of `runningMissions`
    /// changing); false when the network call threw. The poller uses this
    /// to drive its backoff so a hung link doesn't spin at full speed
    /// (Wave 3 fix 5.7).
    @discardableResult
    private func refreshRunningMissions() async -> Bool {
        var ok = true
        do {
            runningMissions = try await api.getRunningMissions()
        } catch {
            print("Failed to refresh running missions: \(error)")
            ok = false
        }

        // Only fetch the (~35 kB) full mission list when there is reason to
        // believe this view actually has child missions to display. The
        // previous code unconditionally downloaded the whole list every 3 s
        // so it could client-side filter to children of one mission, burning
        // a quarter of a metered cellular plan's headroom on a no-op for the
        // common case of a non-boss mission (Wave 3 fix 5.7).
        guard let id = viewingMissionId else { return ok }
        let alreadyHasChildren = !childMissions.isEmpty
        let runningAsParent = runningMissions.contains { $0.missionId == id }
        guard alreadyHasChildren || runningAsParent || viewingMissionIsBoss else {
            // Defensive: if we used to have children and now don't, reflect
            // that in state. Doesn't issue any network call.
            return ok
        }
        do {
            let workers = try await api.getChildMissions(parentId: id)
            guard viewingMissionId == id else { return ok }
            childMissions = workers
        } catch {
            // Don't flip ok=false: missing child list is non-fatal and we
            // don't want it to throttle the running-missions cadence.
            print("Failed to refresh child missions: \(error)")
        }
        return ok
    }

    /// Heuristic: does the currently-viewed mission appear to be a parent?
    /// Used to gate the (large) full-mission-list fetch in
    /// `refreshRunningMissions`. We don't have a server-side
    /// `is_boss` flag, so this is best-effort.
    private var viewingMissionIsBoss: Bool {
        // The mission's own metadata may tag it as having parallel workers.
        // For now, fall back to "we've seen children before, or one is
        // currently running attributed to this mission". This is the same
        // signal the existing code used, just expressed positively.
        guard let id = viewingMissionId else { return false }
        return childMissions.contains(where: { $0.parentMissionId == id })
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
        // Cadence: 5s when healthy, doubled (capped 60s) on each consecutive
        // failure. This stops the previous "60s URLSession timeout, then
        // immediately retry, repeat forever" loop on bad networks (Wave 3
        // fix 5.7). The SSE event handler kicks an immediate refresh on
        // mission lifecycle events (mission_status_changed etc.) so the
        // polling cadence is the floor, not the only update path.
        let baseInterval: TimeInterval = 5
        let maxInterval: TimeInterval = 60
        pollingTask = Task {
            var consecutiveFailures = 0
            while !Task.isCancelled {
                let interval = min(baseInterval * pow(2, Double(consecutiveFailures)), maxInterval)
                try? await Task.sleep(for: .seconds(interval))
                guard !Task.isCancelled else { break }
                let ok = await refreshRunningMissions()
                consecutiveFailures = ok ? 0 : consecutiveFailures + 1
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
        defer {
            if fetchingMissionId == id {
                fetchingMissionId = nil
            }
        }

        // Clear stale workers from previous mission immediately
        childMissions = []

        // Cache-first render so mission switches don't blank the chat.
        // `loadMission` has done this for a while; `switchToMission` (used by
        // the mission switcher, running-mission chip, worker peek) used to
        // skip the cache and show `LoadingView("Loading conversation…")`
        // until both the metadata and the events round-trips returned —
        // multi-second blank on a slow link even when the data was already
        // on disk. (UX audit item #1.)
        let hasCache: Bool
        if let cached = loadCachedMissionData(id) {
            applyViewingMissionWithEvents(cached.mission, events: cached.events)
            hasCache = true
            controlCacheHit = true
            controlCacheStale = false   // optimistic; flipped true on fetch failure below
        } else {
            hasCache = false
            controlCacheHit = false
            isLoading = true
            controlCacheStale = false
            scheduleLargeCachedMissionLoad(id: id)
        }

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
            let snapshot = try await diagnostics.measureAsync(
                "control.fetch_switch_snapshot",
                detail: id
            ) {
                try await api.getMissionSnapshot(id: id)
            }
            let mission = snapshot.mission

            // Race condition guard: only update if this is still the mission we want
            guard fetchingMissionId == id else {
                return // Another mission was requested, discard this response
            }

            // Update current mission if this is the main mission.
            if currentMission?.id == mission.id {
                currentMission = mission
            }

            if !snapshot.events.isEmpty {
                controlCacheHit = hasCache
                controlCacheStale = false
                hasMoreHistory = snapshot.totalEvents > snapshot.events.count
                applyViewingMissionWithEvents(mission, events: snapshot.events)
                if snapshot.latestSequence > 0 { missionMaxSeq[id] = snapshot.latestSequence }
                childMissions = snapshot.childMissions
                cacheMissionWithEvents(mission, events: snapshot.events)
            } else if !hasCache {
                // Only fall through to "no events" if we never rendered cached
                // events — otherwise an intermittent snapshot
                // failure would blow away a perfectly good cached view.
                removeMissionFromCache(mission.id)
                applyViewingMission(mission)
            }

            isLoading = false
            HapticService.selectionChanged()
        } catch {
            // Race condition guard: only show error if this is still the mission we want
            guard fetchingMissionId == id else { return }

            isLoading = false
            print("Failed to switch mission: \(error)")
            HapticService.error()

            // Snapshot fetch failed but we're already showing cached events.
            // Flag the cache as stale so the UI can surface a "Cached · Tap
            // to refresh" pill, rather than silently showing potentially
            // outdated content (Wave 4 fix 5.11).
            if hasCache {
                controlCacheStale = true
            }

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

    /// Re-fetch the snapshot for the currently-viewed mission, used by the
    /// stale-cache pill's tap-to-refresh affordance (Wave 4 fix 5.11).
    private func refreshViewingMissionSnapshot() async {
        guard let id = viewingMissionId else { return }
        do {
            let snapshot = try await diagnostics.measureAsync(
                "control.fetch_refresh_snapshot",
                detail: id
            ) {
                try await api.getMissionSnapshot(id: id)
            }
            guard viewingMissionId == id else { return }
            currentMission = currentMission?.id == snapshot.mission.id ? snapshot.mission : currentMission
            if !snapshot.events.isEmpty {
                hasMoreHistory = snapshot.totalEvents > snapshot.events.count
                applyViewingMissionWithEvents(snapshot.mission, events: snapshot.events)
                if snapshot.latestSequence > 0 { missionMaxSeq[id] = snapshot.latestSequence }
                childMissions = snapshot.childMissions
                cacheMissionWithEvents(snapshot.mission, events: snapshot.events)
            }
            controlCacheStale = false
            HapticService.success()
        } catch {
            print("Failed to refresh snapshot: \(error)")
            HapticService.error()
        }
    }
    
    private func cancelMission(id: String) async {
        // Drop the chip from the running-missions bar immediately so the
        // tap-to-cancel feels instant on slow networks. The
        // `refreshRunningMissions` below reconciles with the server.
        let removedRunning = runningMissions.first { $0.missionId == id }
        if removedRunning != nil {
            withAnimation(.easeOut(duration: 0.2)) {
                runningMissions.removeAll { $0.missionId == id }
            }
        }
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
            // Restore the chip on failure.
            if let restored = removedRunning,
               !runningMissions.contains(where: { $0.missionId == id }) {
                withAnimation {
                    runningMissions.append(restored)
                }
            }
            HapticService.error()
        }
    }

    private var historyEventTypes: [String] {
        ["user_message", "assistant_message", "assistant_message_canonical", "tool_call", "tool_result", "text_delta", "text_op", "thinking"]
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
        if type == "stream_lagged" {
            if let dropped = data["dropped"] as? Int {
                controlDroppedEvents += dropped
            }
            if let viewingId = viewingMissionId {
                Task { await resumeMissionAfterReconnect(id: viewingId) }
            }
            return
        }

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
                // With idempotent send (client_message_id == server id), the
                // optimistic bubble already has this id. We only need to
                // resolve its send state to `.sent`. If we don't have it at
                // all, the message came from another client/session — append.
                if let index = messages.firstIndex(where: { $0.id == id }) {
                    // Heal any orphaned pending/failed state (POST in flight
                    // when SSE landed, or a "failed" bubble whose underlying
                    // POST actually reached the server). Content is taken
                    // from the SSE event in case the server normalised it.
                    messages[index].sendState = .sent
                    messages[index].content = content
                    forgetPendingSend(id: id)
                } else {
                    let message = ChatMessage(id: id, type: .user, content: content)
                    messages.append(message)
                    forgetPendingSend(id: id)
                }
            }
            
        case "assistant_message", "assistant_message_canonical":
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

        case "text_op":
            let bubbleId = data["bubble_id"] as? String ?? "text-op-latest"
            let ops = data["ops"] as? [[String: Any]] ?? []
            var content = textOpBuffers[bubbleId] ?? ""
            var finalized = false

            for op in ops {
                switch op["type"] as? String {
                case "insert":
                    let pos = min(max(op["pos"] as? Int ?? content.count, 0), content.count)
                    let index = content.index(content.startIndex, offsetBy: pos)
                    content.insert(contentsOf: op["text"] as? String ?? "", at: index)
                case "replace":
                    let range = op["range"] as? [Int] ?? []
                    let start = min(max(range.first ?? 0, 0), content.count)
                    let end = min(max(range.dropFirst().first ?? content.count, start), content.count)
                    let startIndex = content.index(content.startIndex, offsetBy: start)
                    let endIndex = content.index(content.startIndex, offsetBy: end)
                    content.replaceSubrange(startIndex..<endIndex, with: op["text"] as? String ?? "")
                case "finalize":
                    finalized = true
                default:
                    continue
                }
            }

            textOpBuffers[bubbleId] = finalized ? nil : content
            upsertStreamingFallbackThought(content: content, done: finalized)

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

            if done,
               data["goal_role"] as? String == "deliverable",
               (viewingMission?.goalMode == true || currentMission?.goalMode == true) {
                let eventId = data["id"] as? String
                let messageId = eventId.map { "goal-deliverable-\($0)" } ?? "goal-deliverable-\(UUID().uuidString)"
                guard !messages.contains(where: { $0.id == messageId }) else { break }
                finalizeActiveThinkingMessages()
                messages.removeAll { $0.isPhase }
                messages.append(
                    ChatMessage(
                        id: messageId,
                        type: .assistant(success: true, costCents: 0, costSource: .unknown, model: nil, sharedFiles: nil),
                        content: content
                    )
                )
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
                let recentCompletedStatusKnown = recentMissions.contains(where: {
                    $0.id == missionId && $0.hasFinishedSuccessfully
                })
                let completedStatusAlreadyKnown =
                    (viewingMission?.id == missionId && viewingMission?.hasFinishedSuccessfully == true)
                    || (currentMission?.id == missionId && currentMission?.hasFinishedSuccessfully == true)
                    || recentCompletedStatusKnown
                if newStatus == .interrupted && completedStatusAlreadyKnown {
                    return
                }

                // If mission is no longer active AND it's the currently viewed mission,
                // mark all pending tools as cancelled
                if newStatus != .active && viewingMissionId == missionId {
                    finalizeActiveThinkingMessages()
                    markActiveToolCallsAsCompleted(withState: .cancelled)
                }
                if newStatus != .active {
                    runningMissions.removeAll { $0.missionId == missionId }
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

// MARK: - Conversation Rows

private struct ConversationRowsView: View {
    let groupedItems: [GroupedChatItem]
    let copiedMessageId: String?
    @Binding var expandedToolGroups: Set<String>
    let onCopy: (ChatMessage) -> Void
    let onRetry: (ChatMessage) -> Void

    var body: some View {
        ForEach(groupedItems) { item in
            switch item {
            case .single(let message):
                MessageBubble(
                    message: message,
                    isCopied: copiedMessageId == message.id,
                    onCopy: { onCopy(message) },
                    onRetry: message.sendState.isFailed ? { onRetry(message) } : nil
                )
                .modifier(ControlBodyRenderProbe(name: "MessageBubble"))
                .id(message.id)
            case .toolGroup(let groupId, let tools):
                ToolGroupView(
                    groupId: groupId,
                    tools: tools,
                    expandedGroups: $expandedToolGroups
                )
                .modifier(ControlBodyRenderProbe(name: "ToolGroupView"))
                .id(item.id)
            }
        }
    }
}

// MARK: - Message Bubble

private struct MessageBubble: View {
    let message: ChatMessage
    var isCopied: Bool = false
    var onCopy: (() -> Void)?
    var onRetry: (() -> Void)?

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
                    .background(bubbleBackground)
                    .clipShape(
                        .rect(
                            topLeadingRadius: 20,
                            bottomLeadingRadius: 20,
                            bottomTrailingRadius: 6,
                            topTrailingRadius: 20
                        )
                    )
                    .overlay(
                        UnevenRoundedRectangle(
                            topLeadingRadius: 20,
                            bottomLeadingRadius: 20,
                            bottomTrailingRadius: 6,
                            topTrailingRadius: 20
                        )
                        .stroke(message.sendState.isFailed ? Theme.error : Color.clear, lineWidth: 1)
                    )
                    // While the message is awaiting server ack, dim the bubble
                    // and overlay a small spinner so the user has unambiguous
                    // feedback that the send is in flight. (UX audit item #11.)
                    .opacity(message.sendState.isPending ? 0.55 : 1)
                    .overlay(alignment: .bottomTrailing) {
                        if message.sendState.isPending {
                            ProgressView()
                                .controlSize(.mini)
                                .tint(.white)
                                .padding(6)
                        } else if message.sendState.isFailed {
                            Image(systemName: "exclamationmark.circle.fill")
                                .font(.caption)
                                .foregroundStyle(Theme.error)
                                .padding(6)
                        }
                    }
                    .animation(.easeOut(duration: 0.15), value: message.sendState.isPending)
                    .animation(.easeOut(duration: 0.15), value: message.sendState.isFailed)
                    .contextMenu {
                        Button {
                            onCopy?()
                        } label: {
                            Label("Copy", systemImage: "doc.on.doc")
                        }
                        if message.sendState.isFailed, onRetry != nil {
                            Button {
                                onRetry?()
                            } label: {
                                Label("Retry send", systemImage: "arrow.clockwise")
                            }
                        }
                    }

                // Inline "Send failed — Tap to retry" affordance directly under
                // the failed bubble. Mirrors iMessage's "Not Delivered" pattern
                // so users get an unmistakable signal and a one-tap recovery.
                if message.sendState.isFailed, let reason = message.sendState.failureReason {
                    Button(action: { onRetry?() }) {
                        HStack(spacing: 4) {
                            Image(systemName: "arrow.clockwise.circle.fill")
                                .font(.caption2)
                            Text("Not sent · Tap to retry")
                                .font(.caption2.weight(.medium))
                        }
                        .foregroundStyle(Theme.error)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Retry send. Failed: \(reason)")
                } else {
                    // Timestamp
                    Text(message.timestamp, style: .time)
                        .font(.caption2)
                        .foregroundStyle(Theme.textMuted)
                }
            }
        }
    }

    /// Background color/material for the user bubble. Failed sends render with
    /// a dimmed error tint so the row is unmistakably distinct from a normal
    /// (sent) bubble, in case the user is glancing rather than reading.
    private var bubbleBackground: Color {
        message.sendState.isFailed ? Theme.error.opacity(0.55) : Theme.accent
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
                    .modifier(ControlBodyRenderProbe(name: "MarkdownView"))
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
            // Image preview with authentication support. The shimmer
            // skeleton matches the inline rich-image placeholder so the
            // chat feels consistent while either type loads.
            Group {
                if let url = fullURL,
                   let data = imageData,
                   let uiImage = ImageMemoryCache.shared.cachedImage(for: url) ?? UIImage(data: data) {
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
                    ShimmerSkeleton(cornerRadius: 12, height: 200)
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

        if ImageMemoryCache.shared.cachedImage(for: url) != nil {
            imageData = Data()
            isLoadingImage = false
            return
        }

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
                    // Validate/downsample before storing row state.
                    if await ImageMemoryCache.shared.image(from: data, url: url) != nil {
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

private struct InlineThinkingSurface: View {
    let message: ChatMessage
    let onOpenTimeline: () -> Void

    var body: some View {
        Button(action: onOpenTimeline) {
            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 6) {
                    Image(systemName: "brain")
                        .font(.caption)
                        .foregroundStyle(message.thinkingDone ? Theme.textMuted : Theme.accent)
                        .symbolEffect(.pulse, options: message.thinkingDone ? .nonRepeating : .repeating)

                    Text(message.thinkingDone ? "Latest thought" : "Thinking")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(message.thinkingDone ? Theme.textSecondary : Theme.accent)

                    Spacer()

                    Image(systemName: "chevron.up.forward")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(Theme.textMuted)
                }

                (Text(message.content) + (message.thinkingDone ? Text("") : Text(" ▍").foregroundColor(Theme.accent)))
                    .font(.caption)
                    .foregroundStyle(Theme.textSecondary)
                    .lineLimit(message.thinkingDone ? 2 : 4)
                    .multilineTextAlignment(.leading)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .padding(10)
            .background(Theme.backgroundSecondary.opacity(0.96))
            .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .stroke(message.thinkingDone ? Theme.border : Theme.accent.opacity(0.35), lineWidth: 0.5)
            )
        }
        .buttonStyle(.plain)
    }
}

private struct ThinkingBubble: View {
    let message: ChatMessage
    @State private var isExpanded: Bool = true
    @State private var elapsedSeconds: Int = 0
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

    /// Stable, chronological thought rows. Completed rows are deduplicated,
    /// but they do not move between separate active/completed sections when
    /// streaming finishes.
    private var visibleThoughts: [ChatMessage] {
        var seen = Set<String>()
        return thinkingMessages.filter { msg in
            let trimmed = msg.content.trimmingCharacters(in: .whitespacesAndNewlines)
            guard msg.thinkingDone else { return true }
            guard !trimmed.isEmpty else { return false }
            guard !seen.contains(trimmed) else { return false }
            seen.insert(trimmed)
            return true
        }
    }

    private var hasActiveThinking: Bool {
        visibleThoughts.contains { !$0.thinkingDone }
    }

    /// Count aligned with what is actually rendered in the sheet.
    private var visibleThoughtCount: Int {
        visibleThoughts.count
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
                    ScrollViewReader { proxy in
                        ScrollView {
                            LazyVStack(spacing: 14) {
                                ForEach(Array(visibleThoughts.enumerated()), id: \.element.id) { index, msg in
                                    ThoughtTimelineRow(
                                        message: msg,
                                        emphasize: !msg.thinkingDone,
                                        isLatest: index == visibleThoughts.count - 1
                                    )
                                    .id(msg.id)
                                    .accessibilityIdentifier(index == visibleThoughts.count - 1 ? "thought-latest" : "thought-row")
                                }
                                Color.clear
                                    .frame(height: 1)
                                    .id("thoughts-bottom")
                                    .accessibilityIdentifier("thoughts-bottom")
                            }
                            .padding()
                        }
                        .accessibilityIdentifier("thoughts-timeline")
                        .onAppear {
                            scrollToLatestThought(proxy)
                        }
                        .onChange(of: visibleThoughtCount) { _, _ in
                            scrollToLatestThought(proxy)
                        }
                        .onChange(of: hasActiveThinking) { _, _ in
                            scrollToLatestThought(proxy)
                        }
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

    private func scrollToLatestThought(_ proxy: ScrollViewProxy) {
        guard hasVisibleThoughts else { return }
        Task { @MainActor in
            try? await Task.sleep(nanoseconds: 80_000_000)
            withAnimation(.snappy(duration: 0.2)) {
                proxy.scrollTo("thoughts-bottom", anchor: .bottom)
            }
        }
    }
}

private struct ThoughtTimelineRow: View {
    let message: ChatMessage
    let emphasize: Bool
    let isLatest: Bool
    @State private var isExpanded: Bool
    @State private var elapsedSeconds: Int = 0
    @State private var timerTask: Task<Void, Never>?

    init(message: ChatMessage, emphasize: Bool, isLatest: Bool) {
        self.message = message
        self.emphasize = emphasize
        self.isLatest = isLatest
        _isExpanded = State(initialValue: emphasize || isLatest)
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
        .onChange(of: isLatest) { _, latest in
            if latest {
                withAnimation(.spring(duration: 0.2)) {
                    isExpanded = true
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
    @State private var derivedMissionById: [String: Mission] = [:]
    @State private var derivedFilteredRunning: [RunningMissionInfo] = []
    @State private var derivedFilteredRecent: [Mission] = []
    @State private var derivedOrderedRunning: [RunningRow] = []
    @State private var derivedJustCompletedMissions: [Mission] = []
    @State private var derivedRecentMissionsForList: [Mission] = []

    private let backendSearchDebounceNanos: UInt64 = 250_000_000

    private var normalizedSearchQuery: String {
        normalizeMetadataText(searchText)
    }

    private func preferredMissionForDuplicateId(_ lhs: Mission, _ rhs: Mission) -> Mission {
        let lhsUpdated = lhs.updatedDate ?? .distantPast
        let rhsUpdated = rhs.updatedDate ?? .distantPast
        return rhsUpdated >= lhsUpdated ? rhs : lhs
    }

    /// A running row carries layout hints so we can render boss + nested
    /// workers without losing the underlying `RunningMissionInfo`.
    private struct RunningRow: Identifiable {
        let info: RunningMissionInfo
        let isBoss: Bool
        /// Non-nil when this row should render indented under a boss. The id
        /// references the boss mission for visual continuity only.
        let nestedUnder: String?

        var id: String { info.missionId }
    }

    private var missionListSignature: String {
        let runningPart = runningMissions
            .map { "\($0.missionId):\($0.state):\($0.title ?? ""):\($0.currentActivity ?? "")" }
            .joined(separator: "|")
        let recentPart = recentMissions
            .map {
                "\($0.id):\($0.status.displayLabel):\($0.updatedDate?.timeIntervalSince1970 ?? 0):\($0.parentMissionId ?? ""):\($0.title ?? ""):\($0.shortDescription ?? ""):\($0.backend ?? "")"
            }
            .joined(separator: "|")
        let backendPart = backendSearchResults
            .map { "\($0.mission.id):\($0.relevanceScore)" }
            .joined(separator: "|")
        return [
            runningPart,
            recentPart,
            searchText,
            backendSearchQuery,
            backendPart
        ].joined(separator: "||")
    }

    private func bossWorkerIds(from missions: [Mission]) -> [String: [String]] {
        var map: [String: [String]] = [:]
        for mission in missions {
            if let parent = mission.parentMissionId, !parent.isEmpty {
                map[parent, default: []].append(mission.id)
            }
        }
        return map
    }

    private func orderedRunningRows(
        filtered: [RunningMissionInfo],
        missionById: [String: Mission],
        workerIdsByBoss: [String: [String]]
    ) -> [RunningRow] {
        let filteredById: [String: RunningMissionInfo] = Dictionary(
            uniqueKeysWithValues: filtered.map { ($0.missionId, $0) }
        )

        var rows: [RunningRow] = []
        var seen = Set<String>()

        // Phase 1: bosses (with their running workers nested directly under).
        for info in filtered where workerIdsByBoss[info.missionId] != nil {
            guard seen.insert(info.missionId).inserted else { continue }
            rows.append(RunningRow(info: info, isBoss: true, nestedUnder: nil))
            for workerId in workerIdsByBoss[info.missionId] ?? [] {
                guard !seen.contains(workerId),
                      let workerInfo = filteredById[workerId]
                else { continue }
                seen.insert(workerId)
                rows.append(RunningRow(info: workerInfo, isBoss: false, nestedUnder: info.missionId))
            }
        }

        // Phase 2: standalone running (no workers, not a worker itself).
        for info in filtered {
            guard !seen.contains(info.missionId) else { continue }
            let mission = missionById[info.missionId]
            if mission?.parentMissionId == nil {
                seen.insert(info.missionId)
                rows.append(RunningRow(info: info, isBoss: false, nestedUnder: nil))
            }
        }

        // Phase 3: orphan workers — running, but their boss isn't. Render them
        // indented so the worker identity is still obvious.
        for info in filtered {
            guard !seen.contains(info.missionId) else { continue }
            seen.insert(info.missionId)
            let parentId = missionById[info.missionId]?.parentMissionId
            rows.append(RunningRow(info: info, isBoss: false, nestedUnder: parentId))
        }

        return rows
    }

    private func recomputeMissionSections() {
        let query = normalizedSearchQuery
        let runningIds = Set(runningMissions.map { $0.missionId })
        let missionById = Dictionary(
            recentMissions.map { ($0.id, $0) },
            uniquingKeysWith: preferredMissionForDuplicateId
        )

        let liveCandidates = runningMissions.filter { info in
            guard let mission = missionById[info.missionId] else { return true }
            return !mission.hasFinishedSuccessfully
        }
        let filteredRunning: [RunningMissionInfo]
        if query.isEmpty {
            filteredRunning = liveCandidates
        } else {
            filteredRunning = liveCandidates
                .compactMap { info -> (RunningMissionInfo, Double)? in
                    let score = runningMissionSearchScore(
                        info,
                        query: query,
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

        let nonRunning = recentMissions.filter { !runningIds.contains($0.id) }
        let filteredRecent: [Mission]
        if query.isEmpty {
            filteredRecent = nonRunning
        } else {
            let localMatches: [Mission] = nonRunning
                .compactMap { mission -> (Mission, Double)? in
                    let score = missionSearchRelevanceScore(mission, query: query)
                    return score > 0 ? (mission, score) : nil
                }
                .sorted { lhs, rhs in
                    if lhs.1 == rhs.1 {
                        return (lhs.0.updatedDate ?? .distantPast) > (rhs.0.updatedDate ?? .distantPast)
                    }
                    return lhs.1 > rhs.1
                }
                .map(\.0)

            if backendSearchQuery == query {
                let byId = Dictionary(
                    nonRunning.map { ($0.id, $0) },
                    uniquingKeysWith: preferredMissionForDuplicateId
                )
                var merged: [Mission] = []
                var seen = Set<String>()

                for result in backendSearchResults {
                    let mission = byId[result.mission.id] ?? result.mission
                    guard !runningIds.contains(mission.id) else { continue }
                    if seen.insert(mission.id).inserted {
                        merged.append(mission)
                    }
                }

                for mission in localMatches {
                    if seen.insert(mission.id).inserted {
                        merged.append(mission)
                    }
                }

                filteredRecent = merged
            } else {
                filteredRecent = localMatches
            }
        }

        let cutoff = Date().addingTimeInterval(-24 * 60 * 60)
        let justCompletedMissions = query.isEmpty
            ? recentMissions
                .filter { mission in
                    guard !runningIds.contains(mission.id) else { return false }
                    switch mission.status {
                    case .completed, .acknowledged, .awaitingUser:
                        return true
                    default:
                        return false
                    }
                }
                .filter { ($0.updatedDate ?? .distantPast) >= cutoff }
                .prefix(5)
                .map { $0 }
            : []
        let justCompletedIds = Set(justCompletedMissions.map(\.id))

        derivedMissionById = missionById
        derivedFilteredRunning = filteredRunning
        derivedFilteredRecent = filteredRecent
        derivedOrderedRunning = orderedRunningRows(
            filtered: filteredRunning,
            missionById: missionById,
            workerIdsByBoss: bossWorkerIds(from: recentMissions)
        )
        derivedJustCompletedMissions = justCompletedMissions
        derivedRecentMissionsForList = filteredRecent.filter { !justCompletedIds.contains($0.id) }
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
                        isWorker: mission.parentMissionId != nil,
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

                // Running missions — boss + nested workers, then standalone.
                if !derivedOrderedRunning.isEmpty {
                    Section("Running") {
                        ForEach(derivedOrderedRunning) { row in
                            let info = row.info
                            let mission = derivedMissionById[info.missionId]
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
                                isWorker: row.nestedUnder != nil,
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

                missionSection("Just Completed", missions: derivedJustCompletedMissions)
                missionSection("Recent", missions: derivedRecentMissionsForList)

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

                if derivedFilteredRunning.isEmpty && derivedFilteredRecent.isEmpty && !normalizedSearchQuery.isEmpty {
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
                recomputeMissionSections()
            }
            .onAppear {
                recomputeMissionSections()
                scheduleBackendSearch(for: searchText)
            }
            .onChange(of: missionListSignature) { _, _ in
                recomputeMissionSections()
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
    /// When true the row renders indented with a small "W" badge so workers
    /// read as visually subordinate to their boss row.
    var isWorker: Bool = false
    let quickActions: [MissionQuickAction]
    let onSelect: () -> Void
    let onQuickAction: ((MissionQuickAction) -> Void)?
    let onCancel: (() -> Void)?

    private var shortId: String {
        String(missionId.prefix(8))
    }

    private var statusColor: Color {
        if isRunning {
            return Theme.accent
        }
        switch status {
        case .pending: return Theme.warning
        case .active: return Theme.accent
        case .awaitingUser: return Theme.warning
        case .acknowledged: return Theme.success
        case .completed: return Theme.success
        case .failed: return Theme.error
        case .interrupted, .blocked: return Theme.error
        case .notFeasible: return Theme.error
        case .unknown: return Theme.textMuted
        }
    }

    private var statusIcon: String {
        if isRunning {
            return "arrow.trianglehead.2.clockwise"
        }
        switch status {
        case .pending: return "clock.fill"
        case .active: return "arrow.trianglehead.2.clockwise"
        case .awaitingUser: return "hand.wave.fill"
        case .acknowledged: return "checkmark.circle.fill"
        case .completed: return "checkmark.circle.fill"
        case .failed: return "xmark.circle.fill"
        case .interrupted: return "pause.circle.fill"
        case .blocked: return "exclamationmark.triangle.fill"
        case .notFeasible: return "xmark.circle.fill"
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
                    .foregroundStyle(Theme.info)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 4)
                    .background(Theme.info.opacity(0.12))
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
                if isWorker {
                    // Indent rail + "W" chip mirrors the cmd+K palette so
                    // workers read as nested under their boss row above.
                    HStack(spacing: 6) {
                        Rectangle()
                            .fill(Theme.info.opacity(0.5))
                            .frame(width: 2, height: 24)
                        Text("W")
                            .font(.caption2.weight(.bold))
                            .foregroundStyle(Theme.info)
                            .frame(width: 14, height: 14)
                            .background(Theme.info.opacity(0.15))
                            .clipShape(RoundedRectangle(cornerRadius: 3))
                    }
                }

                Group {
                    if isRunning {
                        ProgressView()
                            .progressViewStyle(.circular)
                            .controlSize(.small)
                            .tint(Theme.accent)
                    } else {
                        Image(systemName: statusIcon)
                            .font(.system(size: 18))
                            .foregroundStyle(statusColor)
                    }
                }
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

/// Concrete struct holding the main content stack, parameterised on the
/// dynamic bits ControlView needs to inject. Pulling this out of
/// `ControlView` lets SwiftUI's type-checker resolve it independently of
/// the toolbar + sheet + onChange chain on the parent body.
private struct MainContentStack<Banner: View, Pill: View, Messages: View, Worker: View, Input: View>: View {
    let showBanner: Bool
    let bannerView: Banner
    let showStaleCachePill: Bool
    let staleCachePill: Pill
    let messagesView: Messages
    let workerPill: Worker
    let inputView: Input

    var body: some View {
        VStack(spacing: 0) {
            if showBanner {
                bannerView
                    .transition(.move(edge: .top).combined(with: .opacity))
            }
            if showStaleCachePill {
                staleCachePill
                    .transition(.move(edge: .top).combined(with: .opacity))
            }
            ZStack(alignment: .bottom) {
                messagesView
                workerPill
            }
            inputView
        }
        .animation(.easeInOut(duration: 0.2), value: showBanner)
        .animation(.easeInOut(duration: 0.2), value: showStaleCachePill)
    }
}

/// Renders the connection-state banner. Extracted to a separate struct so
/// `ControlView.body` doesn't grow past the Swift type-checker's complexity
/// budget.
private struct ConnectionBannerView: View {
    let state: ConnectionState

    var body: some View {
        // Degraded is a softer signal than disconnect/reconnect; use the
        // standard textSecondary tone rather than warning so users on a
        // marginal cell don't get a red flag for every minor slowdown.
        let tint: Color = state.isDegraded ? Theme.textSecondary : Theme.warning
        return HStack(spacing: 8) {
            Image(systemName: state.icon)
                .font(.system(size: 11, weight: .semibold))
                .symbolEffect(.pulse, options: state.isDegraded ? .nonRepeating : .repeating)
            Text(state.label)
                .font(.caption.weight(.medium))
            Spacer()
        }
        .foregroundStyle(tint)
        .padding(.horizontal, 16)
        .padding(.vertical, 6)
        .background(tint.opacity(0.12))
        .overlay(alignment: .bottom) {
            Rectangle()
                .fill(tint.opacity(0.25))
                .frame(height: 0.5)
        }
    }
}

#Preview {
    NavigationStack {
        ControlView()
    }
}
