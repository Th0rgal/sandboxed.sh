//
//  RunningMissionsBar.swift
//  SandboxedDashboard
//
//  Compact horizontal bar showing currently running missions
//  Allows switching between parallel missions
//

import SwiftUI

struct RunningMissionsBar: View {
    let runningMissions: [RunningMissionInfo]
    let currentMission: Mission?
    let viewingMissionId: String?
    let onSelectMission: (String) -> Void
    let onCancelMission: (String) -> Void
    let onRefresh: () -> Void
    
    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                // Header with refresh button
                headerView
                
                // Current mission if not in running list
                if let mission = currentMission,
                   !runningMissions.contains(where: { $0.missionId == mission.id }) {
                    currentMissionChip(mission)
                }
                
                // Running missions
                ForEach(runningMissions) { mission in
                    runningMissionChip(mission)
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
        }
        .background(.ultraThinMaterial)
    }
    
    // MARK: - Header
    
    private var headerView: some View {
        HStack(spacing: 6) {
            Image(systemName: "square.stack.3d.up")
                .font(.system(size: 12, weight: .medium))
                .foregroundStyle(Theme.textTertiary)
            
            Text("Running")
                .font(.caption.weight(.medium))
                .foregroundStyle(Theme.textTertiary)
            
            Text("(\(runningMissions.count))")
                .font(.caption)
                .foregroundStyle(Theme.textMuted)
            
            Button(action: onRefresh) {
                Image(systemName: "arrow.clockwise")
                    .font(.system(size: 10, weight: .medium))
                    .foregroundStyle(Theme.textMuted)
            }
            .padding(4)
            .contentShape(Rectangle())
        }
    }
    
    // MARK: - Current Mission Chip
    
    private func currentMissionChip(_ mission: Mission) -> some View {
        let isViewing = viewingMissionId == mission.id
        
        return Button {
            onSelectMission(mission.id)
        } label: {
            HStack(spacing: 6) {
                // Status dot
                Circle()
                    .fill(Theme.success)
                    .frame(width: 6, height: 6)
                
                // Mission ID
                Text(String(mission.id.prefix(8)).uppercased())
                    .font(.caption.weight(.medium))
                    .foregroundStyle(Theme.textPrimary)
                    .lineLimit(1)

                // Selection indicator
                if isViewing {
                    Image(systemName: "checkmark")
                        .font(.system(size: 9, weight: .bold))
                        .foregroundStyle(Theme.accent)
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(isViewing ? Theme.accent.opacity(0.15) : Color.white.opacity(0.05))
            .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .stroke(isViewing ? Theme.accent.opacity(0.3) : .clear, lineWidth: 1)
            )
        }
        .buttonStyle(.plain)
    }
    
    // MARK: - Running Mission Chip
    
    private func runningMissionChip(_ mission: RunningMissionInfo) -> some View {
        let isViewing = viewingMissionId == mission.missionId
        let isStalled = mission.isStalled
        // Only show severely stalled state for running missions
        let isSeverlyStalled = mission.isRunning && mission.secondsSinceActivity > 120
        
        let borderColor: Color = {
            if isViewing { return Theme.accent.opacity(0.3) }
            if isSeverlyStalled { return Theme.error.opacity(0.3) }
            if isStalled { return Theme.warning.opacity(0.3) }
            return Theme.border
        }()
        
        let backgroundColor: Color = {
            if isViewing { return Theme.accent.opacity(0.15) }
            if isSeverlyStalled { return Theme.error.opacity(0.1) }
            if isStalled { return Theme.warning.opacity(0.1) }
            return Color.white.opacity(0.05)
        }()
        
        return HStack(spacing: 6) {
            // Tap area for selection
            Button {
                onSelectMission(mission.missionId)
            } label: {
                HStack(spacing: 6) {
                    // Status dot with animation
                    Circle()
                        .fill(statusColor(for: mission))
                        .frame(width: 6, height: 6)
                        .overlay {
                            if mission.isRunning && !isStalled {
                                Circle()
                                    .stroke(statusColor(for: mission).opacity(0.5), lineWidth: 1.5)
                                    .frame(width: 10, height: 10)
                                    .opacity(0.6)
                            }
                        }

                    // Mission ID
                    Text(mission.shortId)
                        .font(.caption.weight(.medium))
                        .foregroundStyle(Theme.textPrimary)
                        .lineLimit(1)

                    // Queue indicator
                    if mission.queueLen > 0 {
                        Text("\(mission.queueLen)Q")
                            .font(.system(size: 9, weight: .medium).monospaced())
                            .foregroundStyle(Theme.warning)
                            .padding(.horizontal, 4)
                            .padding(.vertical, 1)
                            .background(Theme.warning.opacity(0.15))
                            .clipShape(RoundedRectangle(cornerRadius: 3, style: .continuous))
                    }

                    // Stalled indicator
                    if isStalled {
                        HStack(spacing: 2) {
                            Image(systemName: "exclamationmark.triangle.fill")
                                .font(.system(size: 8))
                            Text("\(mission.secondsSinceActivity)s")
                                .font(.system(size: 9).monospaced())
                        }
                        .foregroundStyle(isSeverlyStalled ? Theme.error : Theme.warning)
                    }

                    // Selection indicator
                    if isViewing {
                        Image(systemName: "checkmark")
                            .font(.system(size: 9, weight: .bold))
                            .foregroundStyle(Theme.accent)
                    }
                }
            }
            .buttonStyle(.plain)

            // Cancel button
            Button {
                onCancelMission(mission.missionId)
            } label: {
                Image(systemName: "xmark")
                    .font(.system(size: 9, weight: .medium))
                    .foregroundStyle(Theme.textMuted)
                    .frame(width: 18, height: 18)
                    .background(Color.white.opacity(0.08))
                    .clipShape(Circle())
            }
            .buttonStyle(.plain)
        }
        .padding(.leading, 12)
        .padding(.trailing, 6)
        .padding(.vertical, 8)
        .background(backgroundColor)
        .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .stroke(isViewing ? borderColor : .clear, lineWidth: 1)
        )
    }
    
    // MARK: - Helpers
    
    private func statusColor(for mission: RunningMissionInfo) -> Color {
        // Only show stalled/severely-stalled states for running missions
        if mission.isRunning && mission.secondsSinceActivity > 120 {
            return Theme.error
        } else if mission.isStalled {
            return Theme.warning
        } else if mission.isRunning {
            return Theme.success
        } else {
            return Theme.warning
        }
    }
}

// MARK: - Preview

#Preview {
    VStack(spacing: 0) {
        RunningMissionsBar(
            runningMissions: [
                RunningMissionInfo(
                    missionId: "abc12345-6789-0000-0000-000000000001",
                    state: "running",
                    queueLen: 0,
                    historyLen: 5,
                    secondsSinceActivity: 15,
                    expectedDeliverables: 0
                ),
                RunningMissionInfo(
                    missionId: "def12345-6789-0000-0000-000000000002",
                    state: "running",
                    queueLen: 1,
                    historyLen: 3,
                    secondsSinceActivity: 75,
                    expectedDeliverables: 0
                ),
                RunningMissionInfo(
                    missionId: "ghi12345-6789-0000-0000-000000000003",
                    state: "running",
                    queueLen: 0,
                    historyLen: 10,
                    secondsSinceActivity: 150,
                    expectedDeliverables: 0
                )
            ],
            currentMission: nil,
            viewingMissionId: "abc12345-6789-0000-0000-000000000001",
            onSelectMission: { _ in },
            onCancelMission: { _ in },
            onRefresh: {}
        )
        
        Spacer()
    }
    .background(Theme.backgroundPrimary)
}
