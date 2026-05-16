//
//  BackendAgentService.swift
//  SandboxedDashboard
//
//  Shared service for loading backend/agent data used across views
//

import SwiftUI

/// Result of loading backends and their agents from the API
struct BackendAgentData {
    let backends: [Backend]
    let enabledBackendIds: Set<String>
    let backendAgents: [String: [BackendAgent]]
}

/// Shared service that centralizes backend/agent loading logic.
/// `@MainActor` ensures all mutable static state (cache) is accessed
/// exclusively on the main thread, eliminating data-race risk.
@MainActor
enum BackendAgentService {
    private static let api = APIService.shared

    /// Cached result and timestamp to avoid redundant network calls
    /// (e.g. when skip-agent-selection validates on every "New Mission" tap).
    private static var cachedData: BackendAgentData?
    private static var cacheTimestamp: Date?
    private static let cacheTTL: TimeInterval = 30 // seconds

    /// Load all enabled backends and their agents.
    /// Returns a cached result when available and fresh (within `cacheTTL`).
    static func loadBackendsAndAgents() async -> BackendAgentData {
        if let cached = cachedData,
           let ts = cacheTimestamp,
           Date().timeIntervalSince(ts) < cacheTTL {
            return cached
        }
        let data = await fetchBackendsAndAgents()
        cachedData = data
        cacheTimestamp = Date()
        return data
    }

    /// Force-reload bypassing the cache (e.g. when the user opens Settings).
    static func invalidateCache() {
        cachedData = nil
        cacheTimestamp = nil
    }

    /// Actual network fetch (extracted from the previous loadBackendsAndAgents).
    ///
    /// Fans out the per-backend config and agent lookups via `withTaskGroup`
    /// so a 5-backend install is one RTT-batch instead of five serial calls.
    /// On a 200 ms RTT cellular link this shaved ~1.6 s off every "New
    /// Mission" tap and every Settings open (UX audit item #3).
    private static func fetchBackendsAndAgents() async -> BackendAgentData {
        // Load backends
        let backends: [Backend]
        do {
            backends = try await api.listBackends()
        } catch {
            backends = Backend.defaults
        }

        // Parallel config fetch — one task per backend.
        let configResults = await withTaskGroup(of: (String, Bool).self) { group in
            for backend in backends {
                group.addTask {
                    if let config = try? await api.getBackendConfig(backendId: backend.id) {
                        return (backend.id, config.isEnabled)
                    }
                    // Default to enabled if we can't fetch config (parity with
                    // the previous serial implementation).
                    return (backend.id, true)
                }
            }
            var out: [(String, Bool)] = []
            for await pair in group { out.append(pair) }
            return out
        }
        let enabled = Set(configResults.filter(\.1).map(\.0))

        // Parallel agent fetch for each enabled backend.
        let agentResults = await withTaskGroup(of: (String, [BackendAgent]?).self) { group in
            for backendId in enabled {
                group.addTask {
                    if let agents = try? await api.listBackendAgents(backendId: backendId) {
                        return (backendId, agents)
                    }
                    return (backendId, nil)
                }
            }
            var out: [(String, [BackendAgent]?)] = []
            for await pair in group { out.append(pair) }
            return out
        }
        var backendAgents: [String: [BackendAgent]] = [:]
        for (backendId, agents) in agentResults {
            if let agents {
                backendAgents[backendId] = agents
            } else if backendId == "amp" {
                // Preserve the legacy Amp fallback so the picker isn't empty
                // when the agents endpoint flakes.
                backendAgents[backendId] = [
                    BackendAgent(id: "smart", name: "Smart Mode"),
                    BackendAgent(id: "rush", name: "Rush Mode")
                ]
            }
        }

        return BackendAgentData(
            backends: backends,
            enabledBackendIds: enabled,
            backendAgents: backendAgents
        )
    }

    /// Icon name for a backend ID
    static func icon(for id: String?) -> String {
        switch id {
        case "opencode": return "terminal"
        case "claudecode": return "brain"
        case "amp": return "bolt.fill"
        case "codex": return "chevron.left.forwardslash.chevron.right"
        case "gemini": return "sparkles"
        case "grok": return "xmark.circle"
        default: return "cpu"
        }
    }

    /// Color for a backend ID
    static func color(for id: String?) -> Color {
        switch id {
        case "opencode": return Theme.success
        case "claudecode": return Theme.accent
        case "amp": return .orange
        case "codex": return .cyan
        case "gemini": return .blue
        case "grok": return Color(white: 0.85)
        default: return Theme.textSecondary
        }
    }
}
