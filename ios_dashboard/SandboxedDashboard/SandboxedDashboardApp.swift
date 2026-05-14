//
//  SandboxedDashboardApp.swift
//  SandboxedDashboard
//
//  iOS Dashboard for sandboxed.sh with liquid glass design
//

import SwiftUI

@main
struct SandboxedDashboardApp: App {
    init() {
        // Drain legacy in-UserDefaults mission cache blobs into the on-disk
        // Caches store. Each blob was a multi-KB-to-multi-MB JSON payload
        // held resident by cfprefsd; the migration runs at most once.
        ControlView.migrateMissionCacheIfNeeded()
    }

    var body: some Scene {
        WindowGroup {
            ContentView()
                .preferredColorScheme(.dark)
        }
    }
}
