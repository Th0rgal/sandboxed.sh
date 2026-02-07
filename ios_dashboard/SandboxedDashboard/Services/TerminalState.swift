//
//  TerminalState.swift
//  SandboxedDashboard
//
//  Persistent terminal state that survives tab switches
//

import SwiftUI

@MainActor
@Observable
final class TerminalState {
    static let shared = TerminalState()
    
    var terminalOutput: [TerminalLine] = []
    var connectionStatus: StatusType = .disconnected
    var webSocketTask: URLSessionWebSocketTask?
    var isConnecting = false
    
    private init() {}
    
    func appendLine(_ line: TerminalLine) {
        terminalOutput.append(line)
        // Limit buffer size to prevent memory issues
        if terminalOutput.count > 2000 {
            terminalOutput.removeFirst(500)
        }
    }
    
    func appendOutput(_ text: String) {
        let line = TerminalLine(text: text, type: .output)
        appendLine(line)
    }
    
    func appendInput(_ text: String) {
        let line = TerminalLine(text: "$ \(text)", type: .input)
        appendLine(line)
    }
    
    func appendError(_ text: String) {
        let line = TerminalLine(text: text, type: .error)
        appendLine(line)
    }
    
    func clear() {
        terminalOutput = []
    }
}

// TerminalLine struct using the proper ANSI parser
struct TerminalLine: Identifiable {
    let id = UUID()
    let text: String
    let type: LineType
    let timestamp = Date()
    var attributedText: AttributedString?
    
    enum LineType {
        case input
        case output
        case error
        case system
    }
    
    init(text: String, type: LineType) {
        self.text = text
        self.type = type
        
        // Parse ANSI for output lines using the proper state machine parser
        if type == .output {
            self.attributedText = ANSIParser.parse(text)
        }
    }
    
    var color: Color {
        switch type {
        case .input: return Theme.accent
        case .output: return Theme.textPrimary
        case .error: return Theme.error
        case .system: return Theme.textTertiary
        }
    }
}
