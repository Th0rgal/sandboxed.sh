//
//  NewMissionSheet.swift
//  SandboxedDashboard
//
//  Sheet for creating a new mission with workspace, backend, agent, and model selection
//

import SwiftUI

struct NewMissionSheet: View {
    let workspaces: [Workspace]
    @Binding var selectedWorkspaceId: String?
    let onCreate: (NewMissionOptions) -> Void
    let onCancel: () -> Void
    
    // Backend and agent selection
    @State private var backends: [Backend] = Backend.defaults
    @State private var enabledBackendIds: Set<String> = ["opencode", "claudecode", "amp", "codex", "gemini", "grok"]
    @State private var backendAgents: [String: [BackendAgent]] = [:]
    @State private var selectedAgentValue: String = ""
    
    // Model override
    @State private var providers: [Provider] = []
    @State private var selectedModelOverride: String = ""
    
    // Loading state
    @State private var isLoading = true
    
    private let api = APIService.shared

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                // Header
                VStack(spacing: 8) {
                    Image(systemName: "plus.circle.fill")
                        .font(.system(size: 48))
                        .foregroundStyle(Theme.accent)

                    Text("New Mission")
                        .font(.title2.weight(.semibold))
                        .foregroundStyle(Theme.textPrimary)

                    Text("Configure your mission settings")
                        .font(.subheadline)
                        .foregroundStyle(Theme.textSecondary)
                }
                .padding(.top, 24)
                .padding(.bottom, 24)

                // Form
                ScrollView {
                    VStack(spacing: 20) {
                        // Workspace selection
                        sectionCard(title: "Workspace", icon: "server.rack") {
                            workspaceSelector
                        }
                        
                        // Agent selection (includes backend)
                        sectionCard(title: "Agent", icon: "cpu") {
                            agentSelector
                        }
                        
                        // Model override
                        sectionCard(title: "Model Override", icon: "slider.horizontal.3") {
                            modelSelector
                        }
                    }
                    .padding(.horizontal, 16)
                }

                Spacer()

                // Action buttons
                VStack(spacing: 12) {
                    Button {
                        let parsed = CombinedAgent.parse(selectedAgentValue)
                        onCreate(NewMissionOptions(
                            workspaceId: selectedWorkspaceId,
                            agent: parsed?.agent,
                            modelOverride: selectedModelOverride.isEmpty ? nil : selectedModelOverride,
                            backend: parsed?.backend ?? "opencode"
                        ))
                    } label: {
                        HStack {
                            Image(systemName: "play.fill")
                            Text("Start Mission")
                        }
                        .font(.body.weight(.semibold))
                        .foregroundStyle(.white)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 14)
                        .background(Theme.accent)
                        .clipShape(RoundedRectangle(cornerRadius: 12))
                    }
                    .disabled(workspaces.isEmpty || isLoading)
                    .opacity(workspaces.isEmpty || isLoading ? 0.5 : 1)

                    Button {
                        onCancel()
                    } label: {
                        Text("Cancel")
                            .font(.body)
                            .foregroundStyle(Theme.textSecondary)
                    }
                }
                .padding(.horizontal, 16)
                .padding(.bottom, 24)
            }
            .background(Theme.backgroundSecondary)
        }
        .task {
            await loadData()
        }
    }
    
    // MARK: - Sections
    
    private func sectionCard<Content: View>(
        title: String,
        icon: String,
        @ViewBuilder content: () -> Content
    ) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                Image(systemName: icon)
                    .font(.system(size: 14, weight: .medium))
                    .foregroundStyle(Theme.accent)
                Text(title)
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(Theme.textPrimary)
            }
            
            content()
        }
        .padding(16)
        .background(Theme.backgroundTertiary)
        .clipShape(RoundedRectangle(cornerRadius: 12))
    }
    
    // MARK: - Workspace Selector
    
    private var workspaceSelector: some View {
        VStack(spacing: 8) {
            if workspaces.isEmpty {
                HStack {
                    Image(systemName: "exclamationmark.triangle")
                        .foregroundStyle(Theme.warning)
                    Text("No workspaces available")
                        .font(.subheadline)
                        .foregroundStyle(Theme.textSecondary)
                }
                .padding(.vertical, 8)
            } else {
                ForEach(workspaces) { workspace in
                    workspaceRow(workspace)
                }
            }
        }
    }
    
    private func workspaceRow(_ workspace: Workspace) -> some View {
        Button {
            selectedWorkspaceId = workspace.id
            HapticService.selectionChanged()
        } label: {
            HStack(spacing: 12) {
                // Icon
                ZStack {
                    Circle()
                        .fill(workspace.workspaceType == .host ? Theme.success.opacity(0.15) : Theme.accent.opacity(0.15))
                        .frame(width: 36, height: 36)

                    Image(systemName: workspace.workspaceType.icon)
                        .font(.system(size: 14, weight: .medium))
                        .foregroundStyle(workspace.workspaceType == .host ? Theme.success : Theme.accent)
                }

                // Info
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text(workspace.name)
                            .font(.subheadline.weight(.medium))
                            .foregroundStyle(Theme.textPrimary)

                        if workspace.isDefault {
                            Text("Default")
                                .font(.caption2.weight(.medium))
                                .foregroundStyle(Theme.textSecondary)
                                .padding(.horizontal, 6)
                                .padding(.vertical, 2)
                                .background(Theme.backgroundSecondary)
                                .clipShape(RoundedRectangle(cornerRadius: 4))
                        }
                    }

                    Text(workspace.shortDescription)
                        .font(.caption)
                        .foregroundStyle(Theme.textSecondary)
                }

                Spacer()

                // Selection indicator
                selectionIndicator(isSelected: selectedWorkspaceId == workspace.id)
            }
            .padding(10)
            .background(
                RoundedRectangle(cornerRadius: 10)
                    .fill(selectedWorkspaceId == workspace.id ? Theme.accent.opacity(0.08) : Color.clear)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 10)
                    .stroke(selectedWorkspaceId == workspace.id ? Theme.accent.opacity(0.3) : Color.clear, lineWidth: 1)
            )
        }
        .buttonStyle(.plain)
    }
    
    // MARK: - Agent Selector
    
    private var agentSelector: some View {
        VStack(spacing: 8) {
            if isLoading {
                HStack {
                    ProgressView()
                        .scaleEffect(0.8)
                    Text("Loading agents...")
                        .font(.subheadline)
                        .foregroundStyle(Theme.textSecondary)
                }
                .padding(.vertical, 8)
            } else {
                // Group agents by backend
                ForEach(backends.filter { enabledBackendIds.contains($0.id) }) { backend in
                    let agents = backendAgents[backend.id] ?? []
                    if !agents.isEmpty {
                        backendSection(backend: backend, agents: agents)
                    }
                }
            }
        }
    }
    
    private func backendSection(backend: Backend, agents: [BackendAgent]) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            // Backend header
            HStack(spacing: 6) {
                Image(systemName: backendIcon(for: backend.id))
                    .font(.caption)
                    .foregroundStyle(backendColor(for: backend.id))
                Text(backend.name)
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(Theme.textSecondary)
            }
            .padding(.leading, 4)
            
            // Agents (use agent.id for CLI value, not display name)
            ForEach(agents) { agent in
                let value = "\(backend.id):\(agent.id)"
                agentRow(agent: agent, backend: backend, value: value)
            }
        }
    }
    
    private func agentRow(agent: BackendAgent, backend: Backend, value: String) -> some View {
        Button {
            selectedAgentValue = value
            // Reset model override if switching away from OpenCode (which uses provider/model format)
            if backend.id != "opencode" {
                if selectedModelOverride.contains("/") {
                    selectedModelOverride = ""
                }
            }
            HapticService.selectionChanged()
        } label: {
            HStack(spacing: 12) {
                // Agent icon
                ZStack {
                    Circle()
                        .fill(backendColor(for: backend.id).opacity(0.15))
                        .frame(width: 32, height: 32)
                    
                    Image(systemName: "person.fill")
                        .font(.system(size: 12, weight: .medium))
                        .foregroundStyle(backendColor(for: backend.id))
                }
                
                // Name
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text(agent.name)
                            .font(.subheadline.weight(.medium))
                            .foregroundStyle(Theme.textPrimary)
                        
                    }
                }
                
                Spacer()
                
                // Selection indicator
                selectionIndicator(isSelected: selectedAgentValue == value)
            }
            .padding(8)
            .background(
                RoundedRectangle(cornerRadius: 8)
                    .fill(selectedAgentValue == value ? backendColor(for: backend.id).opacity(0.08) : Color.clear)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 8)
                    .stroke(selectedAgentValue == value ? backendColor(for: backend.id).opacity(0.3) : Color.clear, lineWidth: 1)
            )
        }
        .buttonStyle(.plain)
    }
    
    // MARK: - Model Selector
    
    private var modelSelector: some View {
        VStack(spacing: 8) {
            // Default option
            modelRow(id: "", name: "Default (agent or global)", provider: nil)
            
            // Filter providers based on selected backend
            let selectedBackend = CombinedAgent.parse(selectedAgentValue)?.backend
            let filteredProviders = filterProviders(for: selectedBackend)
            
            ForEach(filteredProviders) { provider in
                providerSection(provider: provider, selectedBackend: selectedBackend)
            }
        }
    }
    
    private func providerSection(provider: Provider, selectedBackend: String?) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            // Provider header
            Text(provider.name)
                .font(.caption.weight(.semibold))
                .foregroundStyle(Theme.textSecondary)
                .padding(.leading, 4)
                .padding(.top, 8)
            
            ForEach(provider.models) { model in
                // Only OpenCode uses provider/model format; all other backends use raw model IDs
                let value = selectedBackend == "opencode"
                    ? "\(provider.id)/\(model.id)"
                    : model.id
                modelRow(id: value, name: model.name, provider: provider)
            }
        }
    }
    
    private func modelRow(id: String, name: String, provider: Provider?) -> some View {
        Button {
            selectedModelOverride = id
            HapticService.selectionChanged()
        } label: {
            HStack(spacing: 12) {
                // Icon
                ZStack {
                    Circle()
                        .fill(Theme.accent.opacity(0.15))
                        .frame(width: 28, height: 28)
                    
                    Image(systemName: id.isEmpty ? "sparkles" : "cpu")
                        .font(.system(size: 10, weight: .medium))
                        .foregroundStyle(Theme.accent)
                }
                
                Text(name)
                    .font(.subheadline)
                    .foregroundStyle(Theme.textPrimary)
                
                Spacer()
                
                selectionIndicator(isSelected: selectedModelOverride == id)
            }
            .padding(8)
            .background(
                RoundedRectangle(cornerRadius: 8)
                    .fill(selectedModelOverride == id ? Theme.accent.opacity(0.08) : Color.clear)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 8)
                    .stroke(selectedModelOverride == id ? Theme.accent.opacity(0.3) : Color.clear, lineWidth: 1)
            )
        }
        .buttonStyle(.plain)
    }
    
    // MARK: - Helpers
    
    private func selectionIndicator(isSelected: Bool) -> some View {
        ZStack {
            Circle()
                .stroke(isSelected ? Theme.accent : Theme.borderSubtle, lineWidth: 2)
                .frame(width: 20, height: 20)

            if isSelected {
                Circle()
                    .fill(Theme.accent)
                    .frame(width: 12, height: 12)
            }
        }
    }
    
    private func backendIcon(for id: String) -> String {
        BackendAgentService.icon(for: id)
    }

    private func backendColor(for id: String) -> Color {
        BackendAgentService.color(for: id)
    }
    
    private func filterProviders(for backend: String?) -> [Provider] {
        if backend == "claudecode" || backend == "amp" {
            // Only show Anthropic models for Claude Code and Amp
            return providers.filter { $0.id == "anthropic" }
        }
        if backend == "codex" {
            // Only show OpenAI models for Codex
            return providers.filter { $0.id == "openai" }
        }
        if backend == "gemini" {
            // Only show Google models for Gemini
            return providers.filter { $0.id == "google" }
        }
        if backend == "grok" {
            // Only show xAI models for Grok Build
            return providers.filter { $0.id == "xai" }
        }
        return providers
    }
    
    // MARK: - Data Loading
    
    private func loadData() async {
        isLoading = true
        defer { isLoading = false }

        // Fetch backends/agents and providers concurrently. Previously these
        // ran serially even though they share no state — the providers fetch
        // is what's gating the model picker, so users on a slow link saw the
        // form half-populated. (UX audit item #15.)
        async let backendDataTask = BackendAgentService.loadBackendsAndAgents()
        async let providersTask: ProvidersResponse? = try? api.listProviders()

        let data = await backendDataTask
        backends = data.backends
        enabledBackendIds = data.enabledBackendIds
        backendAgents = data.backendAgents

        // Set default agent (prefer saved default, then first available)
        if selectedAgentValue.isEmpty {
            // First check for saved default agent
            if let savedDefault = UserDefaults.standard.string(forKey: "default_agent"), !savedDefault.isEmpty {
                let parsed = CombinedAgent.parse(savedDefault)
                // Verify the saved agent still exists
                if let backendId = parsed?.backend,
                   let agentId = parsed?.agent,
                   backendAgents[backendId]?.contains(where: { $0.id == agentId }) == true {
                    selectedAgentValue = savedDefault
                }
            }

            // Fall back to the first available agent.
            if selectedAgentValue.isEmpty {
                if let firstBackend = backends.first(where: { enabledBackendIds.contains($0.id) }),
                          let firstAgent = backendAgents[firstBackend.id]?.first {
                    selectedAgentValue = "\(firstBackend.id):\(firstAgent.id)"
                }
            }
        }

        providers = (await providersTask)?.providers ?? []
    }
}

// MARK: - Mission Options

struct NewMissionOptions {
    let workspaceId: String?
    let agent: String?
    let modelOverride: String?
    let backend: String
}

#Preview {
    NewMissionSheet(
        workspaces: [
            Workspace(
                id: "00000000-0000-0000-0000-000000000000",
                name: "host",
                workspaceType: .host,
                path: "/root",
                status: .ready,
                errorMessage: nil,
                createdAt: "2025-01-05T12:00:00Z"
            ),
            Workspace(
                id: "1",
                name: "project-a",
                workspaceType: .container,
                path: "/var/lib/sandboxed-sh/containers/project-a",
                status: .ready,
                errorMessage: nil,
                createdAt: "2025-01-05T12:00:00Z"
            )
        ],
        selectedWorkspaceId: .constant("00000000-0000-0000-0000-000000000000"),
        onCreate: { (_: NewMissionOptions) in },
        onCancel: {}
    )
}
