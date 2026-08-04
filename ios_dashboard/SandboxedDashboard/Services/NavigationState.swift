//
//  NavigationState.swift
//  SandboxedDashboard
//
//  Shared navigation state for cross-tab navigation
//

import SwiftUI

@MainActor
@Observable
final class NavigationState {
    static let shared = NavigationState()
    
    /// Currently selected tab
    var selectedTab: MainTabView.TabItem = .control
    
    /// Mission ID to open in Control tab (set from History, cleared after use)
    var pendingMissionId: String?

    /// Hermes session id to open in the Control tab. Cleared after use, like
    /// `pendingMissionId` — the tab shows one conversation at a time.
    var pendingHermesSessionId: String?

    private init() {}

    /// Navigate to Control tab with a specific mission
    func openMission(_ missionId: String) {
        pendingMissionId = missionId
        selectedTab = .control
        HapticService.selectionChanged()
    }

    /// Consume the pending mission ID (called by ControlView)
    func consumePendingMission() -> String? {
        let id = pendingMissionId
        pendingMissionId = nil
        return id
    }

    /// Navigate to the Control tab showing a Hermes conversation.
    func openHermesSession(_ sessionId: String) {
        pendingHermesSessionId = sessionId
        selectedTab = .control
        HapticService.selectionChanged()
    }

    /// Handle a `sandboxed://` deep link.
    ///
    /// Supported: `sandboxed://mission/<id>` and `sandboxed://session/<id>` —
    /// the two conversation kinds, mirroring the web's `?mission=` / `?session=`.
    func handle(url: URL) {
        guard url.scheme?.lowercased() == "sandboxed" else { return }
        // Both `sandboxed://session/<id>` (host = "session") and
        // `sandboxed:///session/<id>` (empty host) are accepted.
        var components = url.pathComponents.filter { $0 != "/" }
        if let host = url.host, !host.isEmpty {
            components.insert(host, at: 0)
        }
        guard components.count >= 2 else { return }
        let identifier = components[1]
        guard !identifier.isEmpty else { return }
        switch components[0].lowercased() {
        case "mission":
            openMission(identifier)
        case "session":
            openHermesSession(identifier)
        default:
            break
        }
    }
}
