//
//  HistoryView.swift
//  SandboxedDashboard
//
//  Mission history list with search and filtering
//

import SwiftUI

struct HistoryView: View {
    @State private var missions: [Mission] = []
    @State private var tasks: [TaskState] = []
    @State private var runs: [Run] = []
    @State private var isLoading = true
    @State private var searchText = ""
    @State private var selectedFilter: StatusFilter = .all
    @State private var errorMessage: String?
    @State private var isCleaningUp = false
    @State private var showCleanupResult: String?

    private let api = APIService.shared
    private let nav = NavigationState.shared
    
    enum StatusFilter: String, CaseIterable {
        case all = "All"
        case active = "Active"
        case interrupted = "Interrupted"
        case completed = "Completed"
        case failed = "Failed"
        
        var missionStatuses: [MissionStatus]? {
            switch self {
            case .all: return nil
            case .active: return [.active]
            case .interrupted: return [.interrupted, .blocked]
            case .completed: return [.completed]
            case .failed: return [.failed, .notFeasible]
            }
        }
    }
    
    private var filteredMissions: [Mission] {
        missions.filter { mission in
            // Filter by status
            if let statuses = selectedFilter.missionStatuses, !statuses.contains(mission.status) {
                return false
            }
            
            // Filter by search
            if !searchText.isEmpty {
                let title = mission.title ?? ""
                if !title.localizedCaseInsensitiveContains(searchText) {
                    return false
                }
            }
            
            return true
        }
        .sorted { ($0.updatedDate ?? Date.distantPast) > ($1.updatedDate ?? Date.distantPast) }
    }
    
    var body: some View {
        ZStack {
            Theme.backgroundPrimary.ignoresSafeArea()
            
            VStack(spacing: 0) {
                // Search and filter
                VStack(spacing: 12) {
                    // Search bar
                    HStack(spacing: 10) {
                        Image(systemName: "magnifyingglass")
                            .foregroundStyle(Theme.textTertiary)
                        
                        TextField("Search missions...", text: $searchText)
                            .textFieldStyle(.plain)
                    }
                    .padding(.horizontal, 14)
                    .padding(.vertical, 12)
                    .background(.ultraThinMaterial)
                    .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                    .overlay(
                        RoundedRectangle(cornerRadius: 12, style: .continuous)
                            .stroke(Theme.border, lineWidth: 1)
                    )
                    
                    // Filter pills
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 8) {
                            ForEach(StatusFilter.allCases, id: \.rawValue) { filter in
                                FilterPill(
                                    title: filter.rawValue,
                                    isSelected: selectedFilter == filter
                                ) {
                                    withAnimation(.easeInOut(duration: 0.2)) {
                                        selectedFilter = filter
                                    }
                                    HapticService.selectionChanged()
                                }
                            }
                        }
                    }
                }
                .padding()

                // Cleanup result banner
                if let result = showCleanupResult {
                    HStack {
                        Image(systemName: "checkmark.circle.fill")
                            .foregroundStyle(Theme.success)
                        Text(result)
                            .font(.subheadline)
                            .foregroundStyle(Theme.textPrimary)
                        Spacer()
                        Button {
                            withAnimation { showCleanupResult = nil }
                        } label: {
                            Image(systemName: "xmark")
                                .font(.caption)
                                .foregroundStyle(Theme.textTertiary)
                        }
                    }
                    .padding()
                    .background(Theme.success.opacity(0.1))
                    .clipShape(RoundedRectangle(cornerRadius: 10))
                    .padding(.horizontal)
                    .transition(.move(edge: .top).combined(with: .opacity))
                }
                
                // Content with floating cleanup button
                ZStack(alignment: .bottomTrailing) {
                    if isLoading {
                        LoadingView(message: "Loading history...")
                    } else if let error = errorMessage {
                        EmptyStateView(
                            icon: "exclamationmark.triangle",
                            title: "Failed to Load",
                            message: error,
                            action: { Task { await loadData() } },
                            actionLabel: "Retry"
                        )
                    } else if filteredMissions.isEmpty && tasks.isEmpty {
                        EmptyStateView(
                            icon: "clock.arrow.circlepath",
                            title: "No History",
                            message: "Your missions will appear here"
                        )
                    } else {
                        missionsList
                    }

                    // Floating cleanup button
                    Button {
                        Task { await cleanupEmptyMissions() }
                    } label: {
                        Group {
                            if isCleaningUp {
                                ProgressView()
                                    .scaleEffect(0.8)
                                    .tint(.white)
                            } else {
                                Image(systemName: "sparkles")
                                    .font(.body.weight(.medium))
                            }
                        }
                        .foregroundStyle(.white)
                        .frame(width: 48, height: 48)
                        .background(Theme.accent)
                        .clipShape(Circle())
                        .shadow(color: Theme.accent.opacity(0.4), radius: 8, x: 0, y: 4)
                    }
                    .disabled(isCleaningUp)
                    .opacity(isCleaningUp ? 0.7 : 1)
                    .padding(.trailing, 20)
                    .padding(.bottom, 20)
                }
            }
        }
        .navigationTitle("History")
        .navigationBarTitleDisplayMode(.inline)
        .task {
            await loadData()
        }
        .refreshable {
            await loadData()
        }
    }
    
    private var missionsList: some View {
        ScrollView {
            LazyVStack(spacing: 12) {
                // Missions section
                if !filteredMissions.isEmpty {
                    Section {
                        ForEach(filteredMissions) { mission in
                            Button {
                                nav.openMission(mission.id)
                            } label: {
                                MissionRow(mission: mission)
                            }
                            .buttonStyle(.plain)
                            .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                                if mission.status != .active {
                                    Button(role: .destructive) {
                                        Task { await deleteMission(mission) }
                                    } label: {
                                        Label("Delete", systemImage: "trash")
                                    }
                                }
                            }
                        }
                    } header: {
                        SectionHeader(
                            title: "Missions",
                            count: filteredMissions.count
                        )
                    }
                }
                
                // Active tasks section
                if !tasks.isEmpty {
                    Section {
                        ForEach(tasks) { task in
                            TaskRow(task: task)
                        }
                    } header: {
                        SectionHeader(
                            title: "Active Tasks",
                            count: tasks.count
                        )
                    }
                }
                
                // Archived runs section
                if !runs.isEmpty {
                    Section {
                        ForEach(runs) { run in
                            RunRow(run: run)
                        }
                    } header: {
                        SectionHeader(
                            title: "Archived Runs",
                            count: runs.count
                        )
                    }
                }
            }
            .padding()
        }
    }
    
    private func loadData() async {
        isLoading = true
        errorMessage = nil

        do {
            async let missionsTask = api.listMissions()
            async let tasksTask = api.listTasks()
            async let runsTask = api.listRuns()

            let (missionsResult, tasksResult, runsResult) = try await (missionsTask, tasksTask, runsTask)

            missions = missionsResult
            tasks = tasksResult
            runs = runsResult
        } catch {
            errorMessage = error.localizedDescription
        }

        isLoading = false
    }

    private func deleteMission(_ mission: Mission) async {
        do {
            _ = try await api.deleteMission(id: mission.id)
            withAnimation {
                missions.removeAll { $0.id == mission.id }
            }
            HapticService.success()
        } catch {
            HapticService.error()
            errorMessage = "Failed to delete mission: \(error.localizedDescription)"
        }
    }

    private func cleanupEmptyMissions() async {
        isCleaningUp = true

        do {
            let count = try await api.cleanupEmptyMissions()
            if count > 0 {
                // Refresh the list
                let newMissions = try await api.listMissions()
                withAnimation {
                    missions = newMissions
                    showCleanupResult = "Cleaned up \(count) empty mission\(count == 1 ? "" : "s")"
                }
                HapticService.success()
            } else {
                withAnimation {
                    showCleanupResult = "No empty missions to clean up"
                }
            }

            // Auto-hide the result after 3 seconds
            Task {
                try? await Task.sleep(for: .seconds(3))
                withAnimation {
                    showCleanupResult = nil
                }
            }
        } catch {
            HapticService.error()
            errorMessage = "Cleanup failed: \(error.localizedDescription)"
        }

        isCleaningUp = false
    }
}

// MARK: - Supporting Views

private struct SectionHeader: View {
    let title: String
    let count: Int
    
    var body: some View {
        HStack {
            Text(title.uppercased())
                .font(.caption.weight(.semibold))
                .foregroundStyle(Theme.textTertiary)
            
            Text("(\(count))")
                .font(.caption)
                .foregroundStyle(Theme.textMuted)
            
            Spacer()
        }
        .padding(.bottom, 4)
    }
}

private struct FilterPill: View {
    let title: String
    let isSelected: Bool
    let action: () -> Void
    
    var body: some View {
        Button(action: action) {
            Text(title)
                .font(.subheadline.weight(.medium))
                .foregroundStyle(isSelected ? .white : Theme.textSecondary)
                .padding(.horizontal, 16)
                .padding(.vertical, 8)
                .background(isSelected ? Theme.accent : Color.white.opacity(0.05))
                .clipShape(Capsule())
                .overlay(
                    Capsule()
                        .stroke(isSelected ? .clear : Theme.border, lineWidth: 1)
                )
        }
        .buttonStyle(.plain)
    }
}

private struct MissionRow: View {
    let mission: Mission
    
    private var backendColor: Color {
        switch mission.backend {
        case "opencode": return Theme.success
        case "claudecode": return Theme.accent
        case "amp": return .orange
        default: return Theme.accent
        }
    }
    
    private var backendIcon: String {
        switch mission.backend {
        case "opencode": return "terminal"
        case "claudecode": return "brain"
        case "amp": return "bolt.fill"
        default: return "target"
        }
    }
    
    var body: some View {
        HStack(spacing: 14) {
            // Icon based on backend
            Image(systemName: mission.canResume ? "play.circle" : backendIcon)
                .font(.title3)
                .foregroundStyle(mission.canResume ? Theme.warning : backendColor)
                .frame(width: 40, height: 40)
                .background((mission.canResume ? Theme.warning : backendColor).opacity(0.15))
                .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
            
            // Content
            VStack(alignment: .leading, spacing: 4) {
                Text(mission.displayTitle)
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(Theme.textPrimary)
                    .lineLimit(1)
                
                HStack(spacing: 6) {
                    StatusBadge(status: mission.status.statusType, compact: true)
                    
                    // Show agent if available
                    if let agent = mission.agent, !agent.isEmpty {
                        Text(agent)
                            .font(.caption2.weight(.medium))
                            .foregroundStyle(backendColor)
                            .padding(.horizontal, 6)
                            .padding(.vertical, 2)
                            .background(backendColor.opacity(0.15))
                            .clipShape(RoundedRectangle(cornerRadius: 4))
                    }

                    Text("\(mission.history.count) msg")
                        .font(.caption)
                        .foregroundStyle(Theme.textTertiary)
                        .fixedSize()
                }
            }
            
            Spacer()
            
            // Timestamp and chevron
            VStack(alignment: .trailing, spacing: 4) {
                if let date = mission.updatedDate {
                    Text(date.relativeFormatted)
                        .font(.caption)
                        .foregroundStyle(Theme.textTertiary)
                }
                
                Image(systemName: "chevron.right")
                    .font(.caption)
                    .foregroundStyle(Theme.textMuted)
            }
        }
        .padding(14)
        .background(.ultraThinMaterial)
        .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .stroke(mission.canResume ? Theme.warning.opacity(0.3) : Theme.border, lineWidth: mission.canResume ? 1 : 0.5)
        )
    }
}

private struct TaskRow: View {
    let task: TaskState
    
    var body: some View {
        HStack(spacing: 14) {
            VStack(alignment: .leading, spacing: 4) {
                Text(task.task)
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(Theme.textPrimary)
                    .lineLimit(2)
                
                HStack(spacing: 8) {
                    StatusBadge(status: task.status.statusType, compact: true)
                    
                    Text(task.displayModel)
                        .font(.caption.monospaced())
                        .foregroundStyle(Theme.textTertiary)
                    
                    Text("•")
                        .foregroundStyle(Theme.textMuted)
                    
                    Text("\(task.iterations) iterations")
                        .font(.caption)
                        .foregroundStyle(Theme.textTertiary)
                }
            }
            
            Spacer()
        }
        .padding(14)
        .background(.ultraThinMaterial)
        .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .stroke(Theme.border, lineWidth: 0.5)
        )
    }
}

private struct RunRow: View {
    let run: Run
    
    var body: some View {
        HStack(spacing: 14) {
            VStack(alignment: .leading, spacing: 4) {
                Text(run.inputText)
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(Theme.textPrimary)
                    .lineLimit(2)
                
                HStack(spacing: 8) {
                    if let date = run.createdDate {
                        Text(date.relativeFormatted)
                            .font(.caption)
                            .foregroundStyle(Theme.textTertiary)
                    }
                    
                    Text("•")
                        .foregroundStyle(Theme.textMuted)
                    
                    Text(String(format: "$%.2f", run.costDollars))
                        .font(.caption.monospaced())
                        .foregroundStyle(Theme.success)
                }
            }
            
            Spacer()
        }
        .padding(14)
        .background(.ultraThinMaterial)
        .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .stroke(Theme.border, lineWidth: 0.5)
        )
    }
}

// MARK: - Mission Detail View

struct MissionDetailView: View {
    let mission: Mission
    
    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 16) {
                // Header
                VStack(alignment: .leading, spacing: 8) {
                    HStack {
                        StatusBadge(status: mission.status.statusType)
                        Spacer()
                        if let date = mission.updatedDate {
                            Text(date.formatted(date: .abbreviated, time: .shortened))
                                .font(.caption)
                                .foregroundStyle(Theme.textTertiary)
                        }
                    }
                    
                    Text(mission.title ?? "Untitled Mission")
                        .font(.title3.bold())
                        .foregroundStyle(Theme.textPrimary)
                }
                .padding()
                .background(.ultraThinMaterial)
                .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
                
                // Messages
                if !mission.history.isEmpty {
                    ForEach(mission.history) { entry in
                        HStack(alignment: .top, spacing: 12) {
                            Image(systemName: entry.isUser ? "person.circle.fill" : "sparkles")
                                .foregroundStyle(entry.isUser ? Theme.accent : Theme.textSecondary)
                            
                            Text(entry.content)
                                .font(.body)
                                .foregroundStyle(Theme.textPrimary)
                        }
                        .padding()
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(.ultraThinMaterial.opacity(entry.isUser ? 0.8 : 0.4))
                        .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
                    }
                }
            }
            .padding()
        }
        .background(Theme.backgroundPrimary.ignoresSafeArea())
        .navigationTitle("Mission")
        .navigationBarTitleDisplayMode(.inline)
    }
}

#Preview {
    NavigationStack {
        HistoryView()
    }
}
