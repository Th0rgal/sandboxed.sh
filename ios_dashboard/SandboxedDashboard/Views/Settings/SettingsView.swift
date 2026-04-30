//
//  SettingsView.swift
//  SandboxedDashboard
//
//  Settings page for configuring server connection and app preferences
//

import SwiftUI

struct SettingsView: View {
    @Environment(\.dismiss) private var dismiss

    @State private var serverURL: String
    @State private var isTestingConnection = false
    @State private var connectionStatus: ConnectionStatus = .unknown
    @State private var showingSaveConfirmation = false
    
    // Default agent settings
    @State private var backends: [Backend] = Backend.defaults
    @State private var enabledBackendIds: Set<String> = ["opencode", "claudecode", "amp"]
    @State private var backendAgents: [String: [BackendAgent]] = [:]
    @State private var selectedDefaultAgent: String = ""
    @State private var isLoadingAgents = true
    @State private var skipAgentSelection = false
    @State private var showClearRulesConfirm = false

    private let api = APIService.shared
    private let originalURL: String

    enum ConnectionStatus: Equatable {
        case unknown
        case testing
        case success(authMode: String)
        case failure(message: String)

        var icon: String {
            switch self {
            case .unknown: return "questionmark.circle"
            case .testing: return "arrow.trianglehead.2.clockwise.rotate.90"
            case .success: return "checkmark.circle.fill"
            case .failure: return "xmark.circle.fill"
            }
        }

        var color: Color {
            switch self {
            case .unknown: return Theme.textSecondary
            case .testing: return Theme.accent
            case .success: return Theme.success
            case .failure: return Theme.error
            }
        }

        var message: String {
            switch self {
            case .unknown: return "Not tested"
            case .testing: return "Testing connection..."
            case .success(let authMode): return "Connected (\(authMode))"
            case .failure(let message): return message
            }
        }

        /// Header message for display above the URL field
        var headerMessage: String {
            switch self {
            case .unknown: return "Not tested"
            case .testing: return "Testing..."
            case .success(let authMode): return "Connected (\(authMode))"
            case .failure: return "Failed"
            }
        }
    }

    init() {
        let currentURL = APIService.shared.baseURL
        _serverURL = State(initialValue: currentURL)
        originalURL = currentURL
        _selectedDefaultAgent = State(initialValue: UserDefaults.standard.string(forKey: "default_agent") ?? "")
        _skipAgentSelection = State(initialValue: UserDefaults.standard.bool(forKey: "skip_agent_selection"))
    }

    var body: some View {
        NavigationStack {
            ZStack {
                Theme.backgroundPrimary.ignoresSafeArea()

                ScrollView {
                    VStack(spacing: 24) {
                        // Server Configuration Section
                        VStack(alignment: .leading, spacing: 16) {
                            Label("Server Configuration", systemImage: "server.rack")
                                .font(.headline)
                                .foregroundStyle(Theme.textPrimary)

                            GlassCard(padding: 20, cornerRadius: 20) {
                                VStack(alignment: .leading, spacing: 10) {
                                    // Header: "API URL" + status + refresh button
                                    HStack(spacing: 8) {
                                        Text("API URL")
                                            .font(.caption.weight(.medium))
                                            .foregroundStyle(Theme.textSecondary)

                                        Spacer()

                                        // Status indicator
                                        HStack(spacing: 5) {
                                            Circle()
                                                .fill(connectionStatus.color)
                                                .frame(width: 6, height: 6)

                                            Text(connectionStatus.headerMessage)
                                                .font(.caption2)
                                                .foregroundStyle(connectionStatus.color)
                                        }

                                        // Refresh button
                                        Button {
                                            Task { await testConnection() }
                                        } label: {
                                            Image(systemName: "arrow.clockwise")
                                                .font(.system(size: 11, weight: .medium))
                                                .foregroundStyle(connectionStatus == .testing ? Theme.accent : Theme.textMuted)
                                        }
                                        .disabled(serverURL.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || connectionStatus == .testing)
                                        .symbolEffect(.rotate, isActive: connectionStatus == .testing)
                                    }

                                    // URL input field
                                    TextField("https://your-server.com", text: $serverURL)
                                        .textFieldStyle(.plain)
                                        .textInputAutocapitalization(.never)
                                        .autocorrectionDisabled()
                                        .keyboardType(.URL)
                                        .submitLabel(.done)
                                        .onSubmit {
                                            Task { await testConnection() }
                                        }
                                        .padding(.horizontal, 16)
                                        .padding(.vertical, 14)
                                        .background(Color.white.opacity(0.05))
                                        .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                                        .overlay(
                                            RoundedRectangle(cornerRadius: 12, style: .continuous)
                                                .stroke(Theme.border, lineWidth: 1)
                                        )
                                        .onChange(of: serverURL) { _, _ in
                                            connectionStatus = .unknown
                                        }
                                }
                            }
                        }

                        // Mission Preferences Section
                        VStack(alignment: .leading, spacing: 16) {
                            Label("Mission Preferences", systemImage: "cpu")
                                .font(.headline)
                                .foregroundStyle(Theme.textPrimary)

                            GlassCard(padding: 20, cornerRadius: 20) {
                                VStack(alignment: .leading, spacing: 16) {
                                    // Skip agent selection toggle
                                    VStack(alignment: .leading, spacing: 8) {
                                        HStack {
                                            VStack(alignment: .leading, spacing: 4) {
                                                Text("Skip Agent Selection")
                                                    .font(.subheadline.weight(.medium))
                                                    .foregroundStyle(Theme.textPrimary)
                                                Text("Use default agent without prompting")
                                                    .font(.caption)
                                                    .foregroundStyle(Theme.textSecondary)
                                            }
                                            Spacer()
                                            Toggle("", isOn: $skipAgentSelection)
                                                .labelsHidden()
                                                .tint(Theme.accent)
                                        }
                                    }
                                    
                                    Divider()
                                        .background(Theme.border)
                                    
                                    // Default agent selection
                                    VStack(alignment: .leading, spacing: 8) {
                                        Text("Default Agent")
                                            .font(.caption.weight(.medium))
                                            .foregroundStyle(Theme.textSecondary)
                                        
                                        if isLoadingAgents {
                                            HStack {
                                                ProgressView()
                                                    .scaleEffect(0.8)
                                                Text("Loading agents...")
                                                    .font(.subheadline)
                                                    .foregroundStyle(Theme.textSecondary)
                                            }
                                            .padding(.vertical, 8)
                                        } else {
                                            defaultAgentPicker
                                        }
                                    }
                                }
                            }
                        }

                        // Security & Signing Section
                        VStack(alignment: .leading, spacing: 16) {
                            Label("Security & Signing", systemImage: "key.radiowaves.forward")
                                .font(.headline)
                                .foregroundStyle(Theme.textPrimary)

                            GlassCard(padding: 20, cornerRadius: 20) {
                                VStack(alignment: .leading, spacing: 16) {
                                    // Require Face ID toggle
                                    HStack {
                                        VStack(alignment: .leading, spacing: 4) {
                                            Text("Require Face ID for All Requests")
                                                .font(.subheadline.weight(.medium))
                                                .foregroundStyle(Theme.textPrimary)
                                            Text("Biometric authentication before approving")
                                                .font(.caption)
                                                .foregroundStyle(Theme.textSecondary)
                                        }
                                        Spacer()
                                        Toggle("", isOn: Binding(
                                            get: { FidoApprovalState.shared.requireBiometricForAll },
                                            set: { FidoApprovalState.shared.requireBiometricForAll = $0 }
                                        ))
                                            .labelsHidden()
                                            .tint(Theme.accent)
                                    }

                                    Divider()
                                        .background(Theme.border)

                                    if FidoApprovalState.shared.autoApprovalRules.isEmpty {
                                        // Without this hint, an empty Auto-Approval section is
                                        // invisible — users can't tell the feature exists, let
                                        // alone how rules accumulate. Surface the explanation
                                        // up-front so first-run state isn't a blank divider.
                                        HStack(alignment: .top, spacing: 10) {
                                            Image(systemName: "checkmark.shield")
                                                .font(.system(size: 16, weight: .medium))
                                                .foregroundStyle(Theme.textTertiary)
                                                .frame(width: 24)
                                            VStack(alignment: .leading, spacing: 4) {
                                                Text("No auto-approval rules yet")
                                                    .font(.subheadline.weight(.medium))
                                                    .foregroundStyle(Theme.textPrimary)
                                                Text("When you approve a signing request, you can save the choice as a rule so future identical requests skip the prompt.")
                                                    .font(.caption)
                                                    .foregroundStyle(Theme.textSecondary)
                                                    .fixedSize(horizontal: false, vertical: true)
                                            }
                                        }
                                    } else {
                                        AutoApprovalRulesView()

                                        Button {
                                            showClearRulesConfirm = true
                                            HapticService.lightTap()
                                        } label: {
                                            HStack {
                                                Image(systemName: "trash")
                                                Text("Clear All Rules")
                                            }
                                            .font(.subheadline.weight(.medium))
                                            .foregroundStyle(Theme.error)
                                            .frame(maxWidth: .infinity)
                                            .padding(.vertical, 10)
                                            .background(Theme.error.opacity(0.1))
                                            .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
                                        }
                                        .confirmationDialog(
                                            "Clear all auto-approval rules?",
                                            isPresented: $showClearRulesConfirm,
                                            titleVisibility: .visible
                                        ) {
                                            Button("Clear All Rules", role: .destructive) {
                                                withAnimation {
                                                    FidoApprovalState.shared.autoApprovalRules.removeAll()
                                                }
                                                HapticService.mediumTap()
                                            }
                                            Button("Cancel", role: .cancel) {}
                                        } message: {
                                            Text("This permanently removes every saved auto-approval rule. Future signing requests will require manual approval until you create new rules. This can't be undone.")
                                        }
                                    }
                                }
                            }
                        }

                        // About Section
                        VStack(alignment: .leading, spacing: 16) {
                            Label("About", systemImage: "info.circle")
                                .font(.headline)
                                .foregroundStyle(Theme.textPrimary)

                            GlassCard(padding: 20, cornerRadius: 20) {
                                VStack(alignment: .leading, spacing: 12) {
                                    HStack {
                                        Text("sandboxed.sh Dashboard")
                                            .font(.subheadline.weight(.medium))
                                            .foregroundStyle(Theme.textPrimary)
                                        Spacer()
                                        Text("v1.0")
                                            .font(.caption)
                                            .foregroundStyle(Theme.textSecondary)
                                    }

                                    Divider()
                                        .background(Theme.border)

                                    Text("A native iOS dashboard for managing sandboxed.sh workspaces and missions.")
                                        .font(.caption)
                                        .foregroundStyle(Theme.textSecondary)
                                }
                            }
                        }

                        Spacer(minLength: 40)
                    }
                    .padding(.horizontal, 20)
                    .padding(.top, 20)
                }
            }
            .navigationTitle("Settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button("Cancel") {
                        // Restore original URL on cancel
                        api.baseURL = originalURL
                        dismiss()
                    }
                    .foregroundStyle(Theme.textSecondary)
                }

                ToolbarItem(placement: .topBarTrailing) {
                    Button("Save") {
                        saveSettings()
                    }
                    .fontWeight(.semibold)
                    .foregroundStyle(Theme.accent)
                    .disabled(serverURL.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }
        }
        .presentationDetents([.large])
        .presentationDragIndicator(.visible)
        .task {
            await loadAgents()
        }
    }
    
    // MARK: - Default Agent Picker
    
    private var defaultAgentPicker: some View {
        VStack(spacing: 8) {
            // None option (clear default)
            defaultAgentRow(value: "", backendName: nil, agentName: "Always Ask")
            
            // Group agents by backend
            ForEach(backends.filter { enabledBackendIds.contains($0.id) }) { backend in
                let agents = backendAgents[backend.id] ?? []
                if !agents.isEmpty {
                    backendAgentSection(backend: backend, agents: agents)
                }
            }
        }
    }
    
    private func backendAgentSection(backend: Backend, agents: [BackendAgent]) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            // Backend header
            HStack(spacing: 6) {
                Image(systemName: backendIcon(for: backend.id))
                    .font(.caption2)
                    .foregroundStyle(backendColor(for: backend.id))
                Text(backend.name)
                    .font(.caption2.weight(.semibold))
                    .foregroundStyle(Theme.textSecondary)
            }
            .padding(.leading, 4)
            .padding(.top, 4)
            
            // Agents
            ForEach(agents) { agent in
                let value = "\(backend.id):\(agent.id)"
                defaultAgentRow(value: value, backendName: backend.name, agentName: agent.name)
            }
        }
    }
    
    private func defaultAgentRow(value: String, backendName: String?, agentName: String) -> some View {
        Button {
            selectedDefaultAgent = value
            HapticService.selectionChanged()
        } label: {
            HStack(spacing: 12) {
                // Icon
                ZStack {
                    Circle()
                        .fill(value.isEmpty ? Theme.textSecondary.opacity(0.15) : backendColor(for: CombinedAgent.parse(value)?.backend).opacity(0.15))
                        .frame(width: 28, height: 28)
                    
                    Image(systemName: value.isEmpty ? "questionmark" : "person.fill")
                        .font(.system(size: 10, weight: .medium))
                        .foregroundStyle(value.isEmpty ? Theme.textSecondary : backendColor(for: CombinedAgent.parse(value)?.backend))
                }
                
                // Name
                VStack(alignment: .leading, spacing: 2) {
                    Text(agentName)
                        .font(.subheadline)
                        .foregroundStyle(Theme.textPrimary)
                    
                    if let backendName = backendName {
                        Text(backendName)
                            .font(.caption2)
                            .foregroundStyle(Theme.textSecondary)
                    }
                }
                
                Spacer()
                
                // Selection indicator
                if selectedDefaultAgent == value {
                    Image(systemName: "checkmark.circle.fill")
                        .font(.system(size: 18))
                        .foregroundStyle(Theme.accent)
                }
            }
            .padding(8)
            .background(
                RoundedRectangle(cornerRadius: 8)
                    .fill(selectedDefaultAgent == value ? Theme.accent.opacity(0.08) : Color.clear)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 8)
                    .stroke(selectedDefaultAgent == value ? Theme.accent.opacity(0.3) : Color.clear, lineWidth: 1)
            )
        }
        .buttonStyle(.plain)
    }
    
    // MARK: - Helpers

    private func backendIcon(for id: String?) -> String {
        BackendAgentService.icon(for: id)
    }

    private func backendColor(for id: String?) -> Color {
        BackendAgentService.color(for: id)
    }

    private func loadAgents() async {
        isLoadingAgents = true
        defer { isLoadingAgents = false }

        let data = await BackendAgentService.loadBackendsAndAgents()
        backends = data.backends
        enabledBackendIds = data.enabledBackendIds
        backendAgents = data.backendAgents

        // Clear stale default if the saved agent no longer exists on the server.
        // Only validate when we actually received agents for the backend —
        // a nil/missing entry means the API was unreachable, so we keep the
        // saved preference rather than falsely clearing it.
        if !selectedDefaultAgent.isEmpty,
           let parsed = CombinedAgent.parse(selectedDefaultAgent),
           let agentsForBackend = data.backendAgents[parsed.backend] {
            if !agentsForBackend.contains(where: { $0.id == parsed.agent }) {
                selectedDefaultAgent = ""
            }
        }
    }

    private func testConnection() async {
        let trimmedURL = serverURL.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedURL.isEmpty else { return }

        connectionStatus = .testing

        // Temporarily set the URL to test
        let originalURL = api.baseURL
        api.baseURL = trimmedURL

        do {
            _ = try await api.checkHealth()
            let modeString: String
            switch api.authMode {
            case .disabled:
                modeString = "no auth"
            case .singleTenant:
                modeString = "single tenant"
            case .multiUser:
                modeString = "multi-user"
            }
            connectionStatus = .success(authMode: modeString)
        } catch {
            connectionStatus = .failure(message: error.localizedDescription)
            // Restore original URL on failure
            api.baseURL = originalURL
        }
    }

    private func saveSettings() {
        let trimmedURL = serverURL.trimmingCharacters(in: .whitespacesAndNewlines)
        api.baseURL = trimmedURL

        // Save mission preferences
        UserDefaults.standard.set(selectedDefaultAgent, forKey: "default_agent")
        UserDefaults.standard.set(skipAgentSelection, forKey: "skip_agent_selection")

        // Invalidate the cached backend/agent data so the next validation
        // picks up any server URL or configuration changes.
        BackendAgentService.invalidateCache()

        HapticService.success()
        dismiss()
    }
}

#Preview {
    SettingsView()
}
