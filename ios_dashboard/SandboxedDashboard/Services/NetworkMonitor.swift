//
//  NetworkMonitor.swift
//  SandboxedDashboard
//
//  Decouples the "is the network reachable" signal from the SSE stream.
//  The previous design only flipped the connection banner from SSE state,
//  which silently lied about reachability whenever the SSE socket happened
//  to be held open by an upstream proxy / NAT while every HTTP call hung.
//
//  This monitor combines two signals:
//
//  1. `NWPathMonitor` — kernel-level "is there an IP path".
//  2. A lightweight `/api/health` probe, fired every `healthInterval` while
//     the SSE has been silent for `staleAfter` seconds. A probe failure is
//     additional evidence we're not actually online.
//
//  The aggregated `state` is published as a `ConnectionState` so callers can
//  bind their banner directly to it (or merge it with their SSE state).
//

import Foundation
import Network
import Observation

@MainActor
@Observable
final class NetworkMonitor {
    enum ReachabilityState: Equatable {
        case reachable
        case offline
        case unhealthy(failures: Int)
    }

    enum StreamState: Equatable {
        case connected
        case reconnecting(attempt: Int)
        case authExpired
        case invalidConfiguration
    }

    /// Time since the last byte from the SSE stream after which we consider
    /// the stream "stale" and start proactive health probes.
    static let staleAfter: TimeInterval = 12

    /// Cadence for the `/api/health` probe while the SSE is stale.
    static let healthInterval: TimeInterval = 10

    /// Number of consecutive health failures before we flip from `degraded`
    /// to `disconnected`. One bad probe could be a tiny blip.
    static let failuresBeforeOffline = 3

    /// Aggregated state — bind your UI to this rather than to the SSE state
    /// directly. This already incorporates the SSE state via
    /// `noteStreamReconnecting` / `noteStreamConnected`, so callers don't
    /// need to do their own merging in the View body (which historically
    /// broke the SwiftUI ViewBuilder type-checker on the large ControlView).
    private(set) var state: ConnectionState = .connected

    /// Last time the SSE stream delivered any byte to us. Updated by callers
    /// via `noteStreamActivity`. Initialised to `.now` so a freshly-launched
    /// app doesn't immediately think the stream is stale.
    private(set) var lastStreamActivity: Date = Date()

    /// Set to true while NWPathMonitor reports `.satisfied`.
    private(set) var pathSatisfied: Bool = true

    /// Path + server health are tracked separately from stream transport state
    /// so auth/config failures do not masquerade as a flaky network.
    private(set) var reachabilityState: ReachabilityState = .reachable

    /// Latest stream state reported by the caller. Merged with reachability to
    /// produce the existing banner-facing `state`.
    private(set) var streamState: StreamState = .connected

    private let pathMonitor = NWPathMonitor()
    private let pathQueue = DispatchQueue(label: "md.thomas.openagent.netpath")
    private var healthTask: Task<Void, Never>?
    private var healthFailures = 0
    private var sceneActive = true

    nonisolated init() {}

    func start() {
        APIService.shared.onSuccessfulAuthenticatedRequest = { [weak self] in
            Task { @MainActor in
                self?.noteSuccessfulRequest()
            }
        }
        pathMonitor.pathUpdateHandler = { [weak self] path in
            guard let self else { return }
            let satisfied = path.status == .satisfied
            Task { @MainActor in
                self.pathSatisfied = satisfied
                self.recomputeState()
            }
        }
        pathMonitor.start(queue: pathQueue)
        startHealthLoop()
    }

    func stop() {
        pathMonitor.cancel()
        healthTask?.cancel()
        healthTask = nil
        APIService.shared.onSuccessfulAuthenticatedRequest = nil
    }

    /// Call this from the SSE event handler on every received byte / event.
    /// Resets the staleness timer and the health-probe failure counter.
    func noteStreamActivity() {
        lastStreamActivity = Date()
        healthFailures = 0
        recomputeState()
    }

    func noteSuccessfulRequest() {
        healthFailures = 0
        recomputeState()
    }

    func setSceneActive(_ active: Bool) {
        sceneActive = active
        recomputeState()
    }

    func isStreamFresh(maxAge: TimeInterval = NetworkMonitor.staleAfter) -> Bool {
        Date().timeIntervalSince(lastStreamActivity) <= maxAge
    }

    /// Signal that the SSE socket itself is in a transitional state. Used by
    /// the stream loop so the banner doesn't flicker between `degraded` and
    /// `reconnecting`.
    func noteStreamReconnecting(attempt: Int) {
        streamState = .reconnecting(attempt: attempt)
        recomputeState()
    }

    /// The SSE has just connected and emitted (or replayed) at least one
    /// real event. Clears any stale/offline state.
    func noteStreamConnected() {
        streamState = .connected
        healthFailures = 0
        lastStreamActivity = Date()
        recomputeState()
    }

    /// The SSE is intentionally torn down (mission switch, view disappear).
    /// Banner stays clean — disconnect is expected, not an error.
    func noteStreamIdle() {
        streamState = .connected
        recomputeState()
    }

    func noteStreamAuthExpired() {
        streamState = .authExpired
        recomputeState()
    }

    func noteStreamInvalidConfiguration() {
        streamState = .invalidConfiguration
        recomputeState()
    }

    private func recomputeState() {
        if !pathSatisfied {
            reachabilityState = .offline
        } else if healthFailures > 0 {
            reachabilityState = .unhealthy(failures: healthFailures)
        } else {
            reachabilityState = .reachable
        }

        // Path down beats everything else.
        if reachabilityState == .offline {
            state = .disconnected
            return
        }
        if healthFailures >= Self.failuresBeforeOffline {
            state = .disconnected
            return
        }
        if case .authExpired = streamState {
            state = .authExpired
            return
        }
        if case .invalidConfiguration = streamState {
            state = .invalidConfiguration
            return
        }
        // Stream reconnecting outranks degraded reachability.
        if case .reconnecting(let attempt) = streamState {
            state = .reconnecting(attempt: attempt)
            return
        }
        let staleness = Date().timeIntervalSince(lastStreamActivity)
        if staleness > Self.staleAfter || healthFailures > 0 {
            state = .degraded
            return
        }
        state = .connected
    }

    private func startHealthLoop() {
        healthTask?.cancel()
        healthTask = Task { [weak self] in
            while !Task.isCancelled {
                let jitter = Double.random(in: 0.8...1.2)
                try? await Task.sleep(for: .seconds(Self.healthInterval * jitter))
                guard !Task.isCancelled else { return }
                await self?.runHealthProbeIfNeeded()
            }
        }
    }

    private func runHealthProbeIfNeeded() async {
        // Only probe when the SSE has been silent long enough to be worth
        // burning a request on. On a busy stream this loop is a no-op.
        guard sceneActive else { return }
        let staleness = Date().timeIntervalSince(lastStreamActivity)
        guard staleness > Self.staleAfter else { return }
        guard pathSatisfied else {
            // No path → no point probing; state is already `.disconnected`.
            return
        }

        do {
            let ok = try await APIService.shared.checkHealth()
            if ok {
                healthFailures = 0
            } else {
                healthFailures += 1
            }
        } catch {
            healthFailures += 1
        }
        recomputeState()
    }
}
