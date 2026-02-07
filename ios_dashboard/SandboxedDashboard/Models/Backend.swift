//
//  Backend.swift
//  SandboxedDashboard
//
//  Backend data models for OpenCode, Claude Code, and Amp
//

import Foundation

/// Represents an available backend (OpenCode, Claude Code, Amp)
struct Backend: Codable, Identifiable, Hashable {
    let id: String
    let name: String
    
    static let opencode = Backend(id: "opencode", name: "OpenCode")
    static let claudecode = Backend(id: "claudecode", name: "Claude Code")
    static let amp = Backend(id: "amp", name: "Amp")
    
    /// Default backends when API is unavailable
    static let defaults: [Backend] = [.opencode, .claudecode, .amp]
}

/// Represents an agent within a backend
struct BackendAgent: Codable, Identifiable, Hashable {
    let id: String
    let name: String
}

/// Backend configuration including enabled state
struct BackendConfig: Codable {
    let id: String
    let name: String
    let enabled: Bool
    
    /// Helper to check if backend is enabled (defaults to true if not specified)
    var isEnabled: Bool { enabled }
    
    enum CodingKeys: String, CodingKey {
        case id, name, enabled
    }
}

/// A provider of AI models (e.g., Anthropic, OpenAI)
struct Provider: Codable, Identifiable {
    let id: String
    let name: String
    let billing: BillingType
    let description: String
    let models: [ProviderModel]
    
    enum BillingType: String, Codable {
        case subscription
        case payPerToken = "pay-per-token"
    }
}

/// A model available from a provider
struct ProviderModel: Codable, Identifiable {
    let id: String
    let name: String
    let description: String?
}

/// Response wrapper for providers API
struct ProvidersResponse: Codable {
    let providers: [Provider]
}

/// Combined agent with backend info for display
struct CombinedAgent: Identifiable, Hashable {
    let backend: String
    let backendName: String
    let agent: String
    
    var id: String { "\(backend):\(agent)" }
    var value: String { "\(backend):\(agent)" }
    
    /// Parse a combined value back to backend and agent
    static func parse(_ value: String) -> (backend: String, agent: String)? {
        let parts = value.split(separator: ":", maxSplits: 1)
        guard parts.count == 2 else { return nil }
        return (String(parts[0]), String(parts[1]))
    }
}
