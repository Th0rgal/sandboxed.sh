//
//  GlassButton.swift
//  SandboxedDashboard
//
//  Glass morphism button components
//

import SwiftUI

struct GlassButton: View {
    let title: String
    let icon: String?
    let action: () -> Void
    
    @State private var isPressed = false
    
    init(_ title: String, icon: String? = nil, action: @escaping () -> Void) {
        self.title = title
        self.icon = icon
        self.action = action
    }
    
    var body: some View {
        Button(action: action) {
            HStack(spacing: 8) {
                if let icon = icon {
                    Image(systemName: icon)
                        .font(.system(size: 16, weight: .semibold))
                }
                Text(title)
                    .font(.system(size: 17, weight: .semibold))
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 16)
            .background(.ultraThinMaterial)
            .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 16, style: .continuous)
                    .stroke(.white.opacity(0.2), lineWidth: 0.5)
            )
            .shadow(color: .black.opacity(0.06), radius: 8, y: 4)
            .scaleEffect(isPressed ? 0.97 : 1)
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

struct GlassPrimaryButton: View {
    let title: String
    let icon: String?
    let action: () -> Void
    var isLoading: Bool = false
    var isDisabled: Bool = false
    
    @State private var isPressed = false
    
    init(_ title: String, icon: String? = nil, isLoading: Bool = false, isDisabled: Bool = false, action: @escaping () -> Void) {
        self.title = title
        self.icon = icon
        self.isLoading = isLoading
        self.isDisabled = isDisabled
        self.action = action
    }
    
    var body: some View {
        Button(action: action) {
            HStack(spacing: 8) {
                if isLoading {
                    ProgressView()
                        .tint(.white)
                } else {
                    if let icon = icon {
                        Image(systemName: icon)
                            .font(.system(size: 16, weight: .semibold))
                    }
                    Text(title)
                        .font(.system(size: 17, weight: .semibold))
                }
            }
            .foregroundStyle(.white)
            .frame(maxWidth: .infinity)
            .padding(.vertical, 16)
            .background(
                LinearGradient(
                    colors: [Theme.accent, Theme.accent.opacity(0.85)],
                    startPoint: .top,
                    endPoint: .bottom
                )
            )
            .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
            .shadow(color: Theme.accent.opacity(0.3), radius: 12, y: 6)
            .scaleEffect(isPressed ? 0.97 : 1)
            .opacity(isDisabled ? 0.5 : 1)
        }
        .buttonStyle(.plain)
        .disabled(isLoading || isDisabled)
        .simultaneousGesture(
            DragGesture(minimumDistance: 0)
                .onChanged { _ in
                    guard !isLoading && !isDisabled else { return }
                    withAnimation(.easeInOut(duration: 0.1)) { isPressed = true }
                }
                .onEnded { _ in
                    withAnimation(.easeInOut(duration: 0.15)) { isPressed = false }
                }
        )
    }
}

struct GlassIconButton: View {
    let icon: String
    let action: () -> Void
    var size: CGFloat = 44
    var tint: Color? = nil
    
    @State private var isPressed = false
    
    var body: some View {
        Button(action: action) {
            Image(systemName: icon)
                .font(.system(size: size * 0.4, weight: .semibold))
                .foregroundStyle(tint ?? .primary)
                .frame(width: size, height: size)
                .background(.ultraThinMaterial)
                .clipShape(Circle())
                .overlay(
                    Circle()
                        .stroke(.white.opacity(0.2), lineWidth: 0.5)
                )
                .shadow(color: .black.opacity(0.06), radius: 6, y: 3)
                .scaleEffect(isPressed ? 0.92 : 1)
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

struct GlassDestructiveButton: View {
    let title: String
    let icon: String?
    let action: () -> Void
    
    @State private var isPressed = false
    
    init(_ title: String, icon: String? = nil, action: @escaping () -> Void) {
        self.title = title
        self.icon = icon
        self.action = action
    }
    
    var body: some View {
        Button(action: action) {
            HStack(spacing: 8) {
                if let icon = icon {
                    Image(systemName: icon)
                        .font(.system(size: 16, weight: .semibold))
                }
                Text(title)
                    .font(.system(size: 17, weight: .semibold))
            }
            .foregroundStyle(Theme.error)
            .frame(maxWidth: .infinity)
            .padding(.vertical, 16)
            .background(Theme.error.opacity(0.15))
            .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 16, style: .continuous)
                    .stroke(Theme.error.opacity(0.3), lineWidth: 0.5)
            )
            .scaleEffect(isPressed ? 0.97 : 1)
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
            colors: [.orange.opacity(0.5), .pink.opacity(0.4)],
            startPoint: .topLeading,
            endPoint: .bottomTrailing
        )
        .ignoresSafeArea()
        
        VStack(spacing: 16) {
            GlassButton("Secondary Button", icon: "message") {
                print("Tapped")
            }
            
            GlassPrimaryButton("Send Message", icon: "paperplane.fill") {
                print("Tapped")
            }
            
            GlassPrimaryButton("Loading...", isLoading: true) {
                print("Tapped")
            }
            
            GlassDestructiveButton("Delete", icon: "trash") {
                print("Delete")
            }
            
            HStack(spacing: 16) {
                GlassIconButton(icon: "message.fill") {
                    print("Message")
                }
                
                GlassIconButton(icon: "folder.fill") {
                    print("Folder")
                }
                
                GlassIconButton(icon: "terminal.fill", action: {
                    print("Terminal")
                }, tint: Theme.accent)
                
                GlassIconButton(icon: "xmark", action: {
                    print("Close")
                }, size: 36)
            }
        }
        .padding()
    }
}
