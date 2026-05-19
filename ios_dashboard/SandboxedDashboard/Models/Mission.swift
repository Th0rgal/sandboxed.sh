//
//  Mission.swift
//  SandboxedDashboard
//
//  Mission and task data models
//

import Foundation

enum MissionStatus: String, Codable, CaseIterable {
    case pending
    case active
    /// Agent's turn / automation cycle finished cleanly with no follow-up
    /// queued; the mission is parked waiting for the user. This is the
    /// "Needs You" bucket.
    case awaitingUser = "awaiting_user"
    /// User opened the mission while it was AwaitingUser and the 1h grace
    /// elapsed without a new message — auto-archived under Finished.
    case acknowledged
    case completed
    case failed
    case interrupted
    case blocked
    case notFeasible = "not_feasible"
    case unknown

    var statusType: StatusType {
        switch self {
        case .pending: return .pending
        case .active: return .active
        case .awaitingUser: return .awaitingUser
        case .acknowledged: return .completed
        case .completed: return .completed
        case .failed: return .failed
        case .interrupted, .blocked, .notFeasible, .unknown: return .failed
        }
    }

    var displayLabel: String {
        switch self {
        case .pending: return "Pending"
        case .active: return "Active"
        case .awaitingUser: return "Needs You"
        case .acknowledged: return "Acknowledged"
        case .completed: return "Completed"
        case .failed: return "Failed"
        case .interrupted: return "Interrupted"
        case .blocked: return "Blocked"
        case .notFeasible: return "Not Feasible"
        case .unknown: return "Unknown"
        }
    }

    /// Stored statuses that the user can wake back into Active by sending a
    /// new message. AwaitingUser/Acknowledged count too — the agent is idle
    /// and ready to resume.
    var canResume: Bool {
        switch self {
        case .interrupted, .blocked, .awaitingUser, .acknowledged:
            return true
        default:
            return false
        }
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        let rawValue = try container.decode(String.self)
        self = MissionStatus(rawValue: rawValue) ?? .unknown
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(rawValue)
    }
}

struct MissionHistoryEntry: Codable, Identifiable {
    var id: String { "\(role)-\(content.prefix(20))" }
    let role: String
    let content: String
    
    var isUser: Bool {
        role == "user"
    }
}

struct Mission: Codable, Identifiable, Hashable {
    let id: String
    var status: MissionStatus
    var title: String?
    var shortDescription: String?
    var metadataUpdatedAt: String?
    var metadataSource: String?
    var metadataModel: String?
    var metadataVersion: String?
    let workspaceId: String?
    let workspaceName: String?
    let agent: String?
    let modelOverride: String?
    let backend: String?
    let history: [MissionHistoryEntry]
    let createdAt: String
    var updatedAt: String
    let interruptedAt: String?
    /// Timestamp of the user's first open since this mission last entered
    /// AwaitingUser. Drives the "opened" dot rendered next to Finished
    /// missions, and the backend's 1h ack grace timer.
    let firstViewedAt: String?
    let resumable: Bool
    let parentMissionId: String?

    func hash(into hasher: inout Hasher) {
        hasher.combine(id)
    }

    static func == (lhs: Mission, rhs: Mission) -> Bool {
        lhs.id == rhs.id
    }

    enum CodingKeys: String, CodingKey {
        case id, status, title, history, resumable, agent, backend
        case shortDescription = "short_description"
        case metadataUpdatedAt = "metadata_updated_at"
        case metadataSource = "metadata_source"
        case metadataModel = "metadata_model"
        case metadataVersion = "metadata_version"
        case workspaceId = "workspace_id"
        case workspaceName = "workspace_name"
        case modelOverride = "model_override"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
        case interruptedAt = "interrupted_at"
        case firstViewedAt = "first_viewed_at"
        case parentMissionId = "parent_mission_id"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        status = try container.decode(MissionStatus.self, forKey: .status)
        title = try container.decodeIfPresent(String.self, forKey: .title)
        shortDescription = try container.decodeIfPresent(String.self, forKey: .shortDescription)
        metadataUpdatedAt = try container.decodeIfPresent(String.self, forKey: .metadataUpdatedAt)
        metadataSource = try container.decodeIfPresent(String.self, forKey: .metadataSource)
        metadataModel = try container.decodeIfPresent(String.self, forKey: .metadataModel)
        metadataVersion = try container.decodeIfPresent(String.self, forKey: .metadataVersion)
        workspaceId = try container.decodeIfPresent(String.self, forKey: .workspaceId)
        workspaceName = try container.decodeIfPresent(String.self, forKey: .workspaceName)
        agent = try container.decodeIfPresent(String.self, forKey: .agent)
        modelOverride = try container.decodeIfPresent(String.self, forKey: .modelOverride)
        backend = try container.decodeIfPresent(String.self, forKey: .backend)
        history = try container.decode([MissionHistoryEntry].self, forKey: .history)
        createdAt = try container.decode(String.self, forKey: .createdAt)
        updatedAt = try container.decode(String.self, forKey: .updatedAt)
        interruptedAt = try container.decodeIfPresent(String.self, forKey: .interruptedAt)
        firstViewedAt = try container.decodeIfPresent(String.self, forKey: .firstViewedAt)
        resumable = try container.decodeIfPresent(Bool.self, forKey: .resumable) ?? false
        parentMissionId = try container.decodeIfPresent(String.self, forKey: .parentMissionId)
    }

    var displayTitle: String {
        if let title = title, !title.isEmpty {
            return title.count > 60 ? String(title.prefix(60)) + "..." : title
        }
        return "Untitled Mission"
    }

    var displayShortDescription: String? {
        guard let shortDescription, !shortDescription.isEmpty else { return nil }
        return shortDescription.count > 100 ? String(shortDescription.prefix(100)) + "..." : shortDescription
    }
    
    var updatedDate: Date? {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.date(from: updatedAt) ?? ISO8601DateFormatter().date(from: updatedAt)
    }
    
    var canResume: Bool {
        resumable && status.canResume
    }
}

enum TaskStatus: String, Codable, CaseIterable {
    case pending
    case running
    case completed
    case failed
    case cancelled
    
    var statusType: StatusType {
        switch self {
        case .pending: return .pending
        case .running: return .running
        case .completed: return .completed
        case .failed: return .failed
        case .cancelled: return .cancelled
        }
    }
}

struct TaskState: Codable, Identifiable {
    let id: String
    let status: TaskStatus
    let task: String
    let model: String
    let iterations: Int
    let result: String?
    
    var displayModel: String {
        if let lastPart = model.split(separator: "/").last {
            return String(lastPart)
        }
        return model
    }
}

// MARK: - Queue

struct QueuedMessage: Codable, Identifiable {
    let id: String
    let content: String
    let agent: String?

    /// Truncated content for display (max 100 chars)
    var displayContent: String {
        if content.count > 100 {
            return String(content.prefix(100)) + "..."
        }
        return content
    }
}

// MARK: - Parallel Execution

struct RunningMissionInfo: Codable, Identifiable {
    let missionId: String
    let state: String
    let queueLen: Int
    let historyLen: Int
    let secondsSinceActivity: Int
    let expectedDeliverables: Int
    let currentActivity: String?
    let title: String?

    var id: String { missionId }

    enum CodingKeys: String, CodingKey {
        case missionId = "mission_id"
        case state
        case queueLen = "queue_len"
        case historyLen = "history_len"
        case secondsSinceActivity = "seconds_since_activity"
        case expectedDeliverables = "expected_deliverables"
        case currentActivity = "current_activity"
        case title
    }

    // Custom decoder to handle optional fields
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        missionId = try container.decode(String.self, forKey: .missionId)
        state = try container.decode(String.self, forKey: .state)
        queueLen = try container.decode(Int.self, forKey: .queueLen)
        historyLen = try container.decode(Int.self, forKey: .historyLen)
        secondsSinceActivity = try container.decode(Int.self, forKey: .secondsSinceActivity)
        expectedDeliverables = try container.decode(Int.self, forKey: .expectedDeliverables)
        currentActivity = try container.decodeIfPresent(String.self, forKey: .currentActivity)
        title = try container.decodeIfPresent(String.self, forKey: .title)
    }

    // Memberwise initializer for previews and testing
    init(missionId: String, state: String, queueLen: Int, historyLen: Int, secondsSinceActivity: Int, expectedDeliverables: Int, currentActivity: String? = nil, title: String? = nil) {
        self.missionId = missionId
        self.state = state
        self.queueLen = queueLen
        self.historyLen = historyLen
        self.secondsSinceActivity = secondsSinceActivity
        self.expectedDeliverables = expectedDeliverables
        self.currentActivity = currentActivity
        self.title = title
    }

    var isRunning: Bool {
        state == "running" || state == "waiting_for_tool"
    }

    var isStalled: Bool {
        isRunning && secondsSinceActivity > 60
    }

    /// Short identifier for the mission (first 8 chars of ID)
    var shortId: String {
        String(missionId.prefix(8)).uppercased()
    }

    /// Best available label: title (truncated) or short ID fallback
    var displayLabel: String {
        if let title = title, !title.isEmpty {
            return title.count > 24 ? String(title.prefix(24)) + "…" : title
        }
        return shortId
    }
}

struct MissionMomentSearchResult: Codable {
    let mission: Mission
    let entryIndex: Int
    let role: String
    let snippet: String
    let rationale: String
    let relevanceScore: Double

    enum CodingKeys: String, CodingKey {
        case mission
        case entryIndex = "entry_index"
        case role
        case snippet
        case rationale
        case relevanceScore = "relevance_score"
    }
}

struct MissionSearchResult: Codable {
    let mission: Mission
    let relevanceScore: Double

    enum CodingKeys: String, CodingKey {
        case mission
        case relevanceScore = "relevance_score"
    }
}

// MARK: - Events

struct StoredEvent: Codable, Identifiable, Sendable {
    let id: Int64
    let missionId: String
    let sequence: Int64
    let eventType: String
    let timestamp: String
    let eventId: String?
    let toolCallId: String?
    let toolName: String?
    let content: String
    let metadata: [String: AnyCodable]

    enum CodingKeys: String, CodingKey {
        case id
        case missionId = "mission_id"
        case sequence
        case eventType = "event_type"
        case timestamp
        case eventId = "event_id"
        case toolCallId = "tool_call_id"
        case toolName = "tool_name"
        case content
        case metadata
    }

    init(
        id: Int64,
        missionId: String,
        sequence: Int64,
        eventType: String,
        timestamp: String,
        eventId: String?,
        toolCallId: String?,
        toolName: String?,
        content: String,
        metadata: [String: AnyCodable]
    ) {
        self.id = id
        self.missionId = missionId
        self.sequence = sequence
        self.eventType = eventType
        self.timestamp = timestamp
        self.eventId = eventId
        self.toolCallId = toolCallId
        self.toolName = toolName
        self.content = content
        self.metadata = metadata
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(Int64.self, forKey: .id)
        missionId = try container.decode(String.self, forKey: .missionId)
        sequence = try container.decode(Int64.self, forKey: .sequence)
        eventType = try container.decode(String.self, forKey: .eventType)
        timestamp = try container.decode(String.self, forKey: .timestamp)
        eventId = try container.decodeIfPresent(String.self, forKey: .eventId)
        toolCallId = try container.decodeIfPresent(String.self, forKey: .toolCallId)
        toolName = try container.decodeIfPresent(String.self, forKey: .toolName)
        content = try container.decode(String.self, forKey: .content)

        // Decode metadata as generic JSON
        if let metadataValue = try? container.decode([String: AnyCodable].self, forKey: .metadata) {
            metadata = metadataValue
        } else {
            metadata = [:]
        }
    }
}

// MARK: - Runs

struct Run: Codable, Identifiable {
    let id: String
    let createdAt: String
    let status: String
    let inputText: String
    let finalOutput: String?
    let totalCostCents: Int
    let summaryText: String?
    
    enum CodingKeys: String, CodingKey {
        case id, status
        case createdAt = "created_at"
        case inputText = "input_text"
        case finalOutput = "final_output"
        case totalCostCents = "total_cost_cents"
        case summaryText = "summary_text"
    }
    
    var costDollars: Double {
        Double(totalCostCents) / 100.0
    }
    
    var createdDate: Date? {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.date(from: createdAt) ?? ISO8601DateFormatter().date(from: createdAt)
    }
}
