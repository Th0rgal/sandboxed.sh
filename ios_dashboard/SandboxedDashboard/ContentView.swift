//
//  ContentView.swift
//  SandboxedDashboard
//
//  Main content view with authentication gate and tab navigation
//

import SwiftUI

struct ContentView: View {
    @State private var isAuthenticated = false
    @State private var isCheckingAuth = true
    @State private var authRequired = false
    @State private var showSetupSheet = false

    private let api = APIService.shared

    var body: some View {
        Group {
            if isCheckingAuth {
                LoadingView(message: "Connecting...")
                    .background(Theme.backgroundPrimary.ignoresSafeArea())
            } else if authRequired && !isAuthenticated {
                LoginView(onLogin: { isAuthenticated = true })
            } else {
                MainTabView()
            }
        }
        .task {
            await checkAuth()
        }
        .sheet(isPresented: $showSetupSheet) {
            SetupSheet(onComplete: {
                showSetupSheet = false
                Task { await checkAuth() }
            })
        }
        .onChange(of: api.isConfigured) { _, isConfigured in
            // Re-check auth when server URL is configured
            if isConfigured {
                Task { await checkAuth() }
            }
        }
    }

    private func checkAuth() async {
        isCheckingAuth = true

        // If not configured, show setup sheet
        guard api.isConfigured else {
            isCheckingAuth = false
            showSetupSheet = true
            return
        }

        do {
            let _ = try await api.checkHealth()
            authRequired = api.authRequired
            isAuthenticated = api.isAuthenticated || !authRequired
        } catch {
            // If health check fails, assume we need auth
            authRequired = true
            isAuthenticated = api.isAuthenticated
        }

        isCheckingAuth = false
    }
}

// MARK: - Setup Sheet (First Launch)

struct SetupSheet: View {
    let onComplete: () -> Void

    @State private var serverURL = ""
    @State private var isTestingConnection = false
    @State private var connectionSuccess = false
    @State private var errorMessage: String?

    private let api = APIService.shared

    var body: some View {
        NavigationStack {
            ZStack {
                Theme.backgroundPrimary.ignoresSafeArea()

                VStack(spacing: 32) {
                    Spacer()

                    // Welcome icon
                    VStack(spacing: 16) {
                        Image(systemName: "server.rack")
                            .font(.system(size: 64, weight: .light))
                            .foregroundStyle(Theme.accent)

                        VStack(spacing: 8) {
                            Text("Welcome to sandboxed.sh")
                                .font(.title2.bold())
                                .foregroundStyle(Theme.textPrimary)

                            Text("Enter your server URL to get started")
                                .font(.subheadline)
                                .foregroundStyle(Theme.textSecondary)
                        }
                    }

                    // Server URL input
                    GlassCard(padding: 24, cornerRadius: 24) {
                        VStack(spacing: 20) {
                            VStack(alignment: .leading, spacing: 8) {
                                Text("Server URL")
                                    .font(.caption.weight(.medium))
                                    .foregroundStyle(Theme.textSecondary)

                                TextField("https://your-server.com", text: $serverURL)
                                    .textFieldStyle(.plain)
                                    .textInputAutocapitalization(.never)
                                    .autocorrectionDisabled()
                                    .keyboardType(.URL)
                                    .padding(.horizontal, 16)
                                    .padding(.vertical, 14)
                                    .background(Color.white.opacity(0.05))
                                    .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                                    .overlay(
                                        RoundedRectangle(cornerRadius: 12, style: .continuous)
                                            .stroke(Theme.border, lineWidth: 1)
                                    )
                            }

                            if let error = errorMessage {
                                HStack {
                                    Image(systemName: "exclamationmark.triangle.fill")
                                        .foregroundStyle(Theme.error)
                                    Text(error)
                                        .font(.caption)
                                        .foregroundStyle(Theme.error)
                                    Spacer()
                                }
                            }

                            if connectionSuccess {
                                HStack {
                                    Image(systemName: "checkmark.circle.fill")
                                        .foregroundStyle(Theme.success)
                                    Text("Connection successful!")
                                        .font(.caption)
                                        .foregroundStyle(Theme.success)
                                    Spacer()
                                }
                            }

                            Button {
                                Task { await connectToServer() }
                            } label: {
                                HStack {
                                    if isTestingConnection {
                                        ProgressView()
                                            .progressViewStyle(.circular)
                                            .tint(.white)
                                            .scaleEffect(0.8)
                                    }
                                    Text(isTestingConnection ? "Connecting..." : "Connect")
                                        .fontWeight(.semibold)
                                }
                                .frame(maxWidth: .infinity)
                            }
                            .buttonStyle(GlassProminentButtonStyle())
                            .disabled(serverURL.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || isTestingConnection)
                        }
                    }
                    .padding(.horizontal, 20)

                    Spacer()
                }
            }
            .navigationBarTitleDisplayMode(.inline)
            .interactiveDismissDisabled()
        }
        .presentationDetents([.large])
        .presentationDragIndicator(.hidden)
    }

    private func connectToServer() async {
        let trimmedURL = serverURL.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedURL.isEmpty else { return }

        isTestingConnection = true
        errorMessage = nil
        connectionSuccess = false

        // Save original URL to restore on failure
        let originalURL = api.baseURL
        api.baseURL = trimmedURL

        do {
            _ = try await api.checkHealth()
            connectionSuccess = true
            HapticService.success()

            // Brief delay to show success state
            try? await Task.sleep(for: .milliseconds(500))
            onComplete()
        } catch {
            // Restore original URL on failure
            api.baseURL = originalURL
            errorMessage = "Could not connect. Please check the URL."
            HapticService.error()
        }

        isTestingConnection = false
    }
}

// MARK: - Login View

struct LoginView: View {
    let onLogin: () -> Void
    
    @State private var username = ""
    @State private var password = ""
    @State private var isLoading = false
    @State private var errorMessage: String?
    @State private var serverURL: String
    
    @FocusState private var isUsernameFocused: Bool
    @FocusState private var isPasswordFocused: Bool
    
    private let api = APIService.shared
    
    init(onLogin: @escaping () -> Void) {
        self.onLogin = onLogin
        _serverURL = State(initialValue: APIService.shared.baseURL)
        _username = State(initialValue: UserDefaults.standard.string(forKey: "last_username") ?? "")
    }
    
    var body: some View {
        ZStack {
            // Background
            Theme.backgroundPrimary.ignoresSafeArea()
            
            // Gradient accents
            RadialGradient(
                colors: [Theme.accent.opacity(0.15), .clear],
                center: .topTrailing,
                startRadius: 50,
                endRadius: 400
            )
            .ignoresSafeArea()
            
            RadialGradient(
                colors: [Color.purple.opacity(0.1), .clear],
                center: .bottomLeading,
                startRadius: 50,
                endRadius: 400
            )
            .ignoresSafeArea()
            
            ScrollView {
                VStack(spacing: 32) {
                    Spacer()
                        .frame(height: 60)
                    
                    // Logo
                    VStack(spacing: 16) {
                        Image(systemName: "brain")
                            .font(.system(size: 72, weight: .light))
                            .foregroundStyle(Theme.accent)
                            .symbolEffect(.pulse, options: .repeating)
                        
                        VStack(spacing: 4) {
                            Text("sandboxed.sh")
                                .font(.largeTitle.bold())
                                .foregroundStyle(Theme.textPrimary)
                            
                            Text("Dashboard")
                                .font(.title3)
                                .foregroundStyle(Theme.textSecondary)
                        }
                    }
                    
                    // Login form
                    GlassCard(padding: 24, cornerRadius: 28) {
                        VStack(spacing: 20) {
                            // Server URL field
                            VStack(alignment: .leading, spacing: 8) {
                                Text("Server URL")
                                    .font(.caption.weight(.medium))
                                    .foregroundStyle(Theme.textSecondary)
                                
                                TextField("https://agent-backend.example.com", text: $serverURL)
                                    .textFieldStyle(.plain)
                                    .textInputAutocapitalization(.never)
                                    .autocorrectionDisabled()
                                    .keyboardType(.URL)
                                    .padding(.horizontal, 16)
                                    .padding(.vertical, 14)
                                    .background(Color.white.opacity(0.05))
                                    .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                                    .overlay(
                                        RoundedRectangle(cornerRadius: 12, style: .continuous)
                                            .stroke(Theme.border, lineWidth: 1)
                                    )
                            }

                            if api.authMode == .multiUser {
                                VStack(alignment: .leading, spacing: 8) {
                                    Text("Username")
                                        .font(.caption.weight(.medium))
                                        .foregroundStyle(Theme.textSecondary)

                                    TextField("Enter username", text: $username)
                                        .textFieldStyle(.plain)
                                        .textInputAutocapitalization(.never)
                                        .autocorrectionDisabled()
                                        .focused($isUsernameFocused)
                                        .padding(.horizontal, 16)
                                        .padding(.vertical, 14)
                                        .background(Color.white.opacity(0.05))
                                        .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                                        .overlay(
                                            RoundedRectangle(cornerRadius: 12, style: .continuous)
                                                .stroke(isUsernameFocused ? Theme.accent.opacity(0.5) : Theme.border, lineWidth: 1)
                                        )
                                }
                            }
                            
                            // Password field
                            VStack(alignment: .leading, spacing: 8) {
                                Text("Password")
                                    .font(.caption.weight(.medium))
                                    .foregroundStyle(Theme.textSecondary)
                                
                                SecureField("Enter password", text: $password)
                                    .textFieldStyle(.plain)
                                    .focused($isPasswordFocused)
                                    .padding(.horizontal, 16)
                                    .padding(.vertical, 14)
                                    .background(Color.white.opacity(0.05))
                                    .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                                    .overlay(
                                        RoundedRectangle(cornerRadius: 12, style: .continuous)
                                            .stroke(isPasswordFocused ? Theme.accent.opacity(0.5) : Theme.border, lineWidth: 1)
                                    )
                                    .onSubmit {
                                        login()
                                    }
                            }
                            
                            // Error message
                            if let error = errorMessage {
                                HStack(spacing: 8) {
                                    Image(systemName: "exclamationmark.circle.fill")
                                        .foregroundStyle(Theme.error)
                                    Text(error)
                                        .font(.caption)
                                        .foregroundStyle(Theme.error)
                                }
                                .frame(maxWidth: .infinity, alignment: .leading)
                            }
                            
                            // Login button
                            GlassPrimaryButton(
                                "Sign In",
                                icon: "arrow.right",
                                isLoading: isLoading,
                                isDisabled: password.isEmpty || (api.authMode == .multiUser && username.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                            ) {
                                login()
                            }
                        }
                    }
                    .padding(.horizontal, 24)
                    .onAppear {
                        if api.authMode == .multiUser {
                            isUsernameFocused = true
                        } else {
                            isPasswordFocused = true
                        }
                    }
                    .onChange(of: api.authMode) { _, newMode in
                        if newMode == .multiUser {
                            isUsernameFocused = true
                        } else {
                            isPasswordFocused = true
                        }
                    }
                    
                    Spacer()
                }
            }
        }
    }
    
    private func login() {
        guard !password.isEmpty else { return }
        
        // Update server URL
        api.baseURL = serverURL.trimmingCharacters(in: .whitespacesAndNewlines)
        
        isLoading = true
        errorMessage = nil
        
        Task {
            do {
                let usernameValue = api.authMode == .multiUser ? username : nil
                let _ = try await api.login(password: password, username: usernameValue)
                if api.authMode == .multiUser {
                    let trimmed = username.trimmingCharacters(in: .whitespacesAndNewlines)
                    if !trimmed.isEmpty {
                        UserDefaults.standard.set(trimmed, forKey: "last_username")
                    }
                }
                HapticService.success()
                onLogin()
            } catch {
                if let apiError = error as? APIError {
                    switch apiError {
                    case .httpError(let code, _):
                        if code == 401 {
                            errorMessage = api.authMode == .multiUser ? "Invalid username or password" : "Invalid password"
                        } else {
                            errorMessage = apiError.errorDescription
                        }
                    case .unauthorized:
                        errorMessage = api.authMode == .multiUser ? "Invalid username or password" : "Invalid password"
                    default:
                        errorMessage = apiError.errorDescription
                    }
                } else {
                    errorMessage = error.localizedDescription
                }
                HapticService.error()
            }
            isLoading = false
        }
    }
}

// MARK: - Main Tab View

struct MainTabView: View {
    private var nav = NavigationState.shared
    
    enum TabItem: String, CaseIterable {
        case control = "Control"
        case history = "History"
        case terminal = "Terminal"
        case files = "Files"
        
        var icon: String {
            switch self {
            case .control: return "message.fill"
            case .history: return "clock.fill"
            case .terminal: return "terminal.fill"
            case .files: return "folder.fill"
            }
        }
    }
    
    var body: some View {
        TabView(selection: Binding(
            get: { nav.selectedTab },
            set: { nav.selectedTab = $0 }
        )) {
            ForEach(TabItem.allCases, id: \.rawValue) { tab in
                NavigationStack {
                    tabContent(for: tab)
                }
                .tabItem {
                    Label(tab.rawValue, systemImage: tab.icon)
                }
                .tag(tab)
            }
        }
        .tint(Theme.accent)
    }
    
    @ViewBuilder
    private func tabContent(for tab: TabItem) -> some View {
        switch tab {
        case .control:
            ControlView()
        case .history:
            HistoryView()
        case .terminal:
            TerminalView()
        case .files:
            FilesView()
        }
    }
}

#Preview("Login") {
    LoginView(onLogin: {})
}

#Preview("Main") {
    MainTabView()
}
