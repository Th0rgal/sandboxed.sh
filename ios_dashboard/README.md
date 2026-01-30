# Sandboxed.sh iOS Dashboard

Native iOS dashboard for Sandboxed.sh with **Liquid Glass** design language.

## Features

- **Control** - Chat interface with the AI agent, real-time streaming
- **History** - View past missions with filtering (active, interrupted, completed, failed)
- **Terminal** - Local shell via WebSocket
- **Files** - Server file explorer with upload/download

### Mission Management

- Create new missions with optional model override
- Resume interrupted or blocked missions
- Mark missions as completed/failed
- View mission status (active, completed, failed, interrupted, blocked, not_feasible)
- Model override display per mission

### Parallel Missions

- View all running missions in a compact horizontal bar
- Switch between parallel missions with a single tap
- Real-time status indicators (running, stalled, severely stalled)
- Cancel running missions directly from the bar
- Automatic polling for running mission updates (every 3s)
- SSE event filtering by mission_id to prevent cross-contamination

## Design System

Built with "Quiet Luxury + Liquid Glass" aesthetic:
- Dark-first design (#121214 deep charcoal backgrounds)
- Glass morphism with `.ultraThinMaterial` and `.thinMaterial`
- Indigo accent color (#6366F1)
- Subtle borders (0.06-0.08 opacity)
- Smooth animations (150-200ms, ease-out)

## Requirements

- iOS 18.0+
- Xcode 16.0+
- Swift 6.0

## Building

### Using XcodeGen

```bash
# Install xcodegen if needed
brew install xcodegen

# Generate project
cd ios_dashboard
xcodegen generate

# Open in Xcode
open OpenAgentDashboard.xcodeproj
```

### Command Line Build

```bash
xcodebuild -project OpenAgentDashboard.xcodeproj \
  -scheme OpenAgentDashboard \
  -destination 'platform=iOS Simulator,name=iPhone 17' \
  build
```

## Configuration

The app connects to the Sandboxed.sh backend. Configure the server URL:
- Default: `https://agent-backend.thomas.md`
- Can be changed in the login screen

In multi-user mode, the login screen also asks for a username.

## Project Structure

```
ios_dashboard/
├── project.yml                 # XcodeGen config
├── OpenAgentDashboard/
│   ├── OpenAgentDashboardApp.swift
│   ├── ContentView.swift       # Auth + Tab navigation
│   ├── DesignSystem/
│   │   └── Theme.swift         # Colors, typography, haptics
│   ├── Models/
│   │   ├── Mission.swift
│   │   ├── ChatMessage.swift
│   │   └── FileEntry.swift
│   ├── Services/
│   │   └── APIService.swift    # HTTP + SSE client
│   ├── Views/
│   │   ├── Control/            # Chat interface
│   │   ├── History/            # Mission history
│   │   ├── Terminal/           # Local shell
│   │   ├── Files/              # File explorer
│   │   └── Components/         # Reusable UI
│   │       ├── GlassButton.swift
│   │       ├── GlassCard.swift
│   │       ├── StatusBadge.swift
│   │       ├── LoadingView.swift
│   │       ├── RunningMissionsBar.swift   # Parallel missions UI
│   │       └── ToolUI/         # Tool UI components
│   └── Assets.xcassets/
└── OpenAgentDashboard.xcodeproj/
```

## Glass Components

### GlassCard
```swift
GlassCard {
    Text("Content with glass background")
}
```

### GlassButton
```swift
GlassPrimaryButton("Send", icon: "paperplane.fill") {
    // action
}

GlassIconButton(icon: "plus", action: { })
```

### StatusBadge
```swift
StatusBadge(status: .running)
StatusDot(status: .connected)
```
