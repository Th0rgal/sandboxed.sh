//
//  TerminalView.swift
//  SandboxedDashboard
//
//  SSH terminal with WebSocket connection
//

import SwiftUI

struct TerminalView: View {
    private var state = TerminalState.shared
    private var workspaceState = WorkspaceState.shared
    @State private var inputText = ""

    @FocusState private var isInputFocused: Bool

    private let api = APIService.shared
    
    // Convenience accessors
    private var terminalOutput: [TerminalLine] { state.terminalOutput }
    private var connectionStatus: StatusType { state.connectionStatus }
    private var isConnecting: Bool { state.isConnecting }
    
    var body: some View {
        ZStack(alignment: .top) {
            // Terminal background
            Color(red: 0.04, green: 0.04, blue: 0.05)
                .ignoresSafeArea()
            
            VStack(spacing: 0) {
                // Terminal output (full height)
                terminalOutputView
                
                // Input field
                inputView
            }
            
            // Floating connection header (overlay)
            connectionHeader
        }
        .navigationTitle("Terminal")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                // Workspace selector
                Menu {
                    ForEach(workspaceState.workspaces) { workspace in
                        Button {
                            workspaceState.selectWorkspace(id: workspace.id)
                            // Reconnect to the new workspace
                            disconnect()
                            DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) {
                                connect()
                            }
                            HapticService.selectionChanged()
                        } label: {
                            HStack {
                                Label(workspace.displayLabel, systemImage: workspace.workspaceType.icon)
                                if workspaceState.selectedWorkspace?.id == workspace.id {
                                    Spacer()
                                    Image(systemName: "checkmark")
                                }
                            }
                        }
                    }
                } label: {
                    Image(systemName: "square.stack.3d.up")
                        .font(.system(size: 16))
                        .foregroundStyle(Theme.textSecondary)
                }
            }

            ToolbarItem(placement: .topBarTrailing) {
                // Unified status pill
                HStack(spacing: 0) {
                    // Status side
                    HStack(spacing: 5) {
                        Circle()
                            .fill(connectionStatus == .connected ? Theme.success : Theme.textMuted)
                            .frame(width: 6, height: 6)
                        Text(connectionStatus == .connected ? "Live" : "Off")
                            .font(.caption.weight(.medium))
                            .foregroundStyle(connectionStatus == .connected ? Theme.success : Theme.textSecondary)
                    }
                    .padding(.horizontal, 10)
                    .padding(.vertical, 6)
                    .background(connectionStatus == .connected ? Theme.success.opacity(0.15) : Color.clear)

                    // Divider
                    Rectangle()
                        .fill(Theme.border)
                        .frame(width: 1)

                    // Action side
                    Button {
                        if connectionStatus == .connected {
                            disconnect()
                        } else {
                            connect()
                        }
                    } label: {
                        Text(connectionStatus == .connected ? "End" : "Connect")
                            .font(.caption.weight(.medium))
                            .foregroundStyle(connectionStatus == .connected ? Theme.error : Theme.accent)
                            .padding(.horizontal, 10)
                            .padding(.vertical, 6)
                    }
                }
                .clipShape(Capsule())
                .overlay(
                    Capsule()
                        .stroke(Theme.border, lineWidth: 0.5)
                )
            }
        }
        .task {
            // Load workspaces if not already loaded
            if workspaceState.workspaces.isEmpty {
                await workspaceState.loadWorkspaces()
            }
            connect()
        }
        .onDisappear {
            disconnect()
        }
    }
    
    private var connectionHeader: some View {
        // Only show reconnect overlay when disconnected
        Group {
            if connectionStatus != .connected && !isConnecting {
                VStack(spacing: 16) {
                    Spacer()
                    
                    VStack(spacing: 12) {
                        Image(systemName: "wifi.slash")
                            .font(.system(size: 32))
                            .foregroundStyle(Theme.textMuted)
                        
                        Text("Disconnected")
                            .font(.headline)
                            .foregroundStyle(Theme.textSecondary)
                        
                        Button {
                            connect()
                        } label: {
                            HStack(spacing: 8) {
                                Image(systemName: "arrow.clockwise")
                                Text("Reconnect")
                            }
                            .font(.subheadline.weight(.semibold))
                            .foregroundStyle(.white)
                            .padding(.horizontal, 20)
                            .padding(.vertical, 12)
                            .background(Theme.accent)
                            .clipShape(Capsule())
                        }
                    }
                    .padding(32)
                    .background(.ultraThinMaterial)
                    .clipShape(RoundedRectangle(cornerRadius: 20, style: .continuous))
                    
                    Spacer()
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(Color.black.opacity(0.5))
            } else if isConnecting {
                VStack {
                    Spacer()
                    ProgressView()
                        .scaleEffect(1.5)
                        .tint(Theme.accent)
                    Spacer()
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(Color.black.opacity(0.3))
            }
        }
    }
    
    private var terminalOutputView: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(terminalOutput) { line in
                        Group {
                            if let attributed = line.attributedText {
                                Text(attributed)
                            } else {
                                Text(line.text)
                                    .font(.system(size: 13, weight: .regular, design: .monospaced))
                                    .foregroundStyle(line.color)
                            }
                        }
                        .textSelection(.enabled)
                        .id(line.id)
                    }
                }
                .padding(.horizontal, 12)
                .padding(.top, 8)
                .padding(.bottom, 80) // Space for input
                .frame(maxWidth: .infinity, alignment: .leading)
            }
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
        HStack(spacing: 8) {
            Text("$")
                .font(.system(size: 15, weight: .bold, design: .monospaced))
                .foregroundStyle(Theme.success)
            
            TextField("", text: $inputText, prompt: Text("command").foregroundStyle(Color.white.opacity(0.3)))
                .textFieldStyle(.plain)
                .font(.system(size: 15, weight: .regular, design: .monospaced))
                .foregroundStyle(.white)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .focused($isInputFocused)
                .submitLabel(.send)
                .onSubmit {
                    sendCommand()
                }
            
            if !inputText.isEmpty {
                Button {
                    sendCommand()
                } label: {
                    Image(systemName: "arrow.right.circle.fill")
                        .font(.title2)
                        .foregroundStyle(Theme.accent)
                }
                .disabled(connectionStatus != .connected)
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .background(Color(red: 0.08, green: 0.08, blue: 0.1))
        .overlay(
            Rectangle()
                .frame(height: 1)
                .foregroundStyle(Color.white.opacity(0.1)),
            alignment: .top
        )
    }
    
    // MARK: - WebSocket Connection
    
    private func connect() {
        guard state.connectionStatus != .connected && !state.isConnecting else { return }

        state.isConnecting = true
        state.connectionStatus = .connecting

        let workspaceName = workspaceState.currentWorkspaceLabel
        state.appendLine(TerminalLine(text: "Connecting to \(workspaceName)...", type: .system))
        
        guard let wsURL = buildWebSocketURL() else {
            state.appendLine(TerminalLine(text: "Invalid WebSocket URL", type: .error))
            state.connectionStatus = .error
            state.isConnecting = false
            return
        }
        
        var request = URLRequest(url: wsURL)
        
        // Add auth via subprotocol if available
        if let token = UserDefaults.standard.string(forKey: "jwt_token") {
            request.setValue("sandboxed, jwt.\(token)", forHTTPHeaderField: "Sec-WebSocket-Protocol")
        }
        
        state.webSocketTask = URLSession.shared.webSocketTask(with: request)
        state.webSocketTask?.resume()
        
        // Start receiving messages
        receiveMessages()
        
        // Send initial resize message after a brief delay
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
            if state.connectionStatus == .connecting {
                state.connectionStatus = .connected
                state.appendLine(TerminalLine(text: "Connected.", type: .system))
            }
            state.isConnecting = false
            sendResize(cols: 80, rows: 24)
        }
    }
    
    private func disconnect() {
        state.webSocketTask?.cancel(with: .normalClosure, reason: nil)
        state.webSocketTask = nil
        state.connectionStatus = .disconnected
        state.appendLine(TerminalLine(text: "Disconnected.", type: .system))
    }
    
    private func buildWebSocketURL() -> URL? {
        guard var components = URLComponents(string: api.baseURL) else { return nil }
        components.scheme = components.scheme == "https" ? "wss" : "ws"

        // Use workspace-specific shell if a non-default workspace is selected
        if let workspace = workspaceState.selectedWorkspace, !workspace.isDefault {
            components.path = "/api/workspaces/\(workspace.id)/shell"
        } else {
            components.path = "/api/console/ws"
        }

        return components.url
    }
    
    private func receiveMessages() {
        state.webSocketTask?.receive { [self] result in
            switch result {
            case .success(let message):
                switch message {
                case .string(let text):
                    Task { @MainActor in
                        self.handleOutput(text)
                    }
                case .data(let data):
                    if let text = String(data: data, encoding: .utf8) {
                        Task { @MainActor in
                            self.handleOutput(text)
                        }
                    }
                @unknown default:
                    break
                }
                // Continue receiving
                Task { @MainActor in
                    receiveMessages()
                }
                
            case .failure(let error):
                Task { @MainActor in
                    if state.connectionStatus != .disconnected {
                        state.connectionStatus = .error
                        state.appendLine(TerminalLine(text: "Connection error: \(error.localizedDescription)", type: .error))
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
                state.appendLine(TerminalLine(text: line, type: .output))
            }
        }
    }
    
    private func sendCommand() {
        guard !inputText.isEmpty, state.connectionStatus == .connected else { return }
        
        let command = inputText
        inputText = ""
        
        // Show the command in output
        state.appendLine(TerminalLine(text: "$ \(command)", type: .input))
        
        // Send to WebSocket
        let message = ["t": "i", "d": command + "\n"]
        if let data = try? JSONSerialization.data(withJSONObject: message),
           let jsonString = String(data: data, encoding: .utf8) {
            state.webSocketTask?.send(.string(jsonString)) { error in
                if let error = error {
                    Task { @MainActor in
                        state.appendLine(TerminalLine(text: "Send error: \(error.localizedDescription)", type: .error))
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
            state.webSocketTask?.send(.string(jsonString)) { _ in }
        }
    }
}

#Preview {
    NavigationStack {
        TerminalView()
    }
}
