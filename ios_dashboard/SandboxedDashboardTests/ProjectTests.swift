//
//  ProjectTests.swift
//  SandboxedDashboardTests
//
//  The projects board as the phone consumes it.
//

import XCTest

@testable import sandboxed_sh

final class ProjectTests: XCTestCase {
    private func decode(_ json: String) throws -> ProjectSummary {
        try JSONDecoder().decode(ProjectSummary.self, from: Data(json.utf8))
    }

    func testDecodesAProjectWithHealthAndBinding() throws {
        let project = try decode(
            """
            {
              "slug": "verity",
              "bucket": "attention",
              "attention_reasons": ["blocker reported: CI"],
              "updates_count": 80,
              "conversation": {
                "session_id": "20260804_103847_86ca5c",
                "source": "binding",
                "bound_at": "2026-08-04T17:05:39Z"
              },
              "health": {
                "missions": 353, "active": 3, "failed": 12, "overdue": 2,
                "tracks_needing_attention": 4,
                "tracks": [
                  { "track": "lean431-migration", "verdict": "failing",
                    "missions": 6, "active": 0, "failed": 3, "completed": 3,
                    "overdue": 0, "last_activity_at": "2026-08-03T21:14:02Z" }
                ]
              }
            }
            """
        )
        XCTAssertEqual(project.slug, "verity")
        XCTAssertEqual(project.boundSessionId, "20260804_103847_86ca5c")
        XCTAssertTrue(project.needsAttention)
        XCTAssertEqual(project.health?.tracksNeedingAttention, 4)
        XCTAssertEqual(project.health?.tracks.first?.verdict, .failing)
        XCTAssertTrue(project.health?.tracks.first?.verdict.needsAttention ?? false)
    }

    /// An inferred conversation is almost always a cron tick's throwaway
    /// session, already ended. Offering it as tappable would hand the user a
    /// dead thread, so only a declared binding counts.
    func testAnInferredConversationIsNotOfferedAsBound() throws {
        let project = try decode(
            """
            {
              "slug": "beal", "bucket": "active",
              "conversation": {
                "session_id": "cron_b039e22383e5_20260803_105450",
                "source": "latest_update"
              }
            }
            """
        )
        XCTAssertNil(project.boundSessionId)
        XCTAssertEqual(project.conversation?.sessionId, "cron_b039e22383e5_20260803_105450")
    }

    func testAProjectWithNoConversationDecodes() throws {
        let project = try decode(#"{"slug": "oraxen", "bucket": "paused"}"#)
        XCTAssertNil(project.boundSessionId)
        XCTAssertNil(project.health)
        XCTAssertFalse(project.needsAttention)
        XCTAssertEqual(project.updatesCount, 0)
    }

    /// A verdict this build does not know about must not blank the whole
    /// board: the server may learn new ones long before every phone updates.
    func testAnUnknownVerdictDegradesInsteadOfFailingTheList() throws {
        let project = try decode(
            """
            {
              "slug": "verity", "bucket": "active",
              "health": {
                "missions": 1, "active": 0, "failed": 0, "overdue": 0,
                "tracks_needing_attention": 0,
                "tracks": [{ "track": "t", "verdict": "quantum-entangled",
                             "missions": 1, "active": 0, "failed": 0,
                             "completed": 0, "overdue": 0 }]
              }
            }
            """
        )
        XCTAssertEqual(project.health?.tracks.first?.verdict, .idle)
    }

    func testAnUntrackedBucketDisplaysAsUntracked() throws {
        let project = try decode(
            """
            {
              "slug": "verity", "bucket": "active",
              "health": { "missions": 2, "active": 2, "failed": 0, "overdue": 0,
                          "tracks_needing_attention": 0,
                          "tracks": [{ "verdict": "active", "missions": 2,
                                       "active": 2, "failed": 0, "completed": 0,
                                       "overdue": 0 }] }
            }
            """
        )
        XCTAssertEqual(project.health?.tracks.first?.displayTrack, "untracked")
        XCTAssertNil(project.health?.tracks.first?.track)
    }

    func testDecodesAStateTimelineEntry() throws {
        let state = try JSONDecoder().decode(
            ProjectState.self,
            from: Data(
                """
                {
                  "signature": "phase1-stack|7dba916|clean-ready|none",
                  "headline": "Verity Phase 1",
                  "first_seen_at": "2026-08-04T10:00:00Z",
                  "last_seen_at": "2026-08-04T18:30:00Z",
                  "observations": 34
                }
                """.utf8)
        )
        XCTAssertEqual(state.observations, 34)
        XCTAssertEqual(state.id, "2026-08-04T10:00:00Z")
    }
}

final class MissionProjectMetadataTests: XCTestCase {
    private func mission(project: String?, track: String?) throws -> Mission {
        var fields: [String] = [
            #""id": "11111111-1111-1111-1111-111111111111""#,
            #""status": "active""#,
            #""history": []"#,
            #""created_at": "2026-08-04T10:00:00Z""#,
            #""updated_at": "2026-08-04T10:00:00Z""#,
        ]
        if let project { fields.append("\"project\": \"\(project)\"") }
        if let track { fields.append("\"track\": \"\(track)\"") }
        let json = "{\(fields.joined(separator: ","))}"
        return try JSONDecoder().decode(Mission.self, from: Data(json.utf8))
    }

    func testDecodesProjectAndTrack() throws {
        let decoded = try mission(project: "verity", track: "phase1d/core-c3")
        XCTAssertEqual(decoded.project, "verity")
        XCTAssertEqual(decoded.track, "phase1d/core-c3")
        XCTAssertEqual(decoded.projectLabel, "verity · phase1d/core-c3")
    }

    func testAProjectWithoutATrackLabelsAsJustTheProject() throws {
        XCTAssertEqual(try mission(project: "verity", track: nil).projectLabel, "verity")
    }

    /// Most missions carry no project at all (3909 of 7000 on prod), so the
    /// absence has to be ordinary rather than an empty separator artefact.
    func testAnUntaggedMissionHasNoLabel() throws {
        XCTAssertNil(try mission(project: nil, track: nil).projectLabel)
        XCTAssertNil(try mission(project: nil, track: "orphan-track").projectLabel)
    }
}
