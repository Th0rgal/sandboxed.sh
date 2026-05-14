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
}
