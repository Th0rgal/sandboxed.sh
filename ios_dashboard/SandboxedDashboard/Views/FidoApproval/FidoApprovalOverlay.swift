//
//  FidoApprovalOverlay.swift
//  SandboxedDashboard
//
//  Full-screen overlay for approving/denying FIDO signing requests
//

import SwiftUI

struct FidoApprovalOverlay: View {
    private var fidoState = FidoApprovalState.shared

    private var request: FidoSignRequest? {
        fidoState.pendingRequests.first
    }

    var body: some View {
        if let request {
            ZStack {
                // Dimmed background
                Color.black.opacity(0.4)
                    .ignoresSafeArea()
                    .onTapGesture { } // prevent passthrough

                VStack {
                    Spacer()

                    // Card slides from bottom
                    GlassCard(padding: 24, cornerRadius: 28) {
                        VStack(spacing: 20) {
                            // Header with lock icon
                            HStack {
                                Image(systemName: "key.radiowaves.forward")
                                    .font(.title2)
                                    .foregroundStyle(Theme.warning)
                                Text("Signing Request")
                                    .font(.headline)
                                    .foregroundStyle(Theme.textPrimary)
                                Spacer()
                                CountdownView(expiresAt: request.expiresAt)
                            }

                            // Request details
                            VStack(alignment: .leading, spacing: 12) {
                                DetailRow(label: "Key", value: request.keyFingerprint, icon: "key")
                                if let hostname = request.hostname {
                                    DetailRow(label: "Host", value: hostname, icon: "network")
                                }
                                DetailRow(label: "Origin", value: request.origin, icon: "terminal")
                                if let workspace = request.workspace {
                                    DetailRow(label: "Workspace", value: workspace, icon: "cube")
                                }
                            }
                            .padding(.vertical, 8)

                            // Quick auto-approve chip
                            Button {
                                fidoState.addAutoApprovalRule(
                                    type: .allSSH,
                                    value: nil,
                                    duration: 5,
                                    requireBiometric: false
                                )
                                Task { await fidoState.approve(request.id) }
                            } label: {
                                HStack(spacing: 6) {
                                    Image(systemName: "clock")
                                    Text("Auto-approve SSH for 5 min")
                                }
                                .font(.caption)
                                .foregroundStyle(Theme.textSecondary)
                                .padding(.horizontal, 12)
                                .padding(.vertical, 6)
                                .background(.ultraThinMaterial)
                                .clipShape(Capsule())
                            }

                            // Action buttons
                            HStack(spacing: 16) {
                                Button {
                                    Task { await fidoState.deny(request.id) }
                                } label: {
                                    Label("Deny", systemImage: "xmark")
                                        .frame(maxWidth: .infinity)
                                }
                                .buttonStyle(GlassDenyButtonStyle())

                                Button {
                                    Task { await fidoState.approve(request.id) }
                                } label: {
                                    Label("Approve", systemImage: "checkmark")
                                        .frame(maxWidth: .infinity)
                                }
                                .buttonStyle(GlassApproveButtonStyle())
                            }
                        }
                    }
                    .padding(.horizontal, 16)
                    .padding(.bottom, 16)
                }
            }
            .transition(.move(edge: .bottom).combined(with: .opacity))
            .animation(
                .spring(response: 0.4, dampingFraction: 0.85),
                value: fidoState.pendingRequests.count
            )
        }
    }
}

// MARK: - Countdown View

private struct CountdownView: View {
    let expiresAt: Date

    @State private var remaining: TimeInterval = 0
    @State private var totalDuration: TimeInterval = 30

    private let timer = Timer.publish(every: 1, on: .main, in: .common).autoconnect()

    var body: some View {
        HStack(spacing: 4) {
            ZStack {
                Circle()
                    .stroke(Theme.border, lineWidth: 2)
                    .frame(width: 20, height: 20)

                Circle()
                    .trim(from: 0, to: max(0, remaining / totalDuration))
                    .stroke(
                        remaining > 10 ? Theme.warning : Theme.error,
                        style: StrokeStyle(lineWidth: 2, lineCap: .round)
                    )
                    .frame(width: 20, height: 20)
                    .rotationEffect(.degrees(-90))
            }

            Text("\(Int(max(0, remaining)))s")
                .font(.caption.monospacedDigit())
                .foregroundStyle(remaining > 10 ? Theme.textSecondary : Theme.error)
        }
        .onAppear {
            remaining = expiresAt.timeIntervalSinceNow
            totalDuration = max(remaining, 1)
        }
        .onReceive(timer) { _ in
            remaining = expiresAt.timeIntervalSinceNow
        }
    }
}

// MARK: - Detail Row

private struct DetailRow: View {
    let label: String
    let value: String
    let icon: String

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: icon)
                .font(.system(size: 12, weight: .medium))
                .foregroundStyle(Theme.textMuted)
                .frame(width: 20, alignment: .center)

            Text(label)
                .font(.caption.weight(.medium))
                .foregroundStyle(Theme.textSecondary)
                .frame(width: 70, alignment: .leading)

            Text(value)
                .font(.caption)
                .foregroundStyle(Theme.textPrimary)
                .lineLimit(1)
                .truncationMode(.middle)

            Spacer()
        }
    }
}

// MARK: - Button Styles

struct GlassApproveButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 16, weight: .semibold))
            .foregroundStyle(.white)
            .padding(.vertical, 14)
            .background(
                LinearGradient(
                    colors: [
                        Theme.success,
                        Theme.success.opacity(0.8),
                    ],
                    startPoint: .top,
                    endPoint: .bottom
                )
            )
            .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
            .shadow(color: Theme.success.opacity(0.3), radius: 8, y: 4)
            .scaleEffect(configuration.isPressed ? 0.97 : 1)
            .animation(.easeInOut(duration: 0.15), value: configuration.isPressed)
    }
}

struct GlassDenyButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 16, weight: .semibold))
            .foregroundStyle(Theme.error)
            .padding(.vertical, 14)
            .background(Theme.error.opacity(0.15))
            .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .stroke(Theme.error.opacity(0.3), lineWidth: 0.5)
            )
            .scaleEffect(configuration.isPressed ? 0.97 : 1)
            .animation(.easeInOut(duration: 0.15), value: configuration.isPressed)
    }
}

#Preview {
    ZStack {
        Theme.backgroundPrimary.ignoresSafeArea()
        FidoApprovalOverlay()
    }
}
