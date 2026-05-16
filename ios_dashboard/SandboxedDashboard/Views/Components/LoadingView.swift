//
//  LoadingView.swift
//  SandboxedDashboard
//
//  Loading indicators and shimmer effects
//

import SwiftUI

struct LoadingView: View {
    var message: String = "Loading..."
    
    var body: some View {
        VStack(spacing: 16) {
            ProgressView()
                .scaleEffect(1.2)
                .tint(Theme.accent)
            
            Text(message)
                .font(.subheadline)
                .foregroundStyle(Theme.textSecondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

struct ShimmerView: View {
    @State private var isAnimating = false
    
    var body: some View {
        LinearGradient(
            colors: [
                Color.white.opacity(0.04),
                Color.white.opacity(0.08),
                Color.white.opacity(0.04)
            ],
            startPoint: .leading,
            endPoint: .trailing
        )
        .offset(x: isAnimating ? 300 : -300)
        .animation(.linear(duration: 1.5).repeatForever(autoreverses: false), value: isAnimating)
        .onAppear {
            isAnimating = true
        }
    }
}

struct ShimmerRow: View {
    var height: CGFloat = 16
    var width: CGFloat? = nil
    
    var body: some View {
        RoundedRectangle(cornerRadius: 4)
            .fill(Color.white.opacity(0.06))
            .frame(width: width, height: height)
            .overlay(ShimmerView())
            .clipShape(RoundedRectangle(cornerRadius: 4))
    }
}

struct ShimmerCard: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 12) {
                RoundedRectangle(cornerRadius: 8)
                    .fill(Color.white.opacity(0.06))
                    .frame(width: 40, height: 40)
                    .overlay(ShimmerView())
                    .clipShape(RoundedRectangle(cornerRadius: 8))

                VStack(alignment: .leading, spacing: 6) {
                    ShimmerRow(height: 14, width: 120)
                    ShimmerRow(height: 12, width: 80)
                }
            }

            ShimmerRow(height: 12)
            ShimmerRow(height: 12, width: 200)
        }
        .padding(16)
        .background(Color.white.opacity(0.03))
        .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
    }
}

// Skeleton mirroring WorkspaceCard (WorkspacesView.swift:128-174):
// two-row VStack — top HStack(small symbol + name + spacer + status badge);
// bottom HStack(type pill + spacer + chevron). Padding 16, corner 12, 1pt
// border at white 0.06.
struct ShimmerWorkspaceCard: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                ShimmerRow(height: 16, width: 16)
                ShimmerRow(height: 16, width: 140)
                Spacer(minLength: 8)
                ShimmerRow(height: 18, width: 64)
            }
            HStack {
                ShimmerRow(height: 18, width: 70)
                Spacer()
                ShimmerRow(height: 12, width: 8)
            }
        }
        .padding()
        .background(Color.white.opacity(0.02))
        .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 12)
                .stroke(Color.white.opacity(0.06), lineWidth: 1)
        )
    }
}

// Skeleton mirroring MissionRow (HistoryView.swift:384-456): single HStack
// with 40x40 icon (corner 10) + VStack(title + badge row) + Spacer +
// VStack(timestamp + chevron). Padding 14, corner 14, ultraThinMaterial.
struct ShimmerMissionRow: View {
    var body: some View {
        HStack(spacing: 14) {
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .fill(Color.white.opacity(0.06))
                .frame(width: 40, height: 40)
                .overlay(ShimmerView())
                .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))

            VStack(alignment: .leading, spacing: 4) {
                ShimmerRow(height: 14, width: 180)
                HStack(spacing: 6) {
                    ShimmerRow(height: 16, width: 56)
                    ShimmerRow(height: 14, width: 48)
                    ShimmerRow(height: 12, width: 40)
                }
            }

            Spacer()

            VStack(alignment: .trailing, spacing: 4) {
                ShimmerRow(height: 12, width: 48)
                ShimmerRow(height: 12, width: 8)
            }
        }
        .padding(14)
        .background(.ultraThinMaterial)
        .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .stroke(Theme.border, lineWidth: 0.5)
        )
    }
}

// Skeleton mirroring FileRow (FilesView.swift:607-687): single HStack with
// 48x48 icon (corner 12) + VStack(name + meta row) + Spacer + chevron.
// Padding 12v/16h, corner 14, Theme.backgroundSecondary.
struct ShimmerFileRow: View {
    var body: some View {
        HStack(spacing: 16) {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .fill(Color.white.opacity(0.06))
                .frame(width: 48, height: 48)
                .overlay(ShimmerView())
                .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))

            VStack(alignment: .leading, spacing: 4) {
                ShimmerRow(height: 16, width: 160)
                HStack(spacing: 6) {
                    ShimmerRow(height: 12, width: 48)
                    ShimmerRow(height: 12, width: 60)
                    ShimmerRow(height: 12, width: 40)
                }
            }

            Spacer()

            ShimmerRow(height: 14, width: 8)
        }
        .padding(.vertical, 12)
        .padding(.horizontal, 16)
        .background(Theme.backgroundSecondary)
        .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
    }
}

struct EmptyStateView: View {
    let icon: String
    let title: String
    let message: String
    var action: (() -> Void)? = nil
    var actionLabel: String = "Try Again"
    
    var body: some View {
        VStack(spacing: 20) {
            Image(systemName: icon)
                .font(.system(size: 48))
                .foregroundStyle(Theme.textTertiary)
            
            VStack(spacing: 8) {
                Text(title)
                    .font(.title3.bold())
                    .foregroundStyle(Theme.textPrimary)
                
                Text(message)
                    .font(.subheadline)
                    .foregroundStyle(Theme.textSecondary)
                    .multilineTextAlignment(.center)
            }
            
            if let action = action {
                Button(action: action) {
                    Text(actionLabel)
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(Theme.accent)
                        .padding(.horizontal, 20)
                        .padding(.vertical, 10)
                        .background(Theme.accent.opacity(0.15))
                        .clipShape(Capsule())
                }
            }
        }
        .padding(32)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

#Preview {
    ScrollView {
        VStack(spacing: 16) {
            LoadingView()
                .frame(height: 120)

            ShimmerWorkspaceCard()
            ShimmerMissionRow()
            ShimmerFileRow()

            EmptyStateView(
                icon: "message.badge.filled.fill",
                title: "No Messages",
                message: "Start a conversation with the agent",
                action: { print("Tapped") }
            )
            .frame(height: 200)
        }
        .padding()
    }
    .background(Theme.backgroundPrimary)
}
