//
//  NavigationState.swift
//  OpenAgentDashboard
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
}
