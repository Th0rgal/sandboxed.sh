//
//  WorkspacesView.swift
//  SandboxedDashboard
//
//  Workspace management view
//

import SwiftUI

struct WorkspacesView: View {
    @State private var workspaces: [Workspace] = []
    @State private var isLoading = true
    @State private var errorMessage: String?
    @State private var selectedWorkspace: Workspace?
    @State private var showNewWorkspaceSheet = false

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
                        loadWorkspaces()
                    }
                    .foregroundColor(.blue)
                }
                .padding()
            } else {
                VStack(spacing: 0) {
                    // Header
                    HStack {
                        Text("Workspaces")
                            .font(.largeTitle.bold())
                            .foregroundColor(.white)
                        Spacer()
                        Button(action: { showNewWorkspaceSheet = true }) {
                            Image(systemName: "plus")
                                .font(.title3)
                                .foregroundColor(.white)
                                .frame(width: 40, height: 40)
                                .background(Color.indigo.opacity(0.2))
                                .cornerRadius(10)
                        }
                    }
                    .padding()

                    if workspaces.isEmpty {
                        Spacer()
                        VStack(spacing: 16) {
                            Image(systemName: "server.rack")
                                .font(.system(size: 60))
                                .foregroundColor(.white.opacity(0.2))
                            Text("No workspaces yet")
                                .foregroundColor(.white.opacity(0.4))
                            Text("Create a workspace to get started")
                                .font(.caption)
                                .foregroundColor(.white.opacity(0.3))
                        }
                        Spacer()
                    } else {
                        ScrollView {
                            LazyVStack(spacing: 12) {
                                ForEach(workspaces) { workspace in
                                    WorkspaceCard(workspace: workspace, onTap: {
                                        selectedWorkspace = workspace
                                    })
                                }
                            }
                            .padding()
                        }
                    }
                }
            }
        }
        .sheet(item: $selectedWorkspace) { workspace in
            WorkspaceDetailView(workspace: workspace, onDismiss: {
                selectedWorkspace = nil
                loadWorkspaces()
            })
        }
        .sheet(isPresented: $showNewWorkspaceSheet) {
            NewWorkspaceSheet(onDismiss: {
                showNewWorkspaceSheet = false
                loadWorkspaces()
            })
        }
        .onAppear {
            loadWorkspaces()
        }
    }

    private func loadWorkspaces() {
        isLoading = true
        errorMessage = nil

        APIService.shared.listWorkspaces { result in
            DispatchQueue.main.async {
                isLoading = false
                switch result {
                case .success(let workspaceList):
                    workspaces = workspaceList
                case .failure(let error):
                    errorMessage = error.localizedDescription
                }
            }
        }
    }
}

struct WorkspaceCard: View {
    let workspace: Workspace
    let onTap: () -> Void

    var body: some View {
        Button(action: onTap) {
            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    Image(systemName: "server.rack")
                        .foregroundColor(.indigo)
                    Text(workspace.name)
                        .font(.headline)
                        .foregroundColor(.white)
                    Spacer()
                    WorkspaceStatusBadge(status: workspace.status)
                }

                HStack {
                    Text(workspace.workspaceType.displayName)
                        .font(.caption)
                        .padding(.horizontal, 8)
                        .padding(.vertical, 4)
                        .background(Color.white.opacity(0.04))
                        .cornerRadius(4)
                        .overlay(
                            RoundedRectangle(cornerRadius: 4)
                                .stroke(Color.white.opacity(0.08), lineWidth: 1)
                        )
                        .foregroundColor(.white.opacity(0.6))

                    Spacer()

                    Image(systemName: "chevron.right")
                        .font(.caption)
                        .foregroundColor(.white.opacity(0.4))
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

struct WorkspaceDetailView: View {
    let workspace: Workspace
    let onDismiss: () -> Void

    var body: some View {
        NavigationView {
            ZStack {
                Color.black.ignoresSafeArea()

                ScrollView {
                    VStack(alignment: .leading, spacing: 20) {
                        VStack(alignment: .leading, spacing: 8) {
                            Text("Type")
                                .font(.caption)
                                .foregroundColor(.white.opacity(0.4))
                            Text(workspace.workspaceType.displayName)
                                .foregroundColor(.white)
                        }

                        VStack(alignment: .leading, spacing: 8) {
                            Text("Status")
                                .font(.caption)
                                .foregroundColor(.white.opacity(0.4))
                            HStack {
                                WorkspaceStatusBadge(status: workspace.status)
                                Spacer()
                            }
                        }

                        VStack(alignment: .leading, spacing: 8) {
                            Text("Path")
                                .font(.caption)
                                .foregroundColor(.white.opacity(0.4))
                            Text(workspace.path)
                                .font(.system(.caption, design: .monospaced))
                                .foregroundColor(.white.opacity(0.8))
                        }

                        VStack(alignment: .leading, spacing: 8) {
                            Text("ID")
                                .font(.caption)
                                .foregroundColor(.white.opacity(0.4))
                            Text(workspace.id)
                                .font(.system(.caption, design: .monospaced))
                                .foregroundColor(.white.opacity(0.8))
                        }

                        if let errorMessage = workspace.errorMessage {
                            VStack(alignment: .leading, spacing: 8) {
                                Text("Error")
                                    .font(.caption)
                                    .foregroundColor(.white.opacity(0.4))
                                Text(errorMessage)
                                    .foregroundColor(.red.opacity(0.8))
                                    .padding()
                                    .background(Color.red.opacity(0.1))
                                    .cornerRadius(8)
                            }
                        }
                    }
                    .padding()
                }
            }
            .navigationTitle(workspace.name)
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

struct NewWorkspaceSheet: View {
    let onDismiss: () -> Void
    @State private var name = ""
    @State private var workspaceType: WorkspaceType = .container
    @State private var isCreating = false

    var body: some View {
        NavigationView {
            ZStack {
                Color.black.ignoresSafeArea()

                VStack(spacing: 20) {
                    TextField("Workspace Name", text: $name)
                        .textFieldStyle(.roundedBorder)

                    Picker("Type", selection: $workspaceType) {
                        Text("Host").tag(WorkspaceType.host)
                        Text("Container").tag(WorkspaceType.container)
                    }
                    .pickerStyle(.segmented)

                    Text(workspaceType == .host
                        ? "Runs directly on the host machine filesystem"
                        : "Creates an isolated container environment")
                        .font(.caption)
                        .foregroundColor(.white.opacity(0.5))

                    Spacer()
                }
                .padding()
            }
            .navigationTitle("New Workspace")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .navigationBarLeading) {
                    Button("Cancel") {
                        onDismiss()
                    }
                }
                ToolbarItem(placement: .navigationBarTrailing) {
                    Button(action: createWorkspace) {
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

    private func createWorkspace() {
        isCreating = true
        APIService.shared.createWorkspace(name: name, type: workspaceType) { result in
            DispatchQueue.main.async {
                isCreating = false
                onDismiss()
            }
        }
    }
}
