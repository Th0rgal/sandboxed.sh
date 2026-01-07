//
//  NewMissionSheet.swift
//  OpenAgentDashboard
//
//  Sheet for creating a new mission with workspace selection
//

import SwiftUI

struct NewMissionSheet: View {
    let workspaces: [Workspace]
    @Binding var selectedWorkspaceId: String?
    let onCreate: (String?) -> Void
    let onCancel: () -> Void

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                // Header
                VStack(spacing: 8) {
                    Image(systemName: "plus.circle.fill")
                        .font(.system(size: 48))
                        .foregroundStyle(Theme.accent)

                    Text("New Mission")
                        .font(.title2.weight(.semibold))
                        .foregroundStyle(Theme.textPrimary)

                    Text("Select a workspace for execution")
                        .font(.subheadline)
                        .foregroundStyle(Theme.textSecondary)
                }
                .padding(.top, 24)
                .padding(.bottom, 32)

                // Workspace list
                ScrollView {
                    VStack(spacing: 12) {
                        if workspaces.isEmpty {
                            VStack(spacing: 8) {
                                Image(systemName: "server.rack")
                                    .font(.system(size: 32))
                                    .foregroundStyle(Theme.textMuted)
                                Text("No workspaces available")
                                    .font(.subheadline)
                                    .foregroundStyle(Theme.textSecondary)
                            }
                            .padding(.vertical, 32)
                        } else {
                            ForEach(workspaces) { workspace in
                                WorkspaceRow(
                                    workspace: workspace,
                                    isSelected: selectedWorkspaceId == workspace.id,
                                    onSelect: {
                                        selectedWorkspaceId = workspace.id
                                        HapticService.selectionChanged()
                                    }
                                )
                            }
                        }
                    }
                    .padding(.horizontal, 16)
                }

                Spacer()

                // Action buttons
                VStack(spacing: 12) {
                    Button {
                        onCreate(selectedWorkspaceId)
                    } label: {
                        HStack {
                            Image(systemName: "play.fill")
                            Text("Start Mission")
                        }
                        .font(.body.weight(.semibold))
                        .foregroundStyle(.white)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 14)
                        .background(Theme.accent)
                        .clipShape(RoundedRectangle(cornerRadius: 12))
                    }
                    .disabled(workspaces.isEmpty)

                    Button {
                        onCancel()
                    } label: {
                        Text("Cancel")
                            .font(.body)
                            .foregroundStyle(Theme.textSecondary)
                    }
                }
                .padding(.horizontal, 16)
                .padding(.bottom, 24)
            }
            .background(Theme.backgroundSecondary)
        }
    }
}

struct WorkspaceRow: View {
    let workspace: Workspace
    let isSelected: Bool
    let onSelect: () -> Void

    var body: some View {
        Button(action: onSelect) {
            HStack(spacing: 12) {
                // Icon
                ZStack {
                    Circle()
                        .fill(workspace.workspaceType == .host ? Theme.success.opacity(0.15) : Theme.accent.opacity(0.15))
                        .frame(width: 40, height: 40)

                    Image(systemName: workspace.workspaceType.icon)
                        .font(.system(size: 16, weight: .medium))
                        .foregroundStyle(workspace.workspaceType == .host ? Theme.success : Theme.accent)
                }

                // Info
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text(workspace.name)
                            .font(.body.weight(.medium))
                            .foregroundStyle(Theme.textPrimary)

                        if workspace.isDefault {
                            Text("Default")
                                .font(.caption2.weight(.medium))
                                .foregroundStyle(Theme.textSecondary)
                                .padding(.horizontal, 6)
                                .padding(.vertical, 2)
                                .background(Theme.backgroundTertiary)
                                .clipShape(RoundedRectangle(cornerRadius: 4))
                        }
                    }

                    Text(workspace.shortDescription)
                        .font(.caption)
                        .foregroundStyle(Theme.textSecondary)
                }

                Spacer()

                // Selection indicator
                ZStack {
                    Circle()
                        .stroke(isSelected ? Theme.accent : Theme.borderSubtle, lineWidth: 2)
                        .frame(width: 22, height: 22)

                    if isSelected {
                        Circle()
                            .fill(Theme.accent)
                            .frame(width: 14, height: 14)
                    }
                }
            }
            .padding(12)
            .background(
                RoundedRectangle(cornerRadius: 12)
                    .fill(isSelected ? Theme.accent.opacity(0.08) : Theme.backgroundTertiary)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 12)
                    .stroke(isSelected ? Theme.accent.opacity(0.3) : Color.clear, lineWidth: 1)
            )
        }
        .buttonStyle(.plain)
    }
}

#Preview {
    NewMissionSheet(
        workspaces: [
            Workspace(
                id: "00000000-0000-0000-0000-000000000000",
                name: "host",
                workspaceType: .host,
                path: "/root",
                status: .ready,
                errorMessage: nil,
                createdAt: "2025-01-05T12:00:00Z"
            ),
            Workspace(
                id: "1",
                name: "project-a",
                workspaceType: .chroot,
                path: "/var/lib/openagent/chroots/project-a",
                status: .ready,
                errorMessage: nil,
                createdAt: "2025-01-05T12:00:00Z"
            )
        ],
        selectedWorkspaceId: .constant("00000000-0000-0000-0000-000000000000"),
        onCreate: { _ in },
        onCancel: {}
    )
}
