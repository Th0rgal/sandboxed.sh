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
    VStack(spacing: 24) {
        LoadingView()
            .frame(height: 150)
        
        ShimmerCard()
            .padding()
        
        EmptyStateView(
            icon: "message.badge.filled.fill",
            title: "No Messages",
            message: "Start a conversation with the agent",
            action: { print("Tapped") }
        )
        .frame(height: 250)
    }
    .background(Theme.backgroundPrimary)
}
