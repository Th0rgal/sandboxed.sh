//
//  DesktopStreamView.swift
//  SandboxedDashboard
//
//  Real-time desktop stream viewer with controls
//  Designed to be shown in a bottom sheet, with Picture-in-Picture support
//

import SwiftUI
import AVKit

struct DesktopStreamView: View {
    @State private var streamService = DesktopStreamService.shared
    @State private var showControls = true
    @State private var displayId: String

    @Environment(\.dismiss) private var dismiss

    init(displayId: String = ":99") {
        _displayId = State(initialValue: displayId)
    }

    var body: some View {
        ZStack {
            // Background
            Theme.backgroundPrimary.ignoresSafeArea()

            VStack(spacing: 0) {
                // Header bar
                headerView

                // Stream content with PiP layer
                ZStack {
                    // PiP-enabled layer (hidden, used for feeding frames to PiP)
                    PipLayerView(streamService: streamService)
                        .frame(width: 1, height: 1)
                        .opacity(0)

                    // Visible stream content
                    streamContent
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)

                // Controls (when visible)
                if showControls {
                    controlsView
                        .transition(.move(edge: .bottom).combined(with: .opacity))
                }
            }
        }
        .onAppear {
            streamService.connect(displayId: displayId)
        }
        .onDisappear {
            // Only disconnect if PiP is not active
            // This allows the stream to continue in PiP mode after the sheet is dismissed
            if streamService.isPipActive {
                // Mark for cleanup when PiP stops
                streamService.shouldDisconnectAfterPip = true
            } else {
                streamService.cleanupPip()
                streamService.disconnect()
            }
        }
        .onTapGesture {
            withAnimation(.easeInOut(duration: 0.2)) {
                showControls.toggle()
            }
        }
    }

    // MARK: - Header

    private var headerView: some View {
        HStack(spacing: 12) {
            // Connection indicator
            HStack(spacing: 6) {
                Circle()
                    .fill(streamService.isConnected ? Theme.success : Theme.error)
                    .frame(width: 8, height: 8)
                    .overlay {
                        if streamService.isConnected && !streamService.isPaused {
                            Circle()
                                .stroke(Theme.success.opacity(0.5), lineWidth: 2)
                                .frame(width: 14, height: 14)
                                .opacity(0.6)
                        }
                    }

                Text(streamService.isConnected ? (streamService.isPaused ? "Paused" : "Live") : "Disconnected")
                    .font(.caption.weight(.medium))
                    .foregroundStyle(Theme.textSecondary)
            }

            Spacer()

            // Display ID
            Text(displayId)
                .font(.caption.monospaced())
                .foregroundStyle(Theme.textMuted)

            // Frame counter
            Text("\(streamService.frameCount) frames")
                .font(.caption2.monospaced())
                .foregroundStyle(Theme.textMuted)

            // PiP button (if supported)
            if streamService.isPipSupported {
                Button {
                    streamService.togglePip()
                    HapticService.lightTap()
                } label: {
                    Image(systemName: streamService.isPipActive ? "pip.exit" : "pip.enter")
                        .font(.system(size: 14, weight: .medium))
                        .foregroundStyle(streamService.isPipActive ? Theme.accent : Theme.textSecondary)
                        .frame(width: 28, height: 28)
                        .background(streamService.isPipActive ? Theme.accent.opacity(0.2) : Theme.backgroundSecondary)
                        .clipShape(Circle())
                }
                .disabled(!streamService.isConnected || !streamService.isPipReady)
                .opacity(streamService.isConnected && streamService.isPipReady ? 1 : 0.5)
            }

            // Close button
            Button {
                dismiss()
            } label: {
                Image(systemName: "xmark")
                    .font(.system(size: 14, weight: .medium))
                    .foregroundStyle(Theme.textSecondary)
                    .frame(width: 28, height: 28)
                    .background(Theme.backgroundSecondary)
                    .clipShape(Circle())
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .background(.ultraThinMaterial)
    }

    // MARK: - Stream Content

    @ViewBuilder
    private var streamContent: some View {
        if let frame = streamService.currentFrame {
            // Show the current frame
            Image(uiImage: frame)
                .resizable()
                .aspectRatio(contentMode: .fit)
                .background(Color.black)
        } else if let error = streamService.errorMessage {
            // Show error state
            VStack(spacing: 16) {
                Image(systemName: "exclamationmark.triangle")
                    .font(.system(size: 48))
                    .foregroundStyle(Theme.warning)

                Text(error)
                    .font(.subheadline)
                    .foregroundStyle(Theme.textSecondary)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal)

                Button {
                    streamService.connect(displayId: displayId)
                } label: {
                    Label("Retry", systemImage: "arrow.clockwise")
                        .font(.subheadline.weight(.medium))
                        .foregroundStyle(.white)
                        .padding(.horizontal, 20)
                        .padding(.vertical, 10)
                        .background(Theme.accent)
                        .clipShape(Capsule())
                }
            }
        } else {
            // Loading state
            VStack(spacing: 16) {
                ProgressView()
                    .progressViewStyle(.circular)
                    .tint(Theme.accent)
                    .scaleEffect(1.2)

                Text("Connecting to desktop...")
                    .font(.subheadline)
                    .foregroundStyle(Theme.textSecondary)
            }
        }
    }

    // MARK: - Controls

    private var controlsView: some View {
        VStack(alignment: .leading, spacing: 16) {
            // Play/Pause, PiP, and reconnect buttons
            HStack(spacing: 16) {
                // Play/Pause
                Button {
                    if streamService.isPaused {
                        streamService.resume()
                    } else {
                        streamService.pause()
                    }
                    HapticService.lightTap()
                } label: {
                    Image(systemName: streamService.isPaused ? "play.fill" : "pause.fill")
                        .font(.system(size: 20))
                        .foregroundStyle(.white)
                        .frame(width: 48, height: 48)
                        .background(Theme.accent)
                        .clipShape(Circle())
                }
                .disabled(!streamService.isConnected)
                .opacity(streamService.isConnected ? 1 : 0.5)

                // Reconnect
                Button {
                    streamService.connect(displayId: displayId)
                    HapticService.lightTap()
                } label: {
                    Image(systemName: "arrow.clockwise")
                        .font(.system(size: 16, weight: .medium))
                        .foregroundStyle(Theme.textPrimary)
                        .frame(width: 44, height: 44)
                        .background(Theme.backgroundSecondary)
                        .clipShape(Circle())
                }

                // PiP button in controls (larger, more visible)
                if streamService.isPipSupported {
                    Button {
                        streamService.togglePip()
                        HapticService.lightTap()
                    } label: {
                        Image(systemName: streamService.isPipActive ? "pip.exit" : "pip.enter")
                            .font(.system(size: 16, weight: .medium))
                            .foregroundStyle(streamService.isPipActive ? .white : Theme.textPrimary)
                            .frame(width: 44, height: 44)
                            .background(streamService.isPipActive ? Theme.accent : Theme.backgroundSecondary)
                            .clipShape(Circle())
                    }
                    .disabled(!streamService.isConnected || !streamService.isPipReady)
                    .opacity(streamService.isConnected && streamService.isPipReady ? 1 : 0.5)
                }

                Spacer()
            }
            .frame(maxWidth: .infinity)

            // Quality and FPS sliders
            VStack(alignment: .leading, spacing: 12) {
                // FPS slider
                HStack(spacing: 8) {
                    Text("FPS")
                        .font(.caption.weight(.medium))
                        .foregroundStyle(Theme.textMuted)
                        .frame(width: 55, alignment: .leading)

                    Slider(value: Binding(
                        get: { Double(streamService.fps) },
                        set: { streamService.setFps(Int($0)) }
                    ), in: 1...30, step: 1)
                    .tint(Theme.accent)

                    Text("\(streamService.fps)")
                        .font(.caption.monospaced())
                        .foregroundStyle(Theme.textSecondary)
                        .frame(width: 30, alignment: .trailing)
                }
                .frame(maxWidth: .infinity)

                // Quality slider
                HStack(spacing: 8) {
                    Text("Quality")
                        .font(.caption.weight(.medium))
                        .foregroundStyle(Theme.textMuted)
                        .frame(width: 55, alignment: .leading)

                    Slider(value: Binding(
                        get: { Double(streamService.quality) },
                        set: { streamService.setQuality(Int($0)) }
                    ), in: 10...100, step: 5)
                    .tint(Theme.accent)

                    Text("\(streamService.quality)%")
                        .font(.caption.monospaced())
                        .foregroundStyle(Theme.textSecondary)
                        .frame(width: 40, alignment: .trailing)
                }
                .frame(maxWidth: .infinity)
            }
            .frame(maxWidth: .infinity)
            .padding(.horizontal, 8)
        }
        .padding(16)
        .frame(maxWidth: .infinity)
        .background(.ultraThinMaterial)
    }
}

// MARK: - PiP Layer View (UIViewRepresentable)

/// A UIView wrapper that sets up the AVSampleBufferDisplayLayer for PiP
struct PipLayerView: UIViewRepresentable {
    let streamService: DesktopStreamService

    func makeUIView(context: Context) -> UIView {
        let view = UIView()
        view.backgroundColor = .clear

        // Set up PiP on the main actor
        Task { @MainActor in
            streamService.setupPip(in: view)
        }

        return view
    }

    func updateUIView(_ uiView: UIView, context: Context) {
        // Update layer frame if needed
        if let layer = streamService.sampleBufferDisplayLayer {
            layer.frame = uiView.bounds
        }
    }
}

// MARK: - Preview

#Preview {
    DesktopStreamView(displayId: ":101")
}
