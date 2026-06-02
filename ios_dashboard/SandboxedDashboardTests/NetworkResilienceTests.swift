//
//  NetworkResilienceTests.swift
//  SandboxedDashboardTests
//
//  Coverage for the bad-network paths reworked in the May 2026 hardening
//  pass: SSE parser correctness, request timeout enforcement, and the
//  UserDefaults→filesystem cache migration. Each test pins a behaviour the
//  user explicitly called out (cold-start latency, large JSON, byte-level
//  stream corruption) so future regressions surface immediately.
//

import XCTest
@testable import sandboxed_sh

final class NetworkResilienceTests: XCTestCase {

    // MARK: - URLSession configuration

    /// The dedicated JSON session must override URLSession.shared's 60s
    /// request / 7d resource defaults. Previously the cold-start chain
    /// could stall the UI behind "Connecting…" for a full minute on a
    /// black-hole host. The bound here is 15s/60s — large enough for a
    /// big mission tail on cellular, small enough that the user sees
    /// feedback if the server is gone.
    func testRequestTimeoutIsBounded() {
        XCTAssertLessThanOrEqual(APIService.requestTimeout, 15)
        XCTAssertGreaterThanOrEqual(APIService.requestTimeout, 5)
        XCTAssertLessThanOrEqual(APIService.resourceTimeout, 90)
    }

    /// SSE inactivity threshold drives the URLSession.timeoutIntervalForRequest
    /// on the streaming session — a healthy stream resets it on every byte;
    /// a half-open socket (cell→wifi handoff, NAT idle reset) errors out
    /// within this window so the reconnect loop fires.
    func testStreamInactivityTimeoutIsBounded() {
        XCTAssertLessThanOrEqual(APIService.streamInactivityTimeout, 60)
        XCTAssertGreaterThanOrEqual(APIService.streamInactivityTimeout, 10)
    }

    /// SSE buffer cap exists at all — without it a server that never emits
    /// a blank line could grow the parser buffer unbounded.
    func testStreamBufferCapIsBounded() {
        XCTAssertLessThanOrEqual(APIService.streamMaxBufferBytes, 4 * 1024 * 1024)
        XCTAssertGreaterThanOrEqual(APIService.streamMaxBufferBytes, 64 * 1024)
    }

    /// A missing/blank saved server URL must not silently connect to a
    /// developer-specific backend. Unless a build explicitly supplies
    /// `SandboxedDefaultAPIBaseURL`, first launch should show setup instead of
    /// marking the API as configured.
    @MainActor
    func testBlankBaseURLRequiresConfigurationWhenNoBundleDefaultExists() {
        let defaults = UserDefaults.standard
        let key = "api_base_url"
        let original = defaults.string(forKey: key)

        defer {
            if let original {
                defaults.set(original, forKey: key)
            } else {
                defaults.removeObject(forKey: key)
            }
        }

        defaults.removeObject(forKey: key)
        XCTAssertEqual(APIService.shared.baseURL, APIService.defaultBaseURL)
        XCTAssertEqual(APIService.defaultBaseURL, "")
        XCTAssertFalse(APIService.shared.isConfigured)

        defaults.set("   ", forKey: key)
        XCTAssertEqual(APIService.shared.baseURL, APIService.defaultBaseURL)
        XCTAssertFalse(APIService.shared.isConfigured)
    }

    func testConnectionStateLabelsAreSpecific() {
        XCTAssertEqual(ConnectionState.authExpired.label, "Session expired")
        XCTAssertEqual(ConnectionState.invalidConfiguration.label, "Check server URL")
        XCTAssertEqual(ConnectionState.degraded.label, "Slow connection · catching up")
    }

    func testStreamServiceKeepsWebSocketAndDiagnosticsAnchors() throws {
        let source = try apiServiceSource()

        XCTAssertTrue(source.contains("ControlStreamDiagnostic"))
        XCTAssertTrue(source.contains("ControlStreamTransport"))
        XCTAssertTrue(source.contains("runControlWebSocket"))
        XCTAssertTrue(source.contains("webSocketTask(with: request)"))
        XCTAssertTrue(source.contains("\"resume\""))
        XCTAssertTrue(source.contains("\"since_seq\""))
        XCTAssertTrue(source.contains("runControlSSE"))
        XCTAssertTrue(source.contains("sinceSeq: sinceSeq"))
        XCTAssertTrue(source.contains("URLQueryItem(name: \"since_seq\""))
        XCTAssertTrue(source.contains("falling back to SSE"))
        XCTAssertTrue(source.contains("web_socket_open_failed"))
        XCTAssertTrue(source.contains("SandboxedDefaultAPIBaseURL"))
        XCTAssertFalse(source.contains("nonisolated static let defaultBaseURL = \"https://agent-backend.thomas.md\""),
                       "the iOS app must not hardcode a personal backend as its default")
        XCTAssertFalse(source.contains("components.path = normalizedPath"),
                       "URL construction must preserve any base URL path prefix")
        XCTAssertFalse(source.contains("headers:"),
                       "diagnostics should not copy request headers or auth tokens")
    }

    func testNetworkMonitorSeparatesReachabilityFromStreamState() throws {
        let source = try networkMonitorSource()

        XCTAssertTrue(source.contains("enum ReachabilityState"))
        XCTAssertTrue(source.contains("enum StreamState"))
        XCTAssertTrue(source.contains("reachabilityState"))
        XCTAssertTrue(source.contains("streamState"))
        XCTAssertTrue(source.contains("noteStreamAuthExpired"))
        XCTAssertTrue(source.contains("noteStreamInvalidConfiguration"))
    }

    // MARK: - Mission cache migration

    /// One-shot UserDefaults→filesystem migration: previous releases stored
    /// per-mission JSON blobs in UserDefaults, so cfprefsd held them
    /// resident for the lifetime of the process. The migration moves each
    /// blob to Caches and erases the UserDefaults key. Idempotent — a
    /// second invocation must be a no-op.
    func testMissionCacheMigrationDrainsUserDefaults() throws {
        let defaults = UserDefaults.standard
        let migrationFlag = "did_migrate_mission_cache_v1"
        let keysKey = "cached_mission_keys"
        let prefix = "cached_mission_"
        let id = "test-mission-\(UUID().uuidString)"
        let blob = Data("{\"mission\":{},\"events\":[],\"cachedAt\":1234}".utf8)

        defer {
            defaults.removeObject(forKey: prefix + id)
            defaults.removeObject(forKey: keysKey)
            defaults.removeObject(forKey: migrationFlag)
        }

        // Seed: pretend a previous build wrote a blob under the legacy key.
        defaults.removeObject(forKey: migrationFlag)
        defaults.set([id], forKey: keysKey)
        defaults.set(blob, forKey: prefix + id)

        ControlView.migrateMissionCacheIfNeeded()

        XCTAssertNil(defaults.data(forKey: prefix + id),
                     "legacy blob should be erased after migration")
        XCTAssertTrue(defaults.bool(forKey: migrationFlag),
                      "flag should be set so a second run is a no-op")

        // Second invocation: must not crash and must not reintroduce data.
        defaults.set(blob, forKey: prefix + id)
        ControlView.migrateMissionCacheIfNeeded()
        XCTAssertEqual(defaults.data(forKey: prefix + id), blob,
                       "idempotent: a fresh write after migration must not be touched again")
    }

    /// Regression for the mission-staleness bug: a long catch-up gap (e.g. the
    /// app away for days, the SSE stream not replaying missed events) must be
    /// fully drained in one resume. The pre-fix logic fetched a single page, so
    /// a backlog larger than one page left the conversation tail frozen on an
    /// old message. `drainDelta` must page through the entire gap.
    func testDeltaResumeDrainsEntireBacklogNotJustOnePage() async throws {
        let pageLimit = 5000
        let serverMax: Int64 = 16_001          // > 3 full pages
        func makeEvent(_ seq: Int64) -> StoredEvent {
            StoredEvent(id: seq, missionId: "m", sequence: seq, eventType: "assistant_message",
                        timestamp: "t", eventId: nil, toolCallId: nil, toolName: nil,
                        content: "c", metadata: [:])
        }

        var pageCalls = 0
        let drain = await ControlView.drainDelta(
            from: 0, pageLimit: pageLimit, maxPages: ControlView.deltaResumeMaxPages
        ) { cursor in
            pageCalls += 1
            let start = cursor + 1
            let end = min(cursor + Int64(pageLimit), serverMax)
            guard start <= end else { return ([], serverMax) }
            let events = (start...end).map { makeEvent($0) }
            return (events, serverMax)
        }

        // Pre-fix behavior would stop after one page (5000 events). The drain
        // must collect every event in the gap and advance the cursor to the max.
        XCTAssertEqual(drain.events.count, Int(serverMax),
                       "delta resume must drain the entire backlog, not a single page")
        XCTAssertEqual(drain.finalCursor, serverMax,
                       "cursor must advance to the server max after draining")
        XCTAssertGreaterThanOrEqual(pageCalls, 4,
                       "a >3-page backlog must require multiple page fetches")
    }

    /// A short first page (already caught up) must stop after one fetch and
    /// advance the cursor to the server's max — even when the only events
    /// returned are fewer than a page (or zero, when newer events are all
    /// filtered-out types like tool calls).
    func testDeltaResumeStopsWhenCaughtUp() async throws {
        var pageCalls = 0
        let drain = await ControlView.drainDelta(
            from: 100, pageLimit: 5000, maxPages: 50
        ) { _ in
            pageCalls += 1
            return ([], 137)   // no new conversation rows, but server advanced to 137
        }
        XCTAssertEqual(pageCalls, 1, "an empty/short page means caught up; stop immediately")
        XCTAssertEqual(drain.finalCursor, 137, "cursor advances past filtered-only gap to server max")
        XCTAssertTrue(drain.events.isEmpty)
    }

    private func apiServiceSource() throws -> String {
        let testFile = URL(fileURLWithPath: #filePath)
        let apiService = testFile
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .appendingPathComponent("SandboxedDashboard/Services/APIService.swift")
        return try String(contentsOf: apiService, encoding: .utf8)
    }

    private func networkMonitorSource() throws -> String {
        let testFile = URL(fileURLWithPath: #filePath)
        let source = testFile
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .appendingPathComponent("SandboxedDashboard/Services/NetworkMonitor.swift")
        return try String(contentsOf: source, encoding: .utf8)
    }
}
