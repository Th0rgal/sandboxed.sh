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
    /// Fans out the per-backend config and agent requests with `withTaskGroup`
    /// instead of the previous sequential `for` loops. Previously: N backends →
    /// N config round-trips + M agent round-trips, all serialised. On a slow
    /// cellular link with 3 backends that was ~6 sequential RTTs before the
    /// user saw an agent picker. Now all per-backend calls run concurrently,
    /// gated only by the first (`listBackends`) and capped by the URLSession's
    /// per-host connection limit.
    private static func fetchBackendsAndAgents() async -> BackendAgentData {
        let backends: [Backend]
        do {
            backends = try await api.listBackends()
        } catch {
            backends = Backend.defaults
        }

        // Fan out config probes in parallel. Default-to-enabled on error
        // mirrors the previous behaviour so a flaky `/config` endpoint
        // doesn't strand the user with an empty backend list.
        let enabled: Set<String> = await withTaskGroup(of: (String, Bool).self) { group in
            for backend in backends {
                group.addTask {
                    do {
                        let config = try await api.getBackendConfig(backendId: backend.id)
                        return (backend.id, config.isEnabled)
                    } catch {
                        return (backend.id, true)
                    }
                }
            }
            var result = Set<String>()
            for await (id, isEnabled) in group where isEnabled {
                result.insert(id)
            }
            return result
        }

        // Fan out agent probes in parallel.
        let backendAgents: [String: [BackendAgent]] = await withTaskGroup(of: (String, [BackendAgent]?).self) { group in
            for backendId in enabled {
                group.addTask {
                    do {
                        let agents = try await api.listBackendAgents(backendId: backendId)
                        return (backendId, agents)
                    } catch {
                        return (backendId, nil)
                    }
                }
            }
            var result: [String: [BackendAgent]] = [:]
            for await (id, agents) in group {
                if let agents {
                    result[id] = agents
                } else if id == "amp" {
                    // Amp ships hardcoded fallbacks when its agent list 404s
                    // mid-rollout; preserved from the original behaviour.
                    result[id] = [
                        BackendAgent(id: "smart", name: "Smart Mode"),
                        BackendAgent(id: "rush", name: "Rush Mode")
                    ]
                }
            }
            return result
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
        default: return "cpu"
        }
    }

    /// Color for a backend ID
    static func color(for id: String?) -> Color {
        switch id {
        case "opencode": return Theme.success
        case "claudecode": return Theme.accent
        case "amp": return .orange
        default: return Theme.textSecondary
        }
    }
}
