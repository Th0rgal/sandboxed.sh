//
//  HermesConversationUITests.swift
//  SandboxedDashboardUITests
//
//  Drives the Hermes conversation surface against a real backend.
//
//  Credentials are never committed: the test reads them from the environment
//  and skips itself when they're absent, so CI and other developers aren't
//  broken by its absence. Run it with:
//
//    xcodebuild test -scheme SandboxedDashboard \
//      -destination 'platform=iOS Simulator,name=<device>' \
//      -only-testing:SandboxedDashboardUITests \
//      TEST_RUNNER_SANDBOXED_API_URL=https://… \
//      TEST_RUNNER_SANDBOXED_JWT=<token>
//
//  `TEST_RUNNER_`-prefixed variables are forwarded to the runner process by
//  xcodebuild with the prefix stripped.
//

import XCTest

final class HermesConversationUITests: XCTestCase {

    override func setUp() {
        continueAfterFailure = false
    }

    /// Opens the switcher, selects the first Hermes session, and checks the
    /// conversation renders (header + composer). Exercises the whole chain:
    /// session list → proxy → transcript mapping → shared renderer.
    @MainActor
    func testOpensAHermesSessionFromTheSwitcher() throws {
        let environment = ProcessInfo.processInfo.environment
        let app = XCUIApplication()
        // `-key value` launch arguments land in UserDefaults' argument domain,
        // which is how the app reads both of these — no test-only code path.
        // Without them the app falls back to whatever this simulator was
        // already configured with.
        if let baseURL = environment["SANDBOXED_API_URL"], !baseURL.isEmpty,
            let jwt = environment["SANDBOXED_JWT"], !jwt.isEmpty
        {
            app.launchArguments = ["-api_base_url", baseURL, "-jwt_token", jwt]
        }
        app.launch()

        // The switcher lives behind the layers button in the control toolbar.
        // Its absence means the app is sitting on the setup/login screen.
        let switcher = app.buttons["Switch mission"]
        guard switcher.waitForExistence(timeout: 30) else {
            throw XCTSkip(
                "App is not signed in. Pass SANDBOXED_API_URL / SANDBOXED_JWT via TEST_RUNNER_*, "
                    + "or configure this simulator once by hand."
            )
        }
        switcher.tap()

        // Give the sheet's session fetch a beat, then scroll: the section sits
        // below Running/Just Completed, and List only instantiates rows near
        // the viewport, so an offscreen header genuinely does not exist yet.
        let sessionsHeader = app.staticTexts.matching(
            NSPredicate(format: "label CONTAINS[c] %@", "Hermes Session")
        ).firstMatch
        _ = sessionsHeader.waitForExistence(timeout: 10)
        var scrolls = 0
        while !sessionsHeader.exists && scrolls < 12 {
            app.swipeUp()
            scrolls += 1
        }
        guard sessionsHeader.exists else {
            attach(app.windows.firstMatch.screenshot(), named: "switcher-without-sessions")
            throw XCTSkip("No Hermes sessions section — Hermes is not adopted on this backend")
        }
        attach(app.windows.firstMatch.screenshot(), named: "switcher-with-sessions")

        // First row under the section header.
        let sessionRow = app.staticTexts.matching(
            NSPredicate(format: "label BEGINSWITH %@", "Hermes session")
        ).firstMatch
        XCTAssertTrue(sessionRow.waitForExistence(timeout: 10), "no session row rendered")
        sessionRow.tap()

        // The conversation view: its own header subtitle and composer.
        XCTAssertTrue(
            app.staticTexts["Hermes session"].waitForExistence(timeout: 20),
            "the Hermes conversation header did not render"
        )
        XCTAssertTrue(
            app.textViews["Message Hermes…"].exists
                || app.textFields["Message Hermes…"].exists,
            "the Hermes composer did not render"
        )

        attach(app.windows.firstMatch.screenshot(), named: "hermes-conversation")
    }

    private func attach(_ screenshot: XCUIScreenshot, named name: String) {
        let attachment = XCTAttachment(screenshot: screenshot)
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}
