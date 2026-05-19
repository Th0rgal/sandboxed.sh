//
//  Backend.swift
//  SandboxedDashboard
//
//  Backend data models for OpenCode, Claude Code, Amp, Codex, Gemini, and Grok
//

import Foundation

/// Represents an available backend (OpenCode, Claude Code, Amp, Codex, Gemini, Grok)
struct Backend: Codable, Identifiable, Hashable {
    let id: String
    let name: String

    static let opencode = Backend(id: "opencode", name: "OpenCode")
    static let claudecode = Backend(id: "claudecode", name: "Claude Code")
    static let amp = Backend(id: "amp", name: "Amp")
    static let codex = Backend(id: "codex", name: "Codex")
    static let gemini = Backend(id: "gemini", name: "Gemini CLI")
    static let grok = Backend(id: "grok", name: "Grok Build")

    /// Default backends when API is unavailable
    static let defaults: [Backend] = [.opencode, .claudecode, .amp, .codex, .gemini, .grok]
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
// MARK: - Slash commands

/// One slash command surfaced by `/api/library/builtin-commands`. Mirrors
/// the backend's `CommandSummary` shape — `params` is included so the
/// suggestion popover can hint at required arguments (e.g. `/goal
/// <objective>`).
///
/// All fields except `name` decode defensively: Swift's auto-synthesized
/// `Codable` throws `keyNotFound` on missing JSON keys, but the Rust side
/// uses `skip_serializing_if` on optional/empty fields. A custom decoder
/// keeps us robust to those omissions.
struct SlashCommand: Codable, Identifiable, Hashable {
    let name: String
    let description: String?
    let path: String
    let params: [SlashCommandParam]

    var id: String { name }

    private enum CodingKeys: String, CodingKey {
        case name, description, path, params
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = try c.decode(String.self, forKey: .name)
        description = try c.decodeIfPresent(String.self, forKey: .description)
        path = try c.decodeIfPresent(String.self, forKey: .path) ?? ""
        params = try c.decodeIfPresent([SlashCommandParam].self, forKey: .params) ?? []
    }

    /// Manual init for the ad-hoc test fixtures we sometimes construct
    /// without going through the decoder.
    init(name: String, description: String?, path: String, params: [SlashCommandParam] = []) {
        self.name = name
        self.description = description
        self.path = path
        self.params = params
    }
}

struct SlashCommandParam: Codable, Hashable {
    let name: String
    let required: Bool
    let description: String?

    private enum CodingKeys: String, CodingKey {
        case name, required, description
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = try c.decode(String.self, forKey: .name)
        required = try c.decodeIfPresent(Bool.self, forKey: .required) ?? false
        description = try c.decodeIfPresent(String.self, forKey: .description)
    }

    init(name: String, required: Bool = false, description: String? = nil) {
        self.name = name
        self.required = required
        self.description = description
    }
}

/// Per-backend builtin commands payload. Every field decodes defensively —
/// older codex builds (pre-0.128.0) omit the `codex` field entirely, and
/// any backend with no commands omits its array via Rust's
/// `skip_serializing_if = "Vec::is_empty"`.
struct BuiltinCommandsResponse: Codable {
    let opencode: [SlashCommand]
    let claudecode: [SlashCommand]
    let codex: [SlashCommand]?
    /// Grok Build builtin commands (just `/goal` today — sandboxed.sh-driven,
    /// not a native grok feature). Optional so older backends without the
    /// field decode fine.
    let grok: [SlashCommand]?

    private enum CodingKeys: String, CodingKey {
        case opencode, claudecode, codex, grok
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        opencode = try c.decodeIfPresent([SlashCommand].self, forKey: .opencode) ?? []
        claudecode = try c.decodeIfPresent([SlashCommand].self, forKey: .claudecode) ?? []
        codex = try c.decodeIfPresent([SlashCommand].self, forKey: .codex)
        grok = try c.decodeIfPresent([SlashCommand].self, forKey: .grok)
    }

    init(
        opencode: [SlashCommand] = [],
        claudecode: [SlashCommand] = [],
        codex: [SlashCommand]? = nil,
        grok: [SlashCommand]? = nil
    ) {
        self.opencode = opencode
        self.claudecode = claudecode
        self.codex = codex
        self.grok = grok
    }
}

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
