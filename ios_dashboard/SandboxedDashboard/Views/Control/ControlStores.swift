//
//  ControlStores.swift
//  SandboxedDashboard
//
//  Small observed stores for ControlView state with high invalidation churn.
//

import Foundation
import Observation

@MainActor
@Observable
final class ChatTranscriptStore {
    var messages: [ChatMessage] = []
    var groupedItems: [GroupedChatItem] = []

    @ObservationIgnored
    var groupedItemsRecomputeTask: Task<Void, Never>?
}

@MainActor
@Observable
final class MissionListStore {
    var recentMissions: [Mission] = []
    var childMissions: [Mission] = []
}

@MainActor
@Observable
final class RunningMissionsStore {
    var runningMissions: [RunningMissionInfo] = []
    var showRunningMissions = false

    @ObservationIgnored
    var refreshTask: Task<Void, Never>?
}
