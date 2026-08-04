//
//  MissionSwitcherSheet.swift
//  SandboxedDashboard
//
//  Extracted from ControlView.swift (mechanical split, no behavior change)
//

import SwiftUI

enum MissionQuickAction: Hashable {
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
struct MissionSwitcherSheet: View {
    let runningMissions: [RunningMissionInfo]
    let recentMissions: [Mission]
    /// Hermes conversations. Empty when Hermes isn't adopted on this backend,
    /// which simply hides the section.
    var hermesSessions: [HermesSession] = []
    /// Projects board rows. Empty when the backend has no projects surface,
    /// which simply hides the section.
    var projects: [ProjectSummary] = []
    let currentMissionId: String?
    let viewingMissionId: String?
    var viewingHermesSessionId: String? = nil
    let onSelectMission: (String) -> Void
    var onSelectHermesSession: ((String) -> Void)? = nil
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

    /// Sessions matching the current search (title/preview only — Hermes has
    /// no server-side session search to defer to).
    private var filteredHermesSessions: [HermesSession] {
        guard !normalizedSearchQuery.isEmpty else { return hermesSessions }
        return hermesSessions.filter {
            normalizeMetadataText($0.displayTitle).contains(normalizedSearchQuery)
        }
    }

    /// Projects worth listing: only those with a declared conversation, since
    /// the row's whole purpose is to open one. A project whose conversation is
    /// merely inferred would hand the user a dead cron session.
    private var filteredProjects: [ProjectSummary] {
        let bound = projects.filter { $0.boundSessionId != nil }
        let matching = normalizedSearchQuery.isEmpty
            ? bound
            : bound.filter { normalizeMetadataText($0.slug).contains(normalizedSearchQuery) }
        // Projects needing attention first; the rest alphabetical so the order
        // does not shuffle between refreshes.
        return matching.sorted {
            if $0.needsAttention != $1.needsAttention { return $0.needsAttention }
            return $0.slug < $1.slug
        }
    }

    /// Missions this Hermes session spawned, rendered as its workers.
    private func hermesWorkers(for sessionId: String) -> [Mission] {
        recentMissions.filter { $0.originSessionId == sessionId }
    }

    /// Missions already rendered under a visible Hermes session. They are
    /// filtered out of Running / Just Completed / Recent so a session-spawned
    /// mission appears once, under its session — mirroring the web switcher's
    /// `addedIds` bookkeeping. Keyed on the *visible* sessions only: when a
    /// search hides the session, its workers stay findable in the normal
    /// sections.
    private var nestedHermesWorkerIds: Set<String> {
        guard !hermesSessions.isEmpty else { return [] }
        let visibleSessionIds = Set(filteredHermesSessions.map(\.id))
        guard !visibleSessionIds.isEmpty else { return [] }
        return Set(
            recentMissions.compactMap { mission in
                guard let origin = mission.originSessionId,
                    visibleSessionIds.contains(origin)
                else { return nil }
                return mission.id
            }
        )
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
                        ForEach(
                            derivedOrderedRunning.filter {
                                !nestedHermesWorkerIds.contains($0.info.missionId)
                            }
                        ) { row in
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

                // Projects that have a declared control conversation. Tapping
                // one opens that conversation — the same path as picking the
                // session directly, so there is no second way to be viewing a
                // conversation.
                if !filteredProjects.isEmpty {
                    Section("Projects") {
                        ForEach(filteredProjects) { project in
                            ProjectRow(
                                project: project,
                                isViewing: viewingHermesSessionId == project.boundSessionId,
                                onSelect: {
                                    guard let sessionId = project.boundSessionId else { return }
                                    onSelectHermesSession?(sessionId)
                                }
                            )
                        }
                    }
                }

                // Hermes conversations, with the missions each one spawned
                // nested under it as workers.
                if !filteredHermesSessions.isEmpty {
                    Section("Hermes Sessions") {
                        ForEach(filteredHermesSessions) { session in
                            HermesSessionRow(
                                session: session,
                                workerCount: hermesWorkers(for: session.id).count,
                                isViewing: viewingHermesSessionId == session.id,
                                onSelect: { onSelectHermesSession?(session.id) }
                            )
                            ForEach(hermesWorkers(for: session.id)) { worker in
                                MissionRow(
                                    missionId: worker.id,
                                    displayName: missionDisplayName(for: worker),
                                    title: worker.displayTitle,
                                    shortDescription: missionCardDescription(for: worker),
                                    backend: worker.backend,
                                    status: worker.status,
                                    isRunning: runningMissions.contains {
                                        $0.missionId == worker.id
                                    },
                                    runningState: nil,
                                    isViewing: viewingMissionId == worker.id,
                                    isWorker: true,
                                    quickActions: [],
                                    onSelect: { onSelectMission(worker.id) },
                                    onQuickAction: nil,
                                    onCancel: nil
                                )
                            }
                        }
                    }
                }

                missionSection(
                    "Just Completed",
                    missions: derivedJustCompletedMissions.filter {
                        !nestedHermesWorkerIds.contains($0.id)
                    }
                )
                missionSection(
                    "Recent",
                    missions: derivedRecentMissionsForList.filter {
                        !nestedHermesWorkerIds.contains($0.id)
                    }
                )

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
            // Keep the last rows readable above the bottom-pinned search
            // field (its translucent material let badges bleed through).
            .contentMargins(.bottom, 72, for: .scrollContent)
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

// MARK: - Hermes Session Row

/// One Hermes conversation in the switcher. Deliberately lighter than
/// `MissionRow`: a session has no status, workspace, or backend — only a
/// title, its message count, and how many missions it spawned.
private struct HermesSessionRow: View {
    let session: HermesSession
    let workerCount: Int
    let isViewing: Bool
    let onSelect: () -> Void

    var body: some View {
        Button(action: onSelect) {
            HStack(spacing: 10) {
                Image(systemName: "sparkles")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(Theme.accent)
                    .frame(width: 16)

                VStack(alignment: .leading, spacing: 2) {
                    Text(session.displayTitle)
                        .font(.system(size: 14))
                        .foregroundStyle(Theme.textPrimary)
                        .lineLimit(1)
                    Text(subtitle)
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.textTertiary)
                        .lineLimit(1)
                }

                Spacer(minLength: 4)

                if isViewing {
                    Image(systemName: "checkmark")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(Theme.accent)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    private var subtitle: String {
        var parts = ["Hermes session"]
        if let count = session.messageCount, count > 0 {
            parts.append("\(count) messages")
        }
        if workerCount > 0 {
            parts.append("\(workerCount) worker\(workerCount == 1 ? "" : "s")")
        }
        return parts.joined(separator: " · ")
    }
}

/// A project, opening its declared control conversation.
private struct ProjectRow: View {
    let project: ProjectSummary
    let isViewing: Bool
    let onSelect: () -> Void

    var body: some View {
        Button(action: onSelect) {
            HStack(spacing: 10) {
                Image(systemName: "square.stack.3d.up")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(project.needsAttention ? Theme.warning : Theme.accent)
                    .frame(width: 16)

                VStack(alignment: .leading, spacing: 2) {
                    Text(project.slug)
                        .font(.system(size: 14))
                        .foregroundStyle(Theme.textPrimary)
                        .lineLimit(1)
                    Text(subtitle)
                        .font(.system(size: 11))
                        .foregroundStyle(
                            project.needsAttention ? Theme.warning : Theme.textTertiary
                        )
                        .lineLimit(1)
                }

                Spacer(minLength: 4)

                if isViewing {
                    Image(systemName: "checkmark")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(Theme.accent)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    /// Leads with what is wrong, because that is the reason to tap.
    private var subtitle: String {
        var parts: [String] = []
        if let health = project.health {
            if health.tracksNeedingAttention > 0 {
                let n = health.tracksNeedingAttention
                parts.append("\(n) track\(n == 1 ? "" : "s") need attention")
            }
            if let worst = health.tracks.first, worst.verdict.needsAttention {
                parts.append("\(worst.displayTrack) \(worst.verdict.rawValue)")
            }
            if parts.isEmpty, health.active > 0 {
                parts.append("\(health.active) running")
            }
            if parts.isEmpty, health.missions > 0 {
                parts.append("\(health.missions) mission\(health.missions == 1 ? "" : "s")")
            }
        }
        if parts.isEmpty { parts.append("Project") }
        return parts.joined(separator: " · ")
    }
}
