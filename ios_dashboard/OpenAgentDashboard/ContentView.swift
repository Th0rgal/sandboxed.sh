//
//  ContentView.swift
//  OpenAgentDashboard
//
//  Main content view with authentication gate and tab navigation
//

import SwiftUI

struct ContentView: View {
    @State private var isAuthenticated = false
    @State private var isCheckingAuth = true
    @State private var authRequired = false
    
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
    }
    
    private func checkAuth() async {
        isCheckingAuth = true
        
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

// MARK: - Login View

struct LoginView: View {
    let onLogin: () -> Void
    
    @State private var password = ""
    @State private var isLoading = false
    @State private var errorMessage: String?
    @State private var serverURL: String
    
    @FocusState private var isPasswordFocused: Bool
    
    private let api = APIService.shared
    
    init(onLogin: @escaping () -> Void) {
        self.onLogin = onLogin
        _serverURL = State(initialValue: APIService.shared.baseURL)
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
                            Text("Open Agent")
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
                                isDisabled: password.isEmpty
                            ) {
                                login()
                            }
                        }
                    }
                    .padding(.horizontal, 24)
                    
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
                let _ = try await api.login(password: password)
                HapticService.success()
                onLogin()
            } catch {
                errorMessage = error.localizedDescription
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
