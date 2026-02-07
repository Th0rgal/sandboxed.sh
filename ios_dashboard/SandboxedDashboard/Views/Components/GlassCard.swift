//
//  GlassCard.swift
//  SandboxedDashboard
//
//  Beautiful glass morphism card components with liquid glass effects
//

import SwiftUI

struct GlassCard<Content: View>: View {
    let content: Content
    var padding: CGFloat = 20
    var cornerRadius: CGFloat = 24
    
    init(
        padding: CGFloat = 20,
        cornerRadius: CGFloat = 24,
        @ViewBuilder content: () -> Content
    ) {
        self.content = content()
        self.padding = padding
        self.cornerRadius = cornerRadius
    }
    
    var body: some View {
        content
            .padding(padding)
            .background(.ultraThinMaterial)
            .clipShape(RoundedRectangle(cornerRadius: cornerRadius, style: .continuous))
            .shadow(color: .black.opacity(0.06), radius: 16, y: 8)
            .overlay(
                RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                    .stroke(.white.opacity(0.2), lineWidth: 0.5)
            )
    }
}

struct GlassCardLight<Content: View>: View {
    let content: Content
    var padding: CGFloat = 16
    var cornerRadius: CGFloat = 20
    
    init(
        padding: CGFloat = 16,
        cornerRadius: CGFloat = 20,
        @ViewBuilder content: () -> Content
    ) {
        self.content = content()
        self.padding = padding
        self.cornerRadius = cornerRadius
    }
    
    var body: some View {
        content
            .padding(padding)
            .background(.thinMaterial)
            .clipShape(RoundedRectangle(cornerRadius: cornerRadius, style: .continuous))
            .shadow(color: .black.opacity(0.04), radius: 8, y: 4)
    }
}

struct GlassCardThick<Content: View>: View {
    let content: Content
    var padding: CGFloat = 20
    var cornerRadius: CGFloat = 24
    
    init(
        padding: CGFloat = 20,
        cornerRadius: CGFloat = 24,
        @ViewBuilder content: () -> Content
    ) {
        self.content = content()
        self.padding = padding
        self.cornerRadius = cornerRadius
    }
    
    var body: some View {
        content
            .padding(padding)
            .background(.thickMaterial)
            .clipShape(RoundedRectangle(cornerRadius: cornerRadius, style: .continuous))
            .shadow(color: .black.opacity(0.08), radius: 20, y: 10)
            .overlay(
                RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                    .stroke(.white.opacity(0.25), lineWidth: 1)
            )
    }
}

/// Interactive glass card with hover/press states
struct InteractiveGlassCard<Content: View>: View {
    let content: Content
    var padding: CGFloat = 16
    var cornerRadius: CGFloat = 16
    let action: () -> Void
    
    @State private var isPressed = false
    
    init(
        padding: CGFloat = 16,
        cornerRadius: CGFloat = 16,
        action: @escaping () -> Void,
        @ViewBuilder content: () -> Content
    ) {
        self.content = content()
        self.padding = padding
        self.cornerRadius = cornerRadius
        self.action = action
    }
    
    var body: some View {
        Button(action: action) {
            content
                .padding(padding)
                .background(.ultraThinMaterial.opacity(isPressed ? 0.8 : 1))
                .clipShape(RoundedRectangle(cornerRadius: cornerRadius, style: .continuous))
                .overlay(
                    RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                        .stroke(.white.opacity(isPressed ? 0.25 : 0.12), lineWidth: 0.5)
                )
                .scaleEffect(isPressed ? 0.98 : 1)
        }
        .buttonStyle(.plain)
        .simultaneousGesture(
            DragGesture(minimumDistance: 0)
                .onChanged { _ in
                    withAnimation(.easeInOut(duration: 0.1)) { isPressed = true }
                }
                .onEnded { _ in
                    withAnimation(.easeInOut(duration: 0.15)) { isPressed = false }
                }
        )
    }
}

#Preview {
    ZStack {
        LinearGradient(
            colors: [.indigo.opacity(0.6), .purple.opacity(0.4)],
            startPoint: .topLeading,
            endPoint: .bottomTrailing
        )
        .ignoresSafeArea()
        
        VStack(spacing: 20) {
            GlassCard {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Glass Card")
                        .font(.headline)
                    Text("Beautiful translucent design")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            
            GlassCardLight {
                Text("Light Glass Card")
                    .frame(maxWidth: .infinity)
            }
            
            GlassCardThick {
                Text("Thick Glass Card")
                    .frame(maxWidth: .infinity)
            }
            
            InteractiveGlassCard(action: { print("Tapped") }) {
                Text("Interactive Card - Tap me!")
                    .frame(maxWidth: .infinity)
            }
        }
        .padding()
    }
}
