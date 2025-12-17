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
        let attributedText: AttributedString?
        
        enum LineType {
            case input
            case output
            case error
            case system
        }
        
        init(text: String, type: LineType) {
            self.text = text
            self.type = type
            self.attributedText = type == .output ? Self.parseANSI(text) : nil
        }
        
        var color: Color {
            switch type {
            case .input: return Theme.accent
            case .output: return Theme.textPrimary
            case .error: return Theme.error
            case .system: return Theme.textTertiary
            }
        }
        
        /// Parse ANSI escape codes and return AttributedString with colors
        private static func parseANSI(_ text: String) -> AttributedString? {
            // First, strip ALL non-SGR escape sequences (cursor movement, etc.)
            let cleanedText = stripNonColorEscapes(text)
            
            var result = AttributedString()
            var currentColor: Color = .white
            var currentBgColor: Color? = nil
            var isBold = false
            var isDim = false
            
            // Pattern to match SGR (color/style) escape sequences only
            let pattern = "\u{001B}\\[([0-9;]*)m"
            guard let regex = try? NSRegularExpression(pattern: pattern, options: []) else {
                // If regex fails, return plain text
                var attr = AttributedString(cleanedText)
                attr.foregroundColor = .white
                attr.font = .system(size: 13, weight: .regular, design: .monospaced)
                return attr
            }
            
            let nsText = cleanedText as NSString
            var lastEnd = 0
            let matches = regex.matches(in: cleanedText, options: [], range: NSRange(location: 0, length: nsText.length))
            
            for match in matches {
                // Add text before this escape sequence
                if match.range.location > lastEnd {
                    let textRange = NSRange(location: lastEnd, length: match.range.location - lastEnd)
                    let substring = nsText.substring(with: textRange)
                    if !substring.isEmpty {
                        var attr = AttributedString(substring)
                        attr.foregroundColor = isDim ? currentColor.opacity(0.6) : currentColor
                        attr.font = .system(size: 13, weight: isBold ? .bold : .regular, design: .monospaced)
                        if let bg = currentBgColor {
                            attr.backgroundColor = bg
                        }
                        result.append(attr)
                    }
                }
                
                // Parse the SGR codes
                if match.numberOfRanges > 1 {
                    let codeRange = match.range(at: 1)
                    let codeString = nsText.substring(with: codeRange)
                    let codes = codeString.isEmpty ? [0] : codeString.split(separator: ";").compactMap { Int($0) }
                    
                    var i = 0
                    while i < codes.count {
                        let code = codes[i]
                        switch code {
                        case 0: // Reset all
                            currentColor = .white
                            currentBgColor = nil
                            isBold = false
                            isDim = false
                        case 1: isBold = true
                        case 2: isDim = true
                        case 22: isBold = false; isDim = false
                        // Foreground colors (30-37, 90-97)
                        case 30: currentColor = Color(white: 0.2)
                        case 31: currentColor = Color(red: 0.94, green: 0.33, blue: 0.31)
                        case 32: currentColor = Color(red: 0.33, green: 0.86, blue: 0.43)
                        case 33: currentColor = Color(red: 0.98, green: 0.74, blue: 0.25)
                        case 34: currentColor = Color(red: 0.40, green: 0.57, blue: 0.93)
                        case 35: currentColor = Color(red: 0.83, green: 0.42, blue: 0.78)
                        case 36: currentColor = Color(red: 0.30, green: 0.82, blue: 0.87)
                        case 37: currentColor = Color(white: 0.9)
                        case 39: currentColor = .white // Default
                        case 90: currentColor = Color(white: 0.5)
                        case 91: currentColor = Color(red: 1, green: 0.45, blue: 0.45)
                        case 92: currentColor = Color(red: 0.45, green: 1, blue: 0.55)
                        case 93: currentColor = Color(red: 1, green: 0.9, blue: 0.45)
                        case 94: currentColor = Color(red: 0.55, green: 0.7, blue: 1)
                        case 95: currentColor = Color(red: 1, green: 0.55, blue: 0.95)
                        case 96: currentColor = Color(red: 0.45, green: 0.95, blue: 1)
                        case 97: currentColor = .white
                        // Background colors (40-47, 100-107)
                        case 40: currentBgColor = Color(white: 0.1)
                        case 41: currentBgColor = Color(red: 0.6, green: 0.15, blue: 0.15)
                        case 42: currentBgColor = Color(red: 0.15, green: 0.5, blue: 0.2)
                        case 43: currentBgColor = Color(red: 0.6, green: 0.45, blue: 0.1)
                        case 44: currentBgColor = Color(red: 0.15, green: 0.25, blue: 0.55)
                        case 45: currentBgColor = Color(red: 0.5, green: 0.2, blue: 0.45)
                        case 46: currentBgColor = Color(red: 0.1, green: 0.45, blue: 0.5)
                        case 47: currentBgColor = Color(white: 0.7)
                        case 49: currentBgColor = nil // Default bg
                        // 256 color mode (38;5;n or 48;5;n)
                        case 38:
                            if i + 2 < codes.count && codes[i + 1] == 5 {
                                currentColor = color256(codes[i + 2])
                                i += 2
                            }
                        case 48:
                            if i + 2 < codes.count && codes[i + 1] == 5 {
                                currentBgColor = color256(codes[i + 2])
                                i += 2
                            }
                        default: break
                        }
                        i += 1
                    }
                }
                
                lastEnd = match.range.location + match.range.length
            }
            
            // Add remaining text after last escape sequence
            if lastEnd < nsText.length {
                let textRange = NSRange(location: lastEnd, length: nsText.length - lastEnd)
                let substring = nsText.substring(with: textRange)
                if !substring.isEmpty {
                    var attr = AttributedString(substring)
                    attr.foregroundColor = isDim ? currentColor.opacity(0.6) : currentColor
                    attr.font = .system(size: 13, weight: isBold ? .bold : .regular, design: .monospaced)
                    if let bg = currentBgColor {
                        attr.backgroundColor = bg
                    }
                    result.append(attr)
                }
            }
            
            return result.characters.isEmpty ? nil : result
        }
        
        /// Strip all non-SGR escape sequences (cursor movement, screen clear, etc.)
        private static func stripNonColorEscapes(_ text: String) -> String {
            var result = text
            
            // Pattern 1: CSI sequences that are NOT color codes (not ending in 'm')
            // This catches [47C (cursor forward), [1G (cursor to column), [2J (clear), etc.
            // Using a more explicit pattern to catch all CSI commands except 'm'
            let csiPattern = "\\x1B\\[([0-9]*;?)*[ABCDEFGHIJKLPSTXZcfghilnqrsu@`]"
            if let regex = try? NSRegularExpression(pattern: csiPattern, options: []) {
                result = regex.stringByReplacingMatches(
                    in: result,
                    options: [],
                    range: NSRange(result.startIndex..., in: result),
                    withTemplate: ""
                )
            }
            
            // Pattern 2: Also catch any remaining [xxC or [xxA etc. patterns that might have slipped through
            // This is a fallback for malformed sequences
            let fallbackPattern = "\\[\\d+[A-Za-z]"
            if let regex = try? NSRegularExpression(pattern: fallbackPattern, options: []) {
                result = regex.stringByReplacingMatches(
                    in: result,
                    options: [],
                    range: NSRange(result.startIndex..., in: result),
                    withTemplate: ""
                )
            }
            
            // Pattern 3: OSC sequences (ESC ] ... BEL or ESC ] ... ST)
            let oscPattern = "\\x1B\\][^\\x07\\x1B]*(?:\\x07|\\x1B\\\\)?"
            if let regex = try? NSRegularExpression(pattern: oscPattern, options: []) {
                result = regex.stringByReplacingMatches(
                    in: result,
                    options: [],
                    range: NSRange(result.startIndex..., in: result),
                    withTemplate: ""
                )
            }
            
            // Pattern 4: Private mode sequences (ESC [ ? ... h/l)
            let privatePattern = "\\x1B\\[\\?[0-9;]*[hl]"
            if let regex = try? NSRegularExpression(pattern: privatePattern, options: []) {
                result = regex.stringByReplacingMatches(
                    in: result,
                    options: [],
                    range: NSRange(result.startIndex..., in: result),
                    withTemplate: ""
                )
            }
            
            // Pattern 5: Character set and single-char escapes
            let miscPattern = "\\x1B[\\(\\)][AB012]|\\x1B[78DEHM=>]"
            if let regex = try? NSRegularExpression(pattern: miscPattern, options: []) {
                result = regex.stringByReplacingMatches(
                    in: result,
                    options: [],
                    range: NSRange(result.startIndex..., in: result),
                    withTemplate: ""
                )
            }
            
            return result
        }
        
        /// Convert 256-color palette index to Color
        private static func color256(_ index: Int) -> Color {
            if index < 16 {
                // Standard colors
                let colors: [Color] = [
                    Color(white: 0.1), Color(red: 0.8, green: 0.2, blue: 0.2),
                    Color(red: 0.2, green: 0.8, blue: 0.3), Color(red: 0.8, green: 0.7, blue: 0.2),
                    Color(red: 0.3, green: 0.4, blue: 0.9), Color(red: 0.8, green: 0.3, blue: 0.7),
                    Color(red: 0.2, green: 0.7, blue: 0.8), Color(white: 0.85),
                    Color(white: 0.4), Color(red: 1, green: 0.4, blue: 0.4),
                    Color(red: 0.4, green: 1, blue: 0.5), Color(red: 1, green: 0.95, blue: 0.4),
                    Color(red: 0.5, green: 0.6, blue: 1), Color(red: 1, green: 0.5, blue: 0.9),
                    Color(red: 0.4, green: 0.95, blue: 1), .white
                ]
                return colors[index]
            } else if index < 232 {
                // 216 color cube (6x6x6)
                let n = index - 16
                let b = n % 6
                let g = (n / 6) % 6
                let r = n / 36
                return Color(
                    red: r == 0 ? 0 : Double(r * 40 + 55) / 255,
                    green: g == 0 ? 0 : Double(g * 40 + 55) / 255,
                    blue: b == 0 ? 0 : Double(b * 40 + 55) / 255
                )
            } else {
                // Grayscale (24 shades)
                let gray = Double((index - 232) * 10 + 8) / 255
                return Color(white: gray)
            }
        }
    }
    
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
                .background(Theme.backgroundSecondary)
                .clipShape(Capsule())
                .overlay(
                    Capsule()
                        .stroke(Theme.border, lineWidth: 1)
                )
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
                terminalOutput.append(TerminalLine(text: line, type: .output))
            }
        }
        
        // Limit history
        if terminalOutput.count > 1000 {
            terminalOutput.removeFirst(terminalOutput.count - 1000)
        }
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
