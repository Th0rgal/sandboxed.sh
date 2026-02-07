//
//  Mission.swift
//  SandboxedDashboard
//
//  Mission and task data models
//

import Foundation

enum MissionStatus: String, Codable, CaseIterable {
    case active
    case completed
    case failed
    case interrupted
    case blocked
    case notFeasible = "not_feasible"
    
    var statusType: StatusType {
        switch self {
        case .active: return .active
        case .completed: return .completed
        case .failed: return .failed
        case .interrupted: return .interrupted
        case .blocked: return .blocked
        case .notFeasible: return .failed
        }
    }
    
    var displayLabel: String {
        switch self {
        case .active: return "Active"
        case .completed: return "Completed"
        case .failed: return "Failed"
        case .interrupted: return "Interrupted"
        case .blocked: return "Blocked"
        case .notFeasible: return "Not Feasible"
        }
    }
    
    var canResume: Bool {
        self == .interrupted || self == .blocked
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
    let title: String?
    let workspaceId: String?
    let workspaceName: String?
    let agent: String?
    let backend: String?
    let history: [MissionHistoryEntry]
    let createdAt: String
    let updatedAt: String
    let interruptedAt: String?
    let resumable: Bool

    func hash(into hasher: inout Hasher) {
        hasher.combine(id)
    }

    static func == (lhs: Mission, rhs: Mission) -> Bool {
        lhs.id == rhs.id
    }

    enum CodingKeys: String, CodingKey {
        case id, status, title, history, resumable, agent, backend
        case workspaceId = "workspace_id"
        case workspaceName = "workspace_name"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
        case interruptedAt = "interrupted_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        status = try container.decode(MissionStatus.self, forKey: .status)
        title = try container.decodeIfPresent(String.self, forKey: .title)
        workspaceId = try container.decodeIfPresent(String.self, forKey: .workspaceId)
        workspaceName = try container.decodeIfPresent(String.self, forKey: .workspaceName)
        agent = try container.decodeIfPresent(String.self, forKey: .agent)
        backend = try container.decodeIfPresent(String.self, forKey: .backend)
        history = try container.decode([MissionHistoryEntry].self, forKey: .history)
        createdAt = try container.decode(String.self, forKey: .createdAt)
        updatedAt = try container.decode(String.self, forKey: .updatedAt)
        interruptedAt = try container.decodeIfPresent(String.self, forKey: .interruptedAt)
        resumable = try container.decodeIfPresent(Bool.self, forKey: .resumable) ?? false
    }

    var displayTitle: String {
        if let title = title, !title.isEmpty {
            return title.count > 60 ? String(title.prefix(60)) + "..." : title
        }
        return "Untitled Mission"
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

    var id: String { missionId }

    enum CodingKeys: String, CodingKey {
        case missionId = "mission_id"
        case state
        case queueLen = "queue_len"
        case historyLen = "history_len"
        case secondsSinceActivity = "seconds_since_activity"
        case expectedDeliverables = "expected_deliverables"
    }

    // Memberwise initializer for previews and testing
    init(missionId: String, state: String, queueLen: Int, historyLen: Int, secondsSinceActivity: Int, expectedDeliverables: Int) {
        self.missionId = missionId
        self.state = state
        self.queueLen = queueLen
        self.historyLen = historyLen
        self.secondsSinceActivity = secondsSinceActivity
        self.expectedDeliverables = expectedDeliverables
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
}

struct ParallelConfig: Codable {
    let maxParallelMissions: Int
    let runningCount: Int
    
    enum CodingKeys: String, CodingKey {
        case maxParallelMissions = "max_parallel_missions"
        case runningCount = "running_count"
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
