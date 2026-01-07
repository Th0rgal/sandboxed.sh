//
//  Workspace.swift
//  OpenAgentDashboard
//
//  Workspace model for execution environments
//

import Foundation

/// Type of workspace execution environment.
enum WorkspaceType: String, Codable, CaseIterable {
    case host
    case chroot

    var displayName: String {
        switch self {
        case .host: return "Host"
        case .chroot: return "Chroot"
        }
    }

    var icon: String {
        switch self {
        case .host: return "desktopcomputer"
        case .chroot: return "cube.box"
        }
    }
}

/// Status of a workspace.
enum WorkspaceStatus: String, Codable, CaseIterable {
    case pending
    case building
    case ready
    case error

    var displayName: String {
        switch self {
        case .pending: return "Pending"
        case .building: return "Building"
        case .ready: return "Ready"
        case .error: return "Error"
        }
    }

    var isReady: Bool {
        self == .ready
    }
}

/// A workspace definition.
struct Workspace: Codable, Identifiable {
    let id: String
    let name: String
    let workspaceType: WorkspaceType
    let path: String
    let status: WorkspaceStatus
    let errorMessage: String?
    let createdAt: String

    enum CodingKeys: String, CodingKey {
        case id, name, path, status
        case workspaceType = "workspace_type"
        case errorMessage = "error_message"
        case createdAt = "created_at"
    }

    init(id: String, name: String, workspaceType: WorkspaceType, path: String, status: WorkspaceStatus, errorMessage: String?, createdAt: String) {
        self.id = id
        self.name = name
        self.workspaceType = workspaceType
        self.path = path
        self.status = status
        self.errorMessage = errorMessage
        self.createdAt = createdAt
    }

    /// Check if this is the default host workspace.
    var isDefault: Bool {
        // The default workspace has a nil UUID (all zeros)
        id == "00000000-0000-0000-0000-000000000000"
    }

    /// Display label for the workspace.
    var displayLabel: String {
        if isDefault {
            return "Host (Default)"
        }
        return name
    }

    /// Short description of the workspace.
    var shortDescription: String {
        "\(workspaceType.displayName) - \(path)"
    }
}

// MARK: - Preview Data

extension Workspace {
    static let defaultHost = Workspace(
        id: "00000000-0000-0000-0000-000000000000",
        name: "host",
        workspaceType: .host,
        path: "/root",
        status: .ready,
        errorMessage: nil,
        createdAt: ISO8601DateFormatter().string(from: Date())
    )

    static let previewChroot = Workspace(
        id: "12345678-1234-1234-1234-123456789012",
        name: "project-sandbox",
        workspaceType: .chroot,
        path: "/var/lib/openagent/chroots/project-sandbox",
        status: .ready,
        errorMessage: nil,
        createdAt: ISO8601DateFormatter().string(from: Date())
    )
}
