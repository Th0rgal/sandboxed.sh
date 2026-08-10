//
//  ControlPerformanceDiagnostics.swift
//  SandboxedDashboard
//
//  Lightweight runtime instrumentation for the Control chat surface.
//

import Foundation
import SwiftUI
import os

@MainActor
final class ControlPerformanceDiagnostics {
    static let shared = ControlPerformanceDiagnostics()

    private let logger = Logger(subsystem: "md.thomas.openagent.dashboard", category: "ControlPerformance")
    private let signposter = OSSignposter(subsystem: "md.thomas.openagent.dashboard", category: "ControlPerformance")

    private var records: [ControlPerformanceRecord] = []
    private var renderCounts: [String: Int] = [:]
    private let maxRecords = 24

    private init() {}

    var recentRecords: [ControlPerformanceRecord] {
        records
    }

    var hotRenderCounts: [(name: String, count: Int)] {
        renderCounts
            .sorted { lhs, rhs in
                if lhs.value != rhs.value { return lhs.value > rhs.value }
                return lhs.key < rhs.key
            }
            .prefix(6)
            .map { ($0.key, $0.value) }
    }

    func reset() {
        records.removeAll(keepingCapacity: true)
        renderCounts.removeAll(keepingCapacity: true)
    }

    func recordBodyRender(_ name: String) {
        renderCounts[name, default: 0] += 1
    }

    func measure<T>(
        _ name: StaticString,
        detail: String = "",
        count: Int? = nil,
        operation: () throws -> T
    ) rethrows -> T {
        let state = signposter.beginInterval(name)
        let start = ContinuousClock.now
        defer {
            let duration = start.duration(to: ContinuousClock.now)
            signposter.endInterval(name, state)
            record(String(describing: name), duration: duration, detail: detail, count: count)
        }
        return try operation()
    }

    func measureAsync<T: Sendable>(
        _ name: StaticString,
        detail: String = "",
        count: Int? = nil,
        operation: () async throws -> T
    ) async rethrows -> T {
        let state = signposter.beginInterval(name)
        let start = ContinuousClock.now
        defer {
            let duration = start.duration(to: ContinuousClock.now)
            signposter.endInterval(name, state)
            record(String(describing: name), duration: duration, detail: detail, count: count)
        }
        return try await operation()
    }

    private func record(_ name: String, duration: Duration, detail: String, count: Int?) {
        let millis = Double(duration.components.seconds) * 1_000
            + Double(duration.components.attoseconds) / 1_000_000_000_000_000

        let record = ControlPerformanceRecord(
            name: name,
            detail: detail,
            durationMilliseconds: millis,
            count: count,
            timestamp: Date()
        )
        records.append(record)
        if records.count > maxRecords {
            records.removeFirst(records.count - maxRecords)
        }

        if millis >= 16 {
            logger.warning("\(name, privacy: .public) took \(millis, format: .fixed(precision: 1), privacy: .public) ms count=\(count ?? -1, privacy: .public) \(detail, privacy: .public)")
        } else {
            logger.debug("\(name, privacy: .public) took \(millis, format: .fixed(precision: 1), privacy: .public) ms count=\(count ?? -1, privacy: .public) \(detail, privacy: .public)")
        }
    }
}

struct ControlPerformanceRecord: Identifiable, Equatable {
    let id = UUID()
    let name: String
    let detail: String
    let durationMilliseconds: Double
    let count: Int?
    let timestamp: Date

    var compactDuration: String {
        String(format: "%.1f ms", durationMilliseconds)
    }
}

private struct ControlPerformanceDiagnosticsEnabledKey: EnvironmentKey {
    static let defaultValue = false
}

extension EnvironmentValues {
    var controlPerformanceDiagnosticsEnabled: Bool {
        get { self[ControlPerformanceDiagnosticsEnabledKey.self] }
        set { self[ControlPerformanceDiagnosticsEnabledKey.self] = newValue }
    }
}

struct ControlBodyRenderProbe: ViewModifier {
    @Environment(\.controlPerformanceDiagnosticsEnabled) private var enabled
    let name: String

    func body(content: Content) -> some View {
        if enabled {
            ControlPerformanceDiagnostics.shared.recordBodyRender(name)
        }
        return content
    }
}
