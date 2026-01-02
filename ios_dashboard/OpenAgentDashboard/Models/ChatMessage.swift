//
//  ChatMessage.swift
//  OpenAgentDashboard
//
//  Chat message models for the control view
//

import Foundation

enum ChatMessageType {
    case user
    case assistant(success: Bool, costCents: Int, model: String?)
    case thinking(done: Bool, startTime: Date)
    case phase(phase: String, detail: String?, agent: String?)
    case toolUI(name: String)
    case system
    case error
}

struct ChatMessage: Identifiable {
    let id: String
    let type: ChatMessageType
    var content: String
    var toolUI: ToolUIContent?
    let timestamp: Date
    
    init(id: String = UUID().uuidString, type: ChatMessageType, content: String, toolUI: ToolUIContent? = nil, timestamp: Date = Date()) {
        self.id = id
        self.type = type
        self.content = content
        self.toolUI = toolUI
        self.timestamp = timestamp
    }
    
    var isUser: Bool {
        if case .user = type { return true }
        return false
    }
    
    var isAssistant: Bool {
        if case .assistant = type { return true }
        return false
    }
    
    var isThinking: Bool {
        if case .thinking = type { return true }
        return false
    }
    
    var isToolUI: Bool {
        if case .toolUI = type { return true }
        return false
    }
    
    var isPhase: Bool {
        if case .phase = type { return true }
        return false
    }
    
    var thinkingDone: Bool {
        if case .thinking(let done, _) = type { return done }
        return false
    }
    
    var thinkingStartTime: Date? {
        if case .thinking(_, let startTime) = type { return startTime }
        return nil
    }
    
    var displayModel: String? {
        if case .assistant(_, _, let model) = type {
            if let model = model {
                return model.split(separator: "/").last.map(String.init)
            }
        }
        return nil
    }
    
    var costFormatted: String? {
        if case .assistant(_, let costCents, _) = type, costCents > 0 {
            return String(format: "$%.4f", Double(costCents) / 100.0)
        }
        return nil
    }
}

// MARK: - Control Session State

enum ControlRunState: String, Codable {
    case idle
    case running
    case waitingForTool = "waiting_for_tool"

    var statusType: StatusType {
        switch self {
        case .idle: return .idle
        case .running: return .running
        case .waitingForTool: return .pending
        }
    }

    var label: String {
        switch self {
        case .idle: return "Idle"
        case .running: return "Running"
        case .waitingForTool: return "Waiting"
        }
    }
}

// MARK: - Connection State

enum ConnectionState {
    case connected
    case reconnecting(attempt: Int)
    case disconnected

    var isConnected: Bool {
        if case .connected = self { return true }
        return false
    }

    var label: String {
        switch self {
        case .connected: return ""
        case .reconnecting(let attempt): return attempt > 1 ? "Reconnecting (\(attempt))..." : "Reconnecting..."
        case .disconnected: return "Disconnected"
        }
    }

    var icon: String {
        switch self {
        case .connected: return "wifi"
        case .reconnecting: return "wifi.exclamationmark"
        case .disconnected: return "wifi.slash"
        }
    }
}

// MARK: - Execution Progress

struct ExecutionProgress {
    let total: Int
    let completed: Int
    let current: String?
    let depth: Int
    
    var displayText: String {
        "Subtask \(completed + 1)/\(total)"
    }
}

// MARK: - Phase Labels

enum AgentPhase: String {
    case estimatingComplexity = "estimating_complexity"
    case selectingModel = "selecting_model"
    case splittingTask = "splitting_task"
    case executing = "executing"
    case verifying = "verifying"
    
    var label: String {
        switch self {
        case .estimatingComplexity: return "Analyzing task"
        case .selectingModel: return "Selecting model"
        case .splittingTask: return "Decomposing task"
        case .executing: return "Executing"
        case .verifying: return "Verifying"
        }
    }
    
    var icon: String {
        switch self {
        case .estimatingComplexity: return "brain"
        case .selectingModel: return "cpu"
        case .splittingTask: return "arrow.triangle.branch"
        case .executing: return "play.circle"
        case .verifying: return "checkmark.shield"
        }
    }
}
