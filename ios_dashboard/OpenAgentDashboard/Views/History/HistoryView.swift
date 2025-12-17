//
//  HistoryView.swift
//  OpenAgentDashboard
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
    
    private let api = APIService.shared
    private let nav = NavigationState.shared
    
    enum StatusFilter: String, CaseIterable {
        case all = "All"
        case active = "Active"
        case completed = "Completed"
        case failed = "Failed"
        
        var missionStatus: MissionStatus? {
            switch self {
            case .all: return nil
            case .active: return .active
            case .completed: return .completed
            case .failed: return .failed
            }
        }
    }
    
    private var filteredMissions: [Mission] {
        missions.filter { mission in
            // Filter by status
            if let statusFilter = selectedFilter.missionStatus, mission.status != statusFilter {
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
                
                // Content
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
    
    var body: some View {
        HStack(spacing: 14) {
            // Icon
            Image(systemName: "target")
                .font(.title3)
                .foregroundStyle(Theme.accent)
                .frame(width: 40, height: 40)
                .background(Theme.accent.opacity(0.15))
                .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
            
            // Content
            VStack(alignment: .leading, spacing: 4) {
                Text(mission.displayTitle)
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(Theme.textPrimary)
                    .lineLimit(1)
                
                HStack(spacing: 8) {
                    StatusBadge(status: mission.status.statusType, compact: true)
                    
                    Text("\(mission.history.count) messages")
                        .font(.caption)
                        .foregroundStyle(Theme.textTertiary)
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
                .stroke(Theme.border, lineWidth: 0.5)
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
