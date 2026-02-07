//
//  FilesView.swift
//  SandboxedDashboard
//
//  Remote file explorer with SFTP-like functionality
//

import SwiftUI
import UniformTypeIdentifiers

struct FilesView: View {
    private var workspaceState = WorkspaceState.shared
    @State private var currentPath = "/root/context"
    @State private var entries: [FileEntry] = []
    @State private var isLoading = false
    @State private var errorMessage: String?
    @State private var selectedEntry: FileEntry?
    @State private var showingDeleteAlert = false
    @State private var isEditingPath = false
    @State private var editedPath = ""
    @FocusState private var isPathFieldFocused: Bool
    @State private var showingNewFolderAlert = false
    @State private var newFolderName = ""
    @State private var isImporting = false

    // Track pending path fetch to prevent race conditions
    @State private var fetchingPath: String?

    // Track workspace changes
    @State private var lastWorkspaceId: String?

    private let api = APIService.shared
    
    private var sortedEntries: [FileEntry] {
        let dirs = entries.filter { $0.isDirectory }.sorted { $0.name < $1.name }
        let files = entries.filter { !$0.isDirectory }.sorted { $0.name < $1.name }
        return dirs + files
    }
    
    private var breadcrumbs: [(name: String, path: String)] {
        var crumbs: [(name: String, path: String)] = [("/", "/")]
        var accumulated = ""
        for part in currentPath.split(separator: "/") {
            accumulated += "/" + part
            crumbs.append((String(part), accumulated))
        }
        return crumbs
    }
    
    var body: some View {
        ZStack(alignment: .bottomTrailing) {
            Theme.backgroundPrimary.ignoresSafeArea()
            
            VStack(spacing: 0) {
                // Breadcrumb navigation (compact)
                breadcrumbView
                
                // File list
                if isLoading {
                    LoadingView(message: "Loading files...")
                } else if let error = errorMessage {
                    EmptyStateView(
                        icon: "exclamationmark.triangle",
                        title: "Failed to Load",
                        message: error,
                        action: { Task { await loadDirectory() } },
                        actionLabel: "Retry"
                    )
                } else if sortedEntries.isEmpty {
                    emptyFolderView
                } else {
                    fileListView
                }
            }
            
            // Floating Action Button for Import
            Button {
                isImporting = true
            } label: {
                Image(systemName: "plus")
                    .font(.title2.weight(.semibold))
                    .foregroundStyle(.white)
                    .frame(width: 56, height: 56)
                    .background(Theme.accent)
                    .clipShape(Circle())
                    .shadow(color: Theme.accent.opacity(0.4), radius: 8, x: 0, y: 4)
            }
            .padding(.trailing, 20)
            .padding(.bottom, 20)
        }
        .navigationTitle("Files")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                // Workspace selector
                Menu {
                    // Workspace selection section
                    Section("Workspace") {
                        ForEach(workspaceState.workspaces) { workspace in
                            Button {
                                workspaceState.selectWorkspace(id: workspace.id)
                                // Navigate to the workspace's base path
                                navigateTo(workspaceState.filesBasePath)
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
                    }

                    Divider()

                    // Quick nav section
                    Section("Quick Nav") {
                        Button {
                            navigateTo("/root/context")
                        } label: {
                            Label("Context", systemImage: "tray.and.arrow.down")
                        }

                        Button {
                            navigateTo("/root/work")
                        } label: {
                            Label("Work", systemImage: "hammer")
                        }

                        Button {
                            navigateTo("/root/tools")
                        } label: {
                            Label("Tools", systemImage: "wrench.and.screwdriver")
                        }

                        Divider()

                        Button {
                            navigateTo("/root")
                        } label: {
                            Label("Home", systemImage: "house")
                        }

                        Button {
                            navigateTo("/")
                        } label: {
                            Label("Root", systemImage: "externaldrive")
                        }
                    }
                } label: {
                    Image(systemName: "square.stack.3d.up")
                        .font(.system(size: 16))
                        .foregroundStyle(Theme.textSecondary)
                }
            }

            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    Button {
                        showingNewFolderAlert = true
                    } label: {
                        Label("New Folder", systemImage: "folder.badge.plus")
                    }
                    
                    Button {
                        isImporting = true
                    } label: {
                        Label("Import Files", systemImage: "square.and.arrow.down")
                    }
                    
                    Divider()
                    
                    Button {
                        Task { await loadDirectory() }
                    } label: {
                        Label("Refresh", systemImage: "arrow.clockwise")
                    }
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
            }
        }
        .alert("New Folder", isPresented: $showingNewFolderAlert) {
            TextField("Folder name", text: $newFolderName)
            Button("Cancel", role: .cancel) {
                newFolderName = ""
            }
            Button("Create") {
                Task { await createFolder() }
            }
        }
        .alert("Delete \(selectedEntry?.name ?? "")?", isPresented: $showingDeleteAlert) {
            Button("Cancel", role: .cancel) {}
            Button("Delete", role: .destructive) {
                Task { await deleteSelected() }
            }
        }
        .fileImporter(
            isPresented: $isImporting,
            allowedContentTypes: [.item],
            allowsMultipleSelection: true
        ) { result in
            Task { await handleFileImport(result) }
        }
        .task {
            // Load workspaces if not already loaded
            if workspaceState.workspaces.isEmpty {
                await workspaceState.loadWorkspaces()
            }

            // Set initial path based on workspace
            currentPath = workspaceState.filesBasePath
            lastWorkspaceId = workspaceState.selectedWorkspace?.id

            await loadDirectory()
        }
        .onChange(of: workspaceState.selectedWorkspace?.id) { _, newId in
            // Handle workspace change from other tabs
            if newId != lastWorkspaceId {
                lastWorkspaceId = newId
                navigateTo(workspaceState.filesBasePath)
            }
        }
    }
    
    // MARK: - Subviews
    
    private var breadcrumbView: some View {
        HStack(spacing: 0) {
            // Up button
            if currentPath != "/" && !isEditingPath {
                Button {
                    goUp()
                } label: {
                    Image(systemName: "chevron.left")
                        .font(.body.weight(.medium))
                        .foregroundStyle(Theme.accent)
                        .frame(width: 44, height: 44)
                }
            }
            
            if isEditingPath {
                // Editable path text field
                HStack(spacing: 8) {
                    Image(systemName: "folder")
                        .foregroundStyle(Theme.accent)
                    
                    TextField("Path", text: $editedPath)
                        .font(.subheadline.monospaced())
                        .textFieldStyle(.plain)
                        .autocorrectionDisabled()
                        .textInputAutocapitalization(.never)
                        .focused($isPathFieldFocused)
                        .onSubmit {
                            navigateTo(editedPath)
                            isEditingPath = false
                        }
                    
                    Button {
                        isEditingPath = false
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                            .font(.title3)
                            .foregroundStyle(Theme.textMuted)
                    }
                    
                    Button {
                        navigateTo(editedPath)
                        isEditingPath = false
                    } label: {
                        Image(systemName: "arrow.right.circle.fill")
                            .font(.title3)
                            .foregroundStyle(Theme.accent)
                    }
                }
                .padding(.horizontal, 16)
            } else {
                // Breadcrumb path using / separators
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 0) {
                        ForEach(Array(breadcrumbs.enumerated()), id: \.offset) { index, crumb in
                            // Add / separator after first element (which is "/"), not before
                            if index > 1 {
                                Text("/")
                                    .font(.subheadline.weight(.medium))
                                    .foregroundStyle(Theme.textMuted)
                            }
                            
                            Button {
                                navigateTo(crumb.path)
                            } label: {
                                Text(crumb.name)
                                    .font(.subheadline.weight(index == breadcrumbs.count - 1 ? .semibold : .medium))
                                    .foregroundStyle(index == breadcrumbs.count - 1 ? Theme.textPrimary : Theme.textTertiary)
                                    .padding(.horizontal, 4)
                                    .padding(.vertical, 6)
                                    .background(index == breadcrumbs.count - 1 ? Theme.backgroundSecondary : .clear)
                                    .clipShape(RoundedRectangle(cornerRadius: 6, style: .continuous))
                            }
                        }
                    }
                    .padding(.trailing, 8)
                }
                
                // Edit button - larger tap target
                Button {
                    editedPath = currentPath
                    isEditingPath = true
                    isPathFieldFocused = true
                    HapticService.selectionChanged()
                } label: {
                    Image(systemName: "square.and.pencil")
                        .font(.body)
                        .foregroundStyle(Theme.accent)
                        .frame(width: 44, height: 44)
                }
            }
        }
        .padding(.leading, currentPath == "/" && !isEditingPath ? 12 : 0)
        .frame(height: 44)
        .background(.thinMaterial)
    }
    
    private var emptyFolderView: some View {
        VStack(spacing: 24) {
            Spacer()
            
            Image(systemName: "folder")
                .font(.system(size: 64, weight: .light))
                .foregroundStyle(Theme.textMuted)
            
            VStack(spacing: 8) {
                Text("Empty Folder")
                    .font(.title3.bold())
                    .foregroundStyle(Theme.textPrimary)
                
                Text("Tap + to import files")
                    .font(.subheadline)
                    .foregroundStyle(Theme.textSecondary)
            }
            
            // Quick actions
            HStack(spacing: 12) {
                Button {
                    showingNewFolderAlert = true
                } label: {
                    Label("New Folder", systemImage: "folder.badge.plus")
                        .font(.subheadline.weight(.medium))
                        .foregroundStyle(Theme.textPrimary)
                        .padding(.horizontal, 16)
                        .padding(.vertical, 10)
                        .background(.ultraThinMaterial)
                        .clipShape(Capsule())
                }
            }
            
            Spacer()
            Spacer()
        }
        .frame(maxWidth: .infinity)
    }
    
    private var fileListView: some View {
        ScrollView {
            LazyVStack(spacing: 8) {
                ForEach(sortedEntries) { entry in
                    FileRow(entry: entry)
                        .contentShape(Rectangle())
                        .onTapGesture {
                            HapticService.selectionChanged()
                            if entry.isDirectory {
                                navigateTo(entry.path)
                            } else {
                                selectedEntry = entry
                            }
                        }
                        .contextMenu {
                            if entry.isFile {
                                Button {
                                    downloadFile(entry)
                                } label: {
                                    Label("Download", systemImage: "arrow.down.circle")
                                }
                            }
                            
                            Button(role: .destructive) {
                                selectedEntry = entry
                                showingDeleteAlert = true
                            } label: {
                                Label("Delete", systemImage: "trash")
                            }
                        }
                }
                
                // Bottom padding for FAB
                Spacer()
                    .frame(height: 80)
            }
            .padding(.horizontal, 16)
            .padding(.top, 8)
        }
        .refreshable {
            await loadDirectory()
        }
    }
    
    // MARK: - Actions
    
    private func loadDirectory() async {
        let pathToLoad = currentPath
        fetchingPath = pathToLoad
        
        isLoading = true
        errorMessage = nil
        
        do {
            let result = try await api.listDirectory(path: pathToLoad)
            
            // Race condition guard: only update if this is still the path we want
            guard fetchingPath == pathToLoad else {
                return // Navigation changed, discard this response
            }
            
            entries = result
        } catch {
            // Race condition guard
            guard fetchingPath == pathToLoad else { return }
            
            errorMessage = error.localizedDescription
        }
        
        // Only clear loading if this is still the current fetch
        if fetchingPath == pathToLoad {
            isLoading = false
        }
    }
    
    private func navigateTo(_ path: String) {
        currentPath = path
        Task { await loadDirectory() }
        HapticService.selectionChanged()
    }
    
    private func goUp() {
        guard currentPath != "/" else { return }
        var parts = currentPath.split(separator: "/")
        parts.removeLast()
        currentPath = parts.isEmpty ? "/" : "/" + parts.joined(separator: "/")
        Task { await loadDirectory() }
        HapticService.selectionChanged()
    }
    
    private func createFolder() async {
        guard !newFolderName.isEmpty else { return }
        
        let folderPath = currentPath.hasSuffix("/") 
            ? currentPath + newFolderName 
            : currentPath + "/" + newFolderName
        
        do {
            try await api.createDirectory(path: folderPath)
            newFolderName = ""
            await loadDirectory()
            HapticService.success()
        } catch {
            errorMessage = error.localizedDescription
            HapticService.error()
        }
    }
    
    private func deleteSelected() async {
        guard let entry = selectedEntry else { return }
        
        do {
            try await api.deleteFile(path: entry.path, recursive: entry.isDirectory)
            selectedEntry = nil
            await loadDirectory()
            HapticService.success()
        } catch {
            errorMessage = error.localizedDescription
            HapticService.error()
        }
    }
    
    private func downloadFile(_ entry: FileEntry) {
        guard let url = api.downloadURL(path: entry.path) else { return }
        UIApplication.shared.open(url)
    }
    
    private func handleFileImport(_ result: Result<[URL], Error>) async {
        switch result {
        case .success(let urls):
            for url in urls {
                guard url.startAccessingSecurityScopedResource() else { continue }
                defer { url.stopAccessingSecurityScopedResource() }
                
                do {
                    let data = try Data(contentsOf: url)
                    let _ = try await api.uploadFile(
                        data: data,
                        fileName: url.lastPathComponent,
                        directory: currentPath
                    )
                } catch {
                    errorMessage = "Upload failed: \(error.localizedDescription)"
                    HapticService.error()
                    return
                }
            }
            await loadDirectory()
            HapticService.success()
            
        case .failure(let error):
            errorMessage = error.localizedDescription
            HapticService.error()
        }
    }
}

// MARK: - File Row

private struct FileRow: View {
    let entry: FileEntry
    
    private var iconColor: Color {
        if entry.isDirectory {
            return Theme.accent
        }
        // Color by file type
        let ext = entry.name.components(separatedBy: ".").last?.lowercased() ?? ""
        switch ext {
        case "json", "yaml", "yml", "toml": return .orange
        case "swift", "rs", "py", "js", "ts": return .cyan
        case "md", "txt", "log": return Theme.textSecondary
        case "jpg", "jpeg", "png", "gif", "svg": return .pink
        case "zip", "tar", "gz", "jar": return .purple
        default: return Theme.textSecondary
        }
    }
    
    var body: some View {
        HStack(spacing: 16) {
            // Icon with color accent
            ZStack {
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .fill(iconColor.opacity(0.15))
                    .frame(width: 48, height: 48)
                
                Image(systemName: entry.icon)
                    .font(.title3)
                    .foregroundStyle(iconColor)
            }
            
            // Name and details
            VStack(alignment: .leading, spacing: 4) {
                Text(entry.name)
                    .font(.body.weight(.medium))
                    .foregroundStyle(Theme.textPrimary)
                    .lineLimit(1)
                
                HStack(spacing: 6) {
                    if entry.isFile {
                        Text(entry.formattedSize)
                            .font(.caption)
                            .foregroundStyle(Theme.textTertiary)
                        
                        Text("•")
                            .font(.caption)
                            .foregroundStyle(Theme.textMuted)
                    }
                    
                    Text(entry.kind)
                        .font(.caption)
                        .foregroundStyle(Theme.textMuted)
                    
                    if let date = entry.modifiedDate {
                        Text("•")
                            .font(.caption)
                            .foregroundStyle(Theme.textMuted)
                        
                        Text(date.relativeFormatted)
                            .font(.caption)
                            .foregroundStyle(Theme.textMuted)
                    }
                }
            }
            
            Spacer()
            
            // Chevron for directories
            if entry.isDirectory {
                Image(systemName: "chevron.right")
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(Theme.textMuted)
            }
        }
        .padding(.vertical, 12)
        .padding(.horizontal, 16)
        .background(Theme.backgroundSecondary)
        .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
    }
}

#Preview {
    NavigationStack {
        FilesView()
    }
}
