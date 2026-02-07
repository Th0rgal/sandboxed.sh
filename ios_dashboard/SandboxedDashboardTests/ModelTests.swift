//
//  ModelTests.swift
//  SandboxedDashboardTests
//
//  Unit tests for data models
//

import XCTest
@testable import sandboxed_sh

final class ModelTests: XCTestCase {

    // MARK: - Workspace Tests

    func testWorkspaceDecoding() throws {
        let json = """
        {
            "id": "workspace-id",
            "name": "test-workspace",
            "workspace_type": "container",
            "path": "/var/lib/workspace",
            "status": "ready",
            "error_message": null,
            "created_at": "2024-01-01T00:00:00Z"
        }
        """.data(using: .utf8)!

        let decoder = JSONDecoder()
        let workspace = try decoder.decode(Workspace.self, from: json)

        XCTAssertEqual(workspace.id, "workspace-id")
        XCTAssertEqual(workspace.name, "test-workspace")
        XCTAssertEqual(workspace.workspaceType, .container)
        XCTAssertEqual(workspace.status, .ready)
        XCTAssertNil(workspace.errorMessage)
    }

    func testWorkspaceTypeDisplayName() {
        XCTAssertEqual(WorkspaceType.host.displayName, "Host")
        XCTAssertEqual(WorkspaceType.container.displayName, "Container")
    }

    func testWorkspaceStatusProperties() {
        XCTAssertTrue(WorkspaceStatus.ready.isReady)
        XCTAssertFalse(WorkspaceStatus.pending.isReady)
        XCTAssertFalse(WorkspaceStatus.building.isReady)
        XCTAssertFalse(WorkspaceStatus.error.isReady)
    }

    func testWorkspaceIsDefault() {
        let defaultWorkspace = Workspace.defaultHost
        XCTAssertTrue(defaultWorkspace.isDefault)

        let customWorkspace = Workspace.previewContainer
        XCTAssertFalse(customWorkspace.isDefault)
    }

    // MARK: - Mission Tests

    func testMissionStatusDecoding() throws {
        let statuses = ["active", "completed", "failed", "interrupted", "blocked", "not_feasible"]
        let expectedStatuses: [MissionStatus] = [.active, .completed, .failed, .interrupted, .blocked, .notFeasible]

        for (json, expected) in zip(statuses, expectedStatuses) {
            let data = "\"\(json)\"".data(using: .utf8)!
            let status = try JSONDecoder().decode(MissionStatus.self, from: data)
            XCTAssertEqual(status, expected)
        }
    }

    func testMissionStatusDisplayLabel() {
        XCTAssertEqual(MissionStatus.active.displayLabel, "Active")
        XCTAssertEqual(MissionStatus.completed.displayLabel, "Completed")
        XCTAssertEqual(MissionStatus.failed.displayLabel, "Failed")
        XCTAssertEqual(MissionStatus.interrupted.displayLabel, "Interrupted")
        XCTAssertEqual(MissionStatus.blocked.displayLabel, "Blocked")
        XCTAssertEqual(MissionStatus.notFeasible.displayLabel, "Not Feasible")
    }

    func testMissionStatusCanResume() {
        // Active missions cannot be resumed (already active)
        XCTAssertFalse(MissionStatus.active.canResume)
        // Completed missions cannot be resumed
        XCTAssertFalse(MissionStatus.completed.canResume)
    }

    // MARK: - FileEntry Tests

    func testFileEntryDecoding() throws {
        let json = """
        {
            "name": "test.txt",
            "path": "/home/user/test.txt",
            "kind": "file",
            "size": 1024,
            "mtime": 1704067200
        }
        """.data(using: .utf8)!

        let decoder = JSONDecoder()
        let entry = try decoder.decode(FileEntry.self, from: json)

        XCTAssertEqual(entry.name, "test.txt")
        XCTAssertEqual(entry.path, "/home/user/test.txt")
        XCTAssertTrue(entry.isFile)
        XCTAssertFalse(entry.isDirectory)
        XCTAssertEqual(entry.size, 1024)
    }

    func testFileEntryDirectoryDecoding() throws {
        let json = """
        {
            "name": "docs",
            "path": "/home/user/docs",
            "kind": "dir",
            "size": 0,
            "mtime": 1704067200
        }
        """.data(using: .utf8)!

        let decoder = JSONDecoder()
        let entry = try decoder.decode(FileEntry.self, from: json)

        XCTAssertEqual(entry.name, "docs")
        XCTAssertTrue(entry.isDirectory)
        XCTAssertFalse(entry.isFile)
    }

    func testFileEntryFormattedSize() throws {
        let json = """
        {
            "name": "large.bin",
            "path": "/tmp/large.bin",
            "kind": "file",
            "size": 1048576,
            "mtime": 1704067200
        }
        """.data(using: .utf8)!

        let entry = try JSONDecoder().decode(FileEntry.self, from: json)
        // 1MB = 1024 KB = 1 MB
        XCTAssertTrue(entry.formattedSize.contains("MB") || entry.formattedSize.contains("KB"))
    }

    func testFileEntryIcon() throws {
        // Test Swift file icon
        let swiftJson = """
        {"name": "test.swift", "path": "/tmp/test.swift", "kind": "file", "size": 100, "mtime": 0}
        """.data(using: .utf8)!
        let swiftEntry = try JSONDecoder().decode(FileEntry.self, from: swiftJson)
        XCTAssertEqual(swiftEntry.icon, "doc.text.fill")

        // Test directory icon
        let dirJson = """
        {"name": "folder", "path": "/tmp/folder", "kind": "dir", "size": 0, "mtime": 0}
        """.data(using: .utf8)!
        let dirEntry = try JSONDecoder().decode(FileEntry.self, from: dirJson)
        XCTAssertEqual(dirEntry.icon, "folder.fill")
    }
}
