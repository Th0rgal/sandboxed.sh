//
//  ThemeTests.swift
//  OpenAgentDashboardTests
//
//  Unit tests for Theme and design system
//

import XCTest
import SwiftUI
@testable import Open_Agent

final class ThemeTests: XCTestCase {

    // MARK: - Color Definitions

    func testBackgroundColorsExist() {
        // Verify all background colors are defined
        XCTAssertNotNil(Theme.backgroundPrimary)
        XCTAssertNotNil(Theme.backgroundSecondary)
        XCTAssertNotNil(Theme.backgroundTertiary)
    }

    func testCardColorsExist() {
        XCTAssertNotNil(Theme.card)
        XCTAssertNotNil(Theme.cardElevated)
    }

    func testBorderColorsExist() {
        XCTAssertNotNil(Theme.border)
        XCTAssertNotNil(Theme.borderElevated)
        XCTAssertNotNil(Theme.borderSubtle)
    }

    func testSemanticColorsExist() {
        XCTAssertNotNil(Theme.success)
        XCTAssertNotNil(Theme.warning)
        XCTAssertNotNil(Theme.error)
        XCTAssertNotNil(Theme.info)
    }

    func testTextColorsExist() {
        XCTAssertNotNil(Theme.textPrimary)
        XCTAssertNotNil(Theme.textSecondary)
        XCTAssertNotNil(Theme.textTertiary)
        XCTAssertNotNil(Theme.textMuted)
    }

    func testAccentColorsExist() {
        XCTAssertNotNil(Theme.accent)
        XCTAssertNotNil(Theme.accentLight)
    }

    // MARK: - StatusType Tests

    func testStatusTypeColors() {
        // All status types should have a color
        let statuses: [StatusType] = [
            .pending, .running, .active, .completed, .failed,
            .cancelled, .idle, .error, .connected, .disconnected,
            .connecting, .interrupted, .blocked
        ]

        for status in statuses {
            XCTAssertNotNil(status.color, "Status \(status.label) should have a color")
            XCTAssertNotNil(status.backgroundColor, "Status \(status.label) should have a background color")
        }
    }

    func testStatusTypeLabels() {
        XCTAssertEqual(StatusType.pending.label, "Pending")
        XCTAssertEqual(StatusType.running.label, "Running")
        XCTAssertEqual(StatusType.completed.label, "Completed")
        XCTAssertEqual(StatusType.failed.label, "Failed")
        XCTAssertEqual(StatusType.connected.label, "Connected")
    }

    func testStatusTypeIcons() {
        // All status types should have an icon
        let statuses: [StatusType] = [
            .pending, .running, .active, .completed, .failed,
            .cancelled, .idle, .error, .connected, .disconnected,
            .connecting, .interrupted, .blocked
        ]

        for status in statuses {
            XCTAssertFalse(status.icon.isEmpty, "Status \(status.label) should have an icon")
        }
    }

    func testStatusTypePulse() {
        // These statuses should pulse
        XCTAssertTrue(StatusType.running.shouldPulse)
        XCTAssertTrue(StatusType.active.shouldPulse)
        XCTAssertTrue(StatusType.connecting.shouldPulse)

        // These statuses should not pulse
        XCTAssertFalse(StatusType.pending.shouldPulse)
        XCTAssertFalse(StatusType.completed.shouldPulse)
        XCTAssertFalse(StatusType.failed.shouldPulse)
        XCTAssertFalse(StatusType.idle.shouldPulse)
    }
}
