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
    /// Skill names from library synced to this workspace
    let skills: [String]
    /// Tool names from library synced to this workspace
    let tools: [String]
    /// Plugin identifiers for hooks
    let plugins: [String]
    /// Workspace template name (if created from a template)
    let template: String?
    /// Preferred Linux distribution for container workspaces
    let distro: String?
    /// Environment variables always loaded in this workspace
    let envVars: [String: String]
    /// Init script to run when the workspace is built/rebuilt
    let initScript: String?

    enum CodingKeys: String, CodingKey {
        case id, name, path, status, skills, tools, plugins, template, distro
        case workspaceType = "workspace_type"
        case errorMessage = "error_message"
        case createdAt = "created_at"
        case envVars = "env_vars"
        case initScript = "init_script"
    }

    init(id: String, name: String, workspaceType: WorkspaceType, path: String, status: WorkspaceStatus, errorMessage: String?, createdAt: String, skills: [String] = [], tools: [String] = [], plugins: [String] = [], template: String? = nil, distro: String? = nil, envVars: [String: String] = [:], initScript: String? = nil) {
        self.id = id
        self.name = name
        self.workspaceType = workspaceType
        self.path = path
        self.status = status
        self.errorMessage = errorMessage
        self.createdAt = createdAt
        self.skills = skills
        self.tools = tools
        self.plugins = plugins
        self.template = template
        self.distro = distro
        self.envVars = envVars
        self.initScript = initScript
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        name = try container.decode(String.self, forKey: .name)
        workspaceType = try container.decode(WorkspaceType.self, forKey: .workspaceType)
        path = try container.decode(String.self, forKey: .path)
        status = try container.decode(WorkspaceStatus.self, forKey: .status)
        errorMessage = try container.decodeIfPresent(String.self, forKey: .errorMessage)
        createdAt = try container.decode(String.self, forKey: .createdAt)
        skills = try container.decodeIfPresent([String].self, forKey: .skills) ?? []
        tools = try container.decodeIfPresent([String].self, forKey: .tools) ?? []
        plugins = try container.decodeIfPresent([String].self, forKey: .plugins) ?? []
        template = try container.decodeIfPresent(String.self, forKey: .template)
        distro = try container.decodeIfPresent(String.self, forKey: .distro)
        envVars = try container.decodeIfPresent([String: String].self, forKey: .envVars) ?? [:]
        initScript = try container.decodeIfPresent(String.self, forKey: .initScript)
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
        createdAt: ISO8601DateFormatter().string(from: Date()),
        skills: [],
        tools: [],
        plugins: []
    )

    static let previewChroot = Workspace(
        id: "12345678-1234-1234-1234-123456789012",
        name: "project-sandbox",
        workspaceType: .chroot,
        path: "/var/lib/openagent/containers/project-sandbox",
        status: .ready,
        errorMessage: nil,
        createdAt: ISO8601DateFormatter().string(from: Date()),
        skills: ["code-review", "testing"],
        tools: ["pytest", "eslint"],
        plugins: []
    )
}
