//
//  AgentsView.swift
//  OpenAgentDashboard
//
//  Agent configuration management view
//

import SwiftUI

// AgentConfig model is now in Models/AgentConfig.swift

struct AgentsView: View {
    @State private var agents: [AgentConfig] = []
    @State private var isLoading = true
    @State private var errorMessage: String?
    @State private var selectedAgent: AgentConfig?
    @State private var showNewAgentSheet = false

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()

            if isLoading {
                ProgressView()
                    .progressViewStyle(CircularProgressViewStyle(tint: .white))
            } else if let error = errorMessage {
                VStack(spacing: 16) {
                    Image(systemName: "exclamationmark.triangle")
                        .font(.system(size: 48))
                        .foregroundColor(.red.opacity(0.6))
                    Text(error)
                        .foregroundColor(.white.opacity(0.6))
                        .multilineTextAlignment(.center)
                    Button("Retry") {
                        loadAgents()
                    }
                    .foregroundColor(.blue)
                }
                .padding()
            } else {
                VStack(spacing: 0) {
                    // Header
                    HStack {
                        Text("Agents")
                            .font(.largeTitle.bold())
                            .foregroundColor(.white)
                        Spacer()
                        Button(action: { showNewAgentSheet = true }) {
                            Image(systemName: "plus")
                                .font(.title3)
                                .foregroundColor(.white)
                                .frame(width: 40, height: 40)
                                .background(Color.indigo.opacity(0.2))
                                .cornerRadius(10)
                        }
                    }
                    .padding()

                    if agents.isEmpty {
                        Spacer()
                        VStack(spacing: 16) {
                            Image(systemName: "cpu")
                                .font(.system(size: 60))
                                .foregroundColor(.white.opacity(0.2))
                            Text("No agents yet")
                                .foregroundColor(.white.opacity(0.4))
                            Text("Create an agent to get started")
                                .font(.caption)
                                .foregroundColor(.white.opacity(0.3))
                        }
                        Spacer()
                    } else {
                        ScrollView {
                            LazyVStack(spacing: 12) {
                                ForEach(agents) { agent in
                                    AgentCard(agent: agent, onTap: {
                                        selectedAgent = agent
                                    })
                                }
                            }
                            .padding()
                        }
                    }
                }
            }
        }
        .sheet(item: $selectedAgent) { agent in
            AgentDetailView(agent: agent, onDismiss: {
                selectedAgent = nil
                loadAgents()
            })
        }
        .sheet(isPresented: $showNewAgentSheet) {
            NewAgentSheet(onDismiss: {
                showNewAgentSheet = false
                loadAgents()
            })
        }
        .onAppear {
            loadAgents()
        }
    }

    private func loadAgents() {
        isLoading = true
        errorMessage = nil

        APIService.shared.listAgents { result in
            DispatchQueue.main.async {
                isLoading = false
                switch result {
                case .success(let agentList):
                    agents = agentList
                case .failure(let error):
                    errorMessage = error.localizedDescription
                }
            }
        }
    }
}

struct AgentCard: View {
    let agent: AgentConfig
    let onTap: () -> Void

    var body: some View {
        Button(action: onTap) {
            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    Image(systemName: "cpu")
                        .foregroundColor(.indigo)
                    Text(agent.name)
                        .font(.headline)
                        .foregroundColor(.white)
                    Spacer()
                    Image(systemName: "chevron.right")
                        .font(.caption)
                        .foregroundColor(.white.opacity(0.4))
                }

                Text(agent.model_id)
                    .font(.caption)
                    .foregroundColor(.white.opacity(0.6))

                if !agent.mcp_servers.isEmpty || !agent.skills.isEmpty || !agent.commands.isEmpty {
                    HStack(spacing: 12) {
                        if !agent.mcp_servers.isEmpty {
                            Label("\(agent.mcp_servers.count)", systemImage: "server.rack")
                                .font(.caption2)
                                .foregroundColor(.white.opacity(0.5))
                        }
                        if !agent.skills.isEmpty {
                            Label("\(agent.skills.count)", systemImage: "doc.text")
                                .font(.caption2)
                                .foregroundColor(.white.opacity(0.5))
                        }
                        if !agent.commands.isEmpty {
                            Label("\(agent.commands.count)", systemImage: "terminal")
                                .font(.caption2)
                                .foregroundColor(.white.opacity(0.5))
                        }
                    }
                }
            }
            .padding()
            .background(Color.white.opacity(0.02))
            .cornerRadius(12)
            .overlay(
                RoundedRectangle(cornerRadius: 12)
                    .stroke(Color.white.opacity(0.06), lineWidth: 1)
            )
        }
    }
}

struct AgentDetailView: View {
    let agent: AgentConfig
    let onDismiss: () -> Void

    var body: some View {
        NavigationView {
            ZStack {
                Color.black.ignoresSafeArea()

                ScrollView {
                    VStack(alignment: .leading, spacing: 20) {
                        VStack(alignment: .leading, spacing: 8) {
                            Text("Model")
                                .font(.caption)
                                .foregroundColor(.white.opacity(0.4))
                            Text(agent.model_id)
                                .foregroundColor(.white)
                        }

                        if !agent.mcp_servers.isEmpty {
                            VStack(alignment: .leading, spacing: 8) {
                                Text("MCP Servers")
                                    .font(.caption)
                                    .foregroundColor(.white.opacity(0.4))
                                ForEach(agent.mcp_servers, id: \.self) { server in
                                    HStack {
                                        Image(systemName: "server.rack")
                                            .font(.caption)
                                            .foregroundColor(.indigo.opacity(0.6))
                                        Text(server)
                                            .foregroundColor(.white.opacity(0.8))
                                    }
                                }
                            }
                        }

                        if !agent.skills.isEmpty {
                            VStack(alignment: .leading, spacing: 8) {
                                Text("Skills")
                                    .font(.caption)
                                    .foregroundColor(.white.opacity(0.4))
                                ForEach(agent.skills, id: \.self) { skill in
                                    HStack {
                                        Image(systemName: "doc.text")
                                            .font(.caption)
                                            .foregroundColor(.indigo.opacity(0.6))
                                        Text(skill)
                                            .foregroundColor(.white.opacity(0.8))
                                    }
                                }
                            }
                        }

                        if !agent.commands.isEmpty {
                            VStack(alignment: .leading, spacing: 8) {
                                Text("Commands")
                                    .font(.caption)
                                    .foregroundColor(.white.opacity(0.4))
                                ForEach(agent.commands, id: \.self) { command in
                                    HStack {
                                        Image(systemName: "terminal")
                                            .font(.caption)
                                            .foregroundColor(.indigo.opacity(0.6))
                                        Text("/\(command)")
                                            .foregroundColor(.white.opacity(0.8))
                                    }
                                }
                            }
                        }
                    }
                    .padding()
                }
            }
            .navigationTitle(agent.name)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .navigationBarTrailing) {
                    Button("Done") {
                        onDismiss()
                    }
                    .foregroundColor(.indigo)
                }
            }
        }
    }
}

struct NewAgentSheet: View {
    let onDismiss: () -> Void
    @State private var name = ""
    @State private var isCreating = false

    var body: some View {
        NavigationView {
            ZStack {
                Color.black.ignoresSafeArea()

                VStack(spacing: 20) {
                    TextField("Agent Name", text: $name)
                        .textFieldStyle(.roundedBorder)

                    Text("Note: Create basic agent here, configure details in web dashboard")
                        .font(.caption)
                        .foregroundColor(.white.opacity(0.5))

                    Spacer()
                }
                .padding()
            }
            .navigationTitle("New Agent")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .navigationBarLeading) {
                    Button("Cancel") {
                        onDismiss()
                    }
                }
                ToolbarItem(placement: .navigationBarTrailing) {
                    Button(action: createAgent) {
                        if isCreating {
                            ProgressView()
                                .progressViewStyle(CircularProgressViewStyle())
                        } else {
                            Text("Create")
                        }
                    }
                    .disabled(name.isEmpty || isCreating)
                }
            }
        }
    }

    private func createAgent() {
        isCreating = true
        // Create with default model
        APIService.shared.createAgent(name: name, modelId: "claude-sonnet-4-20250514") { result in
            DispatchQueue.main.async {
                isCreating = false
                onDismiss()
            }
        }
    }
}
