//
//  TerminalView.swift
//  OpenAgentDashboard
//
//  SSH terminal with WebSocket connection
//

import SwiftUI

struct TerminalView: View {
    @State private var terminalOutput: [TerminalLine] = []
    @State private var inputText = ""
    @State private var connectionStatus: StatusType = .disconnected
    @State private var webSocketTask: URLSessionWebSocketTask?
    @State private var isConnecting = false
    
    @FocusState private var isInputFocused: Bool
    
    private let api = APIService.shared
    
    struct TerminalLine: Identifiable {
        let id = UUID()
        let text: String
        let type: LineType
        
        enum LineType {
            case input
            case output
            case error
            case system
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
    
    var body: some View {
        ZStack {
            Theme.backgroundPrimary.ignoresSafeArea()
            
            VStack(spacing: 0) {
                // Connection status header
                connectionHeader
                
                // Terminal output
                terminalOutputView
                
                // Input field
                inputView
            }
        }
        .navigationTitle("Terminal")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button {
                    if connectionStatus == .connected {
                        disconnect()
                    } else {
                        connect()
                    }
                } label: {
                    Text(connectionStatus == .connected ? "Disconnect" : "Connect")
                        .font(.subheadline.weight(.medium))
                }
            }
        }
        .onAppear {
            connect()
        }
        .onDisappear {
            disconnect()
        }
    }
    
    private var connectionHeader: some View {
        HStack(spacing: 10) {
            StatusDot(status: connectionStatus, size: 8)
            
            Text(connectionStatus.label)
                .font(.subheadline)
                .foregroundStyle(Theme.textSecondary)
            
            Spacer()
            
            if connectionStatus != .connected {
                Button {
                    connect()
                } label: {
                    HStack(spacing: 6) {
                        if isConnecting {
                            ProgressView()
                                .scaleEffect(0.7)
                        } else {
                            Image(systemName: "arrow.clockwise")
                        }
                        Text("Reconnect")
                    }
                    .font(.caption.weight(.medium))
                    .foregroundStyle(Theme.accent)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 6)
                    .background(Theme.accent.opacity(0.15))
                    .clipShape(Capsule())
                }
                .disabled(isConnecting)
            }
        }
        .padding(.horizontal)
        .padding(.vertical, 10)
        .background(.ultraThinMaterial)
    }
    
    private var terminalOutputView: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 1) {
                    ForEach(terminalOutput) { line in
                        Text(line.text)
                            .font(.system(size: 13, weight: .regular, design: .monospaced))
                            .foregroundStyle(line.color)
                            .textSelection(.enabled)
                            .id(line.id)
                    }
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .background(Color(red: 0.05, green: 0.05, blue: 0.07))
            .onChange(of: terminalOutput.count) { _, _ in
                if let lastLine = terminalOutput.last {
                    withAnimation(.easeOut(duration: 0.1)) {
                        proxy.scrollTo(lastLine.id, anchor: .bottom)
                    }
                }
            }
        }
    }
    
    private var inputView: some View {
        VStack(spacing: 0) {
            Divider()
                .background(Theme.border)
            
            HStack(spacing: 12) {
                Text("$")
                    .font(.system(.body, design: .monospaced))
                    .foregroundStyle(Theme.accent)
                
                TextField("Enter command...", text: $inputText)
                    .textFieldStyle(.plain)
                    .font(.system(.body, design: .monospaced))
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .focused($isInputFocused)
                    .onSubmit {
                        sendCommand()
                    }
                
                Button {
                    sendCommand()
                } label: {
                    Image(systemName: "return")
                        .font(.body)
                        .foregroundStyle(inputText.isEmpty ? Theme.textMuted : Theme.accent)
                        .frame(width: 36, height: 36)
                        .background(Theme.accent.opacity(inputText.isEmpty ? 0.1 : 0.2))
                        .clipShape(Circle())
                }
                .disabled(inputText.isEmpty || connectionStatus != .connected)
            }
            .padding()
            .background(.ultraThinMaterial)
        }
    }
    
    // MARK: - WebSocket Connection
    
    private func connect() {
        guard connectionStatus != .connected && !isConnecting else { return }
        
        isConnecting = true
        connectionStatus = .connecting
        addSystemLine("Connecting to \(api.baseURL)...")
        
        guard let wsURL = buildWebSocketURL() else {
            addErrorLine("Invalid WebSocket URL")
            connectionStatus = .error
            isConnecting = false
            return
        }
        
        var request = URLRequest(url: wsURL)
        
        // Add auth via subprotocol if available
        if let token = UserDefaults.standard.string(forKey: "jwt_token") {
            request.setValue("openagent, jwt.\(token)", forHTTPHeaderField: "Sec-WebSocket-Protocol")
        }
        
        webSocketTask = URLSession.shared.webSocketTask(with: request)
        webSocketTask?.resume()
        
        // Start receiving messages
        receiveMessages()
        
        // Send initial resize message after a brief delay
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
            if connectionStatus == .connecting {
                connectionStatus = .connected
                addSystemLine("Connected.")
            }
            isConnecting = false
            sendResize(cols: 80, rows: 24)
        }
    }
    
    private func disconnect() {
        webSocketTask?.cancel(with: .normalClosure, reason: nil)
        webSocketTask = nil
        connectionStatus = .disconnected
        addSystemLine("Disconnected.")
    }
    
    private func buildWebSocketURL() -> URL? {
        guard var components = URLComponents(string: api.baseURL) else { return nil }
        components.scheme = components.scheme == "https" ? "wss" : "ws"
        components.path = "/api/console/ws"
        return components.url
    }
    
    private func receiveMessages() {
        webSocketTask?.receive { [self] result in
            switch result {
            case .success(let message):
                switch message {
                case .string(let text):
                    DispatchQueue.main.async {
                        self.handleOutput(text)
                    }
                case .data(let data):
                    if let text = String(data: data, encoding: .utf8) {
                        DispatchQueue.main.async {
                            self.handleOutput(text)
                        }
                    }
                @unknown default:
                    break
                }
                // Continue receiving
                receiveMessages()
                
            case .failure(let error):
                DispatchQueue.main.async {
                    if connectionStatus != .disconnected {
                        connectionStatus = .error
                        addErrorLine("Connection error: \(error.localizedDescription)")
                    }
                }
            }
        }
    }
    
    private func handleOutput(_ text: String) {
        // Split by newlines and add each line
        let lines = text.components(separatedBy: .newlines)
        for line in lines {
            if !line.isEmpty {
                // Strip ANSI escape codes for clean display
                let cleanLine = stripANSICodes(line)
                if !cleanLine.isEmpty {
                    terminalOutput.append(TerminalLine(text: cleanLine, type: .output))
                }
            }
        }
        
        // Limit history
        if terminalOutput.count > 1000 {
            terminalOutput.removeFirst(terminalOutput.count - 1000)
        }
    }
    
    /// Strip ANSI escape codes from terminal output
    private func stripANSICodes(_ text: String) -> String {
        // Match ANSI escape sequences: ESC[...m, ESC[...H, ESC[...J, etc.
        let pattern = "\\x1B\\[[0-9;]*[A-Za-z]|\\x1B\\][^\\x07]*\\x07|\\x1B[\\(\\)][AB012]"
        guard let regex = try? NSRegularExpression(pattern: pattern, options: []) else {
            return text
        }
        let range = NSRange(text.startIndex..., in: text)
        return regex.stringByReplacingMatches(in: text, options: [], range: range, withTemplate: "")
    }
    
    private func sendCommand() {
        guard !inputText.isEmpty, connectionStatus == .connected else { return }
        
        let command = inputText
        inputText = ""
        
        // Show the command in output
        terminalOutput.append(TerminalLine(text: "$ \(command)", type: .input))
        
        // Send to WebSocket
        let message = ["t": "i", "d": command + "\n"]
        if let data = try? JSONSerialization.data(withJSONObject: message),
           let jsonString = String(data: data, encoding: .utf8) {
            webSocketTask?.send(.string(jsonString)) { error in
                if let error = error {
                    DispatchQueue.main.async {
                        addErrorLine("Send error: \(error.localizedDescription)")
                    }
                }
            }
        }
        
        HapticService.lightTap()
    }
    
    private func sendResize(cols: Int, rows: Int) {
        let message = ["t": "r", "c": cols, "r": rows] as [String: Any]
        if let data = try? JSONSerialization.data(withJSONObject: message),
           let jsonString = String(data: data, encoding: .utf8) {
            webSocketTask?.send(.string(jsonString)) { _ in }
        }
    }
    
    private func addSystemLine(_ text: String) {
        terminalOutput.append(TerminalLine(text: text, type: .system))
    }
    
    private func addErrorLine(_ text: String) {
        terminalOutput.append(TerminalLine(text: text, type: .error))
    }
}

#Preview {
    NavigationStack {
        TerminalView()
    }
}
