//
//  SettingsView.swift
//  OpenAgentDashboard
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

    private let api = APIService.shared

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
    }

    init() {
        _serverURL = State(initialValue: APIService.shared.baseURL)
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
                                VStack(alignment: .leading, spacing: 16) {
                                    // Server URL field
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
                                            .onChange(of: serverURL) { _, _ in
                                                connectionStatus = .unknown
                                            }
                                    }

                                    // Connection status
                                    HStack(spacing: 8) {
                                        Image(systemName: connectionStatus.icon)
                                            .foregroundStyle(connectionStatus.color)
                                            .symbolEffect(.rotate, isActive: connectionStatus == .testing)

                                        Text(connectionStatus.message)
                                            .font(.caption)
                                            .foregroundStyle(connectionStatus.color)

                                        Spacer()

                                        // Test Connection button
                                        Button {
                                            Task { await testConnection() }
                                        } label: {
                                            Text("Test")
                                                .font(.caption.weight(.medium))
                                        }
                                        .buttonStyle(.glass)
                                        .disabled(serverURL.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || connectionStatus == .testing)
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
                                        Text("Open Agent Dashboard")
                                            .font(.subheadline.weight(.medium))
                                            .foregroundStyle(Theme.textPrimary)
                                        Spacer()
                                        Text("v1.0")
                                            .font(.caption)
                                            .foregroundStyle(Theme.textSecondary)
                                    }

                                    Divider()
                                        .background(Theme.border)

                                    Text("A native iOS dashboard for managing Open Agent workspaces and missions.")
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
        HapticService.success()
        dismiss()
    }
}

#Preview {
    SettingsView()
}
