//
//  WorkspaceState.swift
//  SandboxedDashboard
//
//  Global workspace selection state shared across tabs
//

import Foundation
import Observation

@MainActor
@Observable
final class WorkspaceState {
    static let shared = WorkspaceState()
    private init() {}

    /// All available workspaces
    var workspaces: [Workspace] = []

    /// Currently selected workspace (nil means host/default)
    var selectedWorkspace: Workspace?

    /// Whether we're currently loading workspaces
    var isLoading = false

    /// Error message if loading failed
    var errorMessage: String?

    private let api = APIService.shared

    /// Load workspaces from the API
    func loadWorkspaces() async {
        guard !isLoading else { return }
        isLoading = true
        errorMessage = nil

        do {
            workspaces = try await api.listWorkspaces()

            // If no workspace is selected, default to host
            if selectedWorkspace == nil {
                selectedWorkspace = workspaces.first { $0.isDefault }
            }

            // Validate selected workspace still exists
            if let selected = selectedWorkspace,
               !workspaces.contains(where: { $0.id == selected.id }) {
                selectedWorkspace = workspaces.first { $0.isDefault }
            }
        } catch {
            errorMessage = error.localizedDescription
        }

        isLoading = false
    }

    /// Select a workspace by ID
    func selectWorkspace(id: String) {
        selectedWorkspace = workspaces.first { $0.id == id }
    }

    /// Get the display name for the current workspace
    var currentWorkspaceLabel: String {
        selectedWorkspace?.displayLabel ?? "Host"
    }

    /// Get the icon for the current workspace type
    var currentWorkspaceIcon: String {
        selectedWorkspace?.workspaceType.icon ?? "desktopcomputer"
    }

    /// Check if the selected workspace is ready
    var isWorkspaceReady: Bool {
        selectedWorkspace?.status.isReady ?? true
    }

    /// Get the base path for file browsing in the current workspace
    var filesBasePath: String {
        guard let workspace = selectedWorkspace else {
            return "/root/context"
        }

        // For host workspace, use /root/context
        if workspace.isDefault {
            return "/root/context"
        }

        // For container workspaces, use the workspace root
        // The backend maps this appropriately
        return "/root"
    }
}
