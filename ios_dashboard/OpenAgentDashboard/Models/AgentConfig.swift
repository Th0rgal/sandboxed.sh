//
//  AgentConfig.swift
//  OpenAgentDashboard
//
//  Agent configuration model
//

import Foundation

/// An agent configuration defining model, tools, and capabilities.
struct AgentConfig: Identifiable, Codable {
    let id: String
    var name: String
    var model_id: String
    var mcp_servers: [String]
    var skills: [String]
    var commands: [String]
    let created_at: String
    let updated_at: String
}

// MARK: - Preview Data

extension AgentConfig {
    static let preview = AgentConfig(
        id: "00000000-0000-0000-0000-000000000001",
        name: "Default Agent",
        model_id: "claude-sonnet-4-20250514",
        mcp_servers: ["playwright", "supabase"],
        skills: ["coding", "research"],
        commands: ["review-pr"],
        created_at: ISO8601DateFormatter().string(from: Date()),
        updated_at: ISO8601DateFormatter().string(from: Date())
    )
}
