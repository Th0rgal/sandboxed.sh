import XCTest

final class OrbFlowUITests: XCTestCase {
    @MainActor private func launch() -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments = ["-api_base_url", "http://127.0.0.1:18766", "-orb_test_reset", "YES"]
        app.launch()
        return app
    }
    @MainActor private func capture(_ app: XCUIApplication, _ name: String) {
        Thread.sleep(forTimeInterval: 0.5)
        let shot = XCTAttachment(screenshot: app.screenshot()); shot.name = name; shot.lifetime = .keepAlways; add(shot)
    }
    @MainActor func testPolishedInboxAndReplyLayout() throws {
        let app = XCUIApplication()
        app.launchArguments = ["-api_base_url", "http://127.0.0.1:18766", "-orb_test_reset", "YES"]
        app.launch()
        let projectRow = app.buttons["project.orb-test"]
        XCTAssertTrue(projectRow.waitForExistence(timeout: 20))
        XCTAssertFalse(app.buttons["home.inbox"].exists)
        XCTAssertTrue(app.staticTexts["1 working"].exists)
        capture(app, "home-projects-working")
        let inboxTab = app.buttons["home.tab.inbox"]
        XCTAssertTrue(inboxTab.exists)
        inboxTab.tap()
        let row = app.otherElements["inbox.row.reconnect"]
        XCTAssertTrue(row.waitForExistence(timeout: 20))
        XCTAssertTrue(app.staticTexts["Needs you"].exists)
        XCTAssertFalse(app.staticTexts["Goal"].exists)
        capture(app, "polished-inbox")
        let working = app.buttons["inbox.workingPill"]
        working.tap()
        XCTAssertTrue(app.staticTexts["Working in background"].exists)
        capture(app, "polished-inbox-working")
        working.tap()
        let peek = app.buttons["inbox.peek.reconnect"]
        XCTAssertGreaterThanOrEqual(peek.frame.height, 44)
        peek.tap()
        XCTAssertTrue(app.staticTexts["Restore the interrupted conversation safely."].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["Unresolved"].exists)
        XCTAssertTrue(app.staticTexts["To decide"].exists)
        XCTAssertTrue(app.buttons["Sources"].exists || app.staticTexts["Sources"].exists)
        capture(app, "polished-inbox-preview")
        let reply = app.buttons["inbox.reply.reconnect"]
        XCTAssertGreaterThanOrEqual(reply.frame.height, 44)
        reply.tap()
        let input = app.textFields.matching(NSPredicate(format: "placeholderValue BEGINSWITH 'Send follow-up' OR placeholderValue BEGINSWITH 'Reply to'")).firstMatch
        XCTAssertTrue(input.waitForExistence(timeout: 5))
        input.tap(); input.typeText("Unsent layout check")
        XCTAssertTrue(app.keyboards.firstMatch.waitForExistence(timeout: 5))
        let send = app.buttons["inbox.send.reconnect"]
        XCTAssertGreaterThanOrEqual(send.frame.height, 44)
        XCTAssertTrue(send.isHittable)
        capture(app, "polished-inbox-keyboard")
        // Only fixture data is touched; no reply is submitted.
    }
    @MainActor func testLoadingCacheLatestMessagesAndModeMenu() async throws {
        var reset = URLRequest(url: URL(string: "http://127.0.0.1:18771/__reset")!)
        reset.httpMethod = "POST"
        _ = try await URLSession.shared.data(for: reset)
        let app = XCUIApplication()
        app.launchArguments = ["-api_base_url", "http://127.0.0.1:18771", "-orb_test_reset", "YES"]
        app.launch()
        XCTAssertTrue(app.buttons["project.orb-test"].waitForExistence(timeout: 20))
        app.buttons["project.orb-test"].tap()
        XCTAssertTrue(app.otherElements["conversations-loading"].exists || app.staticTexts["Loading conversations…"].exists)
        XCTAssertFalse(app.staticTexts["No conversations yet"].exists)
        capture(app, "conversation-loading-skeletons")
        XCTAssertTrue(app.buttons["mission.long-chat"].waitForExistence(timeout: 15))
        app.buttons["mission.long-chat"].tap()
        let latest = app.staticTexts["Message 080 — conversation history"]
        XCTAssertTrue(latest.waitForExistence(timeout: 15))
        XCTAssertTrue(latest.isHittable)
        XCTAssertFalse(app.staticTexts["Message 001 — conversation history"].exists)
        capture(app, "latest-messages")
        for _ in 0..<12 {
            if app.buttons["load-earlier"].isHittable { break }
            app.swipeDown()
        }
        XCTAssertTrue(app.buttons["load-earlier"].isHittable)
        app.buttons["load-earlier"].tap()
        // Prepending keeps the previously visible boundary, then allows reading earlier text.
        for _ in 0..<4 {
            if app.staticTexts["Message 060 — conversation history"].isHittable { break }
            app.swipeDown()
        }
        XCTAssertTrue(app.staticTexts["Message 060 — conversation history"].isHittable)
        try await Task.sleep(for: .seconds(4))
        XCTAssertTrue(app.staticTexts["Message 060 — conversation history"].isHittable)
        app.buttons["composer-add"].tap()
        XCTAssertTrue(app.buttons["Photos"].waitForExistence(timeout: 5))
        app.buttons["Mode"].tap()
        XCTAssertTrue(app.buttons["mode-option-plan"].waitForExistence(timeout: 5))
        capture(app, "composer-mode-picker")
        app.buttons["mode-option-plan"].tap()
        XCTAssertTrue(app.buttons["Clear Plan mode"].exists)
        let input = app.textFields["composer"].exists ? app.textFields["composer"] : app.textViews["composer"]
        input.tap(); input.typeText("Outline a small test")
        app.buttons["Send message"].tap()
        XCTAssertTrue(app.staticTexts["/plan Outline a small test"].waitForExistence(timeout: 15))
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(app.buttons["mission.long-chat"].waitForExistence(timeout: 5))
        app.navigationBars.buttons.element(boundBy: 0).tap()
        app.buttons["project.orb-test"].tap()
        XCTAssertTrue(app.buttons["mission.long-chat"].waitForExistence(timeout: 2))
        XCTAssertFalse(app.staticTexts["Loading conversations…"].exists)
    }
    @MainActor func testRichResponseStaysAtBottomAfterLayoutGrowth() async throws {
        var reset = URLRequest(url: URL(string: "http://127.0.0.1:18766/__reset")!)
        reset.httpMethod = "POST"
        _ = try await URLSession.shared.data(for: reset)
        let app = launch()
        XCTAssertTrue(app.buttons["project.orb-test"].waitForExistence(timeout: 20))
        app.buttons["project.orb-test"].tap()
        let row = app.buttons["mission.rich-growth"]
        XCTAssertTrue(row.waitForExistence(timeout: 15))
        row.tap()
        let end = app.webViews.staticTexts["FINAL RESPONSE END"]
        XCTAssertTrue(end.waitForExistence(timeout: 20))
        let file = app.buttons["result.csv"]
        await fulfillment(of: [XCTNSPredicateExpectation(predicate: NSPredicate(format: "hittable == true"), object: file)], timeout: 15)
        try await Task.sleep(for: .seconds(3))
        XCTAssertTrue(end.isHittable)
        XCTAssertTrue(file.isHittable)
        XCTAssertLessThanOrEqual(file.frame.maxY, app.otherElements["conversation-composer"].frame.minY)
        capture(app, "rich-response-final-bottom")
    }
    @MainActor func testSettingsNavigationAndSharedAddressCreation() throws {
        let app = launch()
        XCTAssertTrue(app.buttons["project.orb-test"].waitForExistence(timeout: 20))
        app.buttons["Settings"].tap()
        XCTAssertTrue(app.buttons["settings.machines"].waitForExistence(timeout: 5))
        capture(app, "settings-home")
        app.buttons["settings.machines"].tap()
        XCTAssertTrue(app.buttons["ssh.fixture-host"].waitForExistence(timeout: 10))
        capture(app, "settings-machines")
        app.buttons["ssh.add"].tap()
        let name = app.textFields["ssh.name"]
        XCTAssertTrue(name.waitForExistence(timeout: 5))
        name.tap(); name.typeText("iPhone address")
        let host = app.textFields["ssh.host"]
        host.tap(); host.typeText("phone-test.example")
        app.buttons["Save"].tap()
        XCTAssertTrue(app.staticTexts["iPhone address"].waitForExistence(timeout: 10))
        app.navigationBars["Machines"].buttons["Settings"].tap()
        app.buttons["settings.providers"].tap()
        XCTAssertTrue(app.staticTexts["Fixture provider"].waitForExistence(timeout: 10))
        capture(app, "settings-providers")
    }
    @MainActor func testCompactComposerKeyboardAndSettings() throws {
        let app = launch()
        XCTAssertTrue(app.buttons["project.orb-test"].waitForExistence(timeout: 20))
        capture(app, "compact-projects")
        app.buttons["Settings"].tap()
        XCTAssertTrue(app.buttons["settings.backend"].waitForExistence(timeout: 5))
        app.buttons["settings.backend"].tap()
        XCTAssertTrue(app.textFields["server-url"].waitForExistence(timeout: 10))
        XCTAssertEqual(app.textFields["server-url"].value as? String, "http://127.0.0.1:18766")
        capture(app, "compact-server-settings")
        app.navigationBars["Backend"].buttons["Settings"].tap()
        app.buttons["Done"].tap()
        app.buttons["project.orb-test"].tap()
        capture(app, "compact-project-folders")
        app.buttons["mission.rich-chatgpt"].tap()
        XCTAssertTrue(app.webViews.staticTexts["Rendement annualisé"].waitForExistence(timeout: 15))
        XCTAssertFalse(app.staticTexts["response complete"].exists)
        let composer = app.otherElements["conversation-composer"]
        XCTAssertGreaterThan(composer.frame.height, 60)
        XCTAssertLessThanOrEqual(composer.frame.height, 110)
        XCTContext.runActivity(named: "Idle composer height: \(composer.frame.height) pt") { _ in }
        XCTAssertGreaterThanOrEqual(app.buttons["Send message"].frame.width, 44)
        capture(app, "compact-composer-idle")
        let input = app.textFields["composer"].exists ? app.textFields["composer"] : app.textViews["composer"]
        input.tap(); input.typeText("Explain the calculation\nand compare the assumptions.")
        XCTAssertTrue(app.keyboards.firstMatch.waitForExistence(timeout: 10))
        XCTAssertLessThanOrEqual(composer.frame.maxY, app.keyboards.firstMatch.frame.minY + 1)
        XCTAssertTrue(app.buttons["Send message"].isHittable)
        capture(app, "compact-composer-keyboard")
        app.buttons["agent-selection"].tap()
        XCTAssertTrue(app.buttons["picker.model"].waitForExistence(timeout: 10))
        capture(app, "compact-agent-settings")
        app.buttons["Done"].tap()
        XCTAssertEqual(input.value as? String, "Explain the calculation\nand compare the assumptions.")
    }
    @MainActor func testLoginSurvivesRelaunchAndRenewsInvalidToken() async throws {
        let app = XCUIApplication()
        app.launchArguments = ["-api_base_url", "http://127.0.0.1:18772", "-orb_test_reset", "YES", "-orb_test_reset_auth", "YES"]
        app.launch()
        XCTAssertTrue(app.secureTextFields.firstMatch.waitForExistence(timeout: 20))
        app.secureTextFields.firstMatch.tap(); app.secureTextFields.firstMatch.typeText("orb-test-password")
        app.buttons["Sign In"].tap()
        XCTAssertTrue(app.buttons["project.orb-test"].waitForExistence(timeout: 15))
        app.terminate()
        app.launchArguments = ["-api_base_url", "http://127.0.0.1:18772"]
        app.launch()
        XCTAssertTrue(app.buttons["project.orb-test"].waitForExistence(timeout: 15))
        XCTAssertFalse(app.secureTextFields.firstMatch.exists)
        app.terminate()
        let (beforeData, _) = try await URLSession.shared.data(from: URL(string: "http://127.0.0.1:18772/__counts")!)
        let before = try XCTUnwrap(JSONSerialization.jsonObject(with: beforeData) as? [String: Int])
        var expire = URLRequest(url: URL(string: "http://127.0.0.1:18772/__expire")!)
        expire.httpMethod = "POST"
        _ = try await URLSession.shared.data(for: expire)
        app.launch()
        XCTAssertTrue(app.buttons["project.orb-test"].waitForExistence(timeout: 20))
        XCTAssertFalse(app.buttons["reconnect"].exists)
        XCTAssertFalse(app.secureTextFields.firstMatch.exists)
        let (data, _) = try await URLSession.shared.data(from: URL(string: "http://127.0.0.1:18772/__counts")!)
        let counts = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Int])
        XCTAssertEqual(counts["/api/auth/login"], (before["/api/auth/login"] ?? 0) + 1)
        capture(app, "saved-login-renewed")
    }
    @MainActor func testExpiredSessionAndServerPassword() throws {
        addUIInterruptionMonitor(withDescription: "Password AutoFill prompt") { alert in
            if alert.buttons["Not Now"].exists { alert.buttons["Not Now"].tap(); return true }
            return false
        }
        let app = XCUIApplication()
        app.launchArguments = ["-api_base_url", "http://127.0.0.1:18770", "-orb_test_reset", "YES", "-orb_test_reset_auth", "YES"]
        app.launch()
        XCTAssertTrue(app.buttons["reconnect"].waitForExistence(timeout: 20))
        XCTAssertFalse(app.buttons["New project"].exists)
        XCTAssertFalse(app.searchFields.firstMatch.exists)
        XCTAssertFalse(app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "401")).firstMatch.exists)
        capture(app, "auth-reconnect")
        app.buttons["reconnect"].tap()
        let password = app.secureTextFields["server-password"]
        XCTAssertTrue(password.waitForExistence(timeout: 10))
        password.tap(); password.typeText("wrong")
        app.buttons["Connect"].tap()
        XCTAssertTrue(app.staticTexts["Incorrect password. Please try again."].waitForExistence(timeout: 10))
        password.tap(); password.typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: 5) + "orb-test-password")
        app.buttons["Connect"].tap()
        XCTAssertTrue(app.buttons["project.orb-test"].waitForExistence(timeout: 15))
        if app.buttons["Not Now"].waitForExistence(timeout: 5) {
            app.buttons["Not Now"].tap()
            XCTAssertTrue(app.buttons["Not Now"].waitForNonExistence(timeout: 5))
        }
        app.buttons["Settings"].tap()
        XCTAssertTrue(app.buttons["settings.backend"].waitForExistence(timeout: 5))
        app.buttons["settings.backend"].tap()
        XCTAssertTrue(password.waitForExistence(timeout: 10))
        password.tap(); password.typeText("wrong")
        app.buttons["Connect"].tap()
        XCTAssertTrue(app.staticTexts["Incorrect password. Please try again."].waitForExistence(timeout: 10))
        app.navigationBars["Backend"].buttons["Settings"].tap()
        app.buttons["Done"].tap()
        XCTAssertTrue(app.buttons["project.orb-test"].exists)
        if app.buttons["Not Now"].waitForExistence(timeout: 5) {
            app.buttons["Not Now"].tap()
            XCTAssertTrue(app.buttons["Not Now"].waitForNonExistence(timeout: 5))
        }
        app.buttons["Settings"].tap()
        XCTAssertTrue(app.buttons["settings.backend"].waitForExistence(timeout: 5))
        app.buttons["settings.backend"].tap()
        XCTAssertTrue(password.waitForExistence(timeout: 10))
        password.tap()
        password.typeText("orb-test-password")
        capture(app, "auth-server-password")
        app.buttons["Connect"].tap()
        XCTAssertTrue(password.waitForNonExistence(timeout: 15))
        XCTAssertTrue(app.buttons["project.orb-test"].waitForExistence(timeout: 15))
    }
    @MainActor func testCloudReconnectNoticeRemainsVisible() throws {
        let app = launch()
        XCTAssertTrue(app.buttons["project.orb-test"].waitForExistence(timeout: 20))
        app.buttons["project.orb-test"].tap(); app.buttons["mission.reconnect"].tap()
        XCTAssertTrue(app.staticTexts["Reconnect your ChatGPT account in Orb on your Mac."].waitForExistence(timeout: 15))
        let input = app.textFields["composer"].exists ? app.textFields["composer"] : app.textViews["composer"]
        input.tap(); input.typeText("Continue")
        XCTAssertFalse(app.buttons["Send message"].isEnabled)
        capture(app, "compact-reconnect-notice")
    }
    @MainActor func testLoginLayoutWithKeyboard() throws {
        let app = XCUIApplication()
        app.launchArguments = ["-api_base_url", "http://127.0.0.1:18769", "-orb_test_reset", "YES"]
        app.launch()
        XCTAssertTrue(app.staticTexts["Sign in to Orb"].waitForExistence(timeout: 15))
        let password = app.secureTextFields.firstMatch
        XCTAssertTrue(password.waitForExistence(timeout: 10))
        password.tap(); password.typeText("layout-only")
        XCTAssertTrue(app.keyboards.firstMatch.waitForExistence(timeout: 10))
        XCTAssertTrue(app.buttons["Sign In"].isHittable)
        capture(app, "compact-login-keyboard")
    }
    @MainActor func testProjectsFoldersConversationAndContext() throws {
        let app = launch()
        XCTAssertTrue(app.buttons["project.orb-test"].waitForExistence(timeout: 20))
        app.buttons["project.orb-test"].tap()
        XCTAssertTrue(app.buttons["folder.Design/Images"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.buttons["mission.local-only"].exists)
        app.buttons["mission.existing"].tap()
        XCTAssertTrue(app.textFields["composer"].waitForExistence(timeout: 10) || app.textViews["composer"].exists)
        let shot = XCTAttachment(screenshot: app.screenshot()); shot.name = "orb-conversation"; shot.lifetime = .keepAlways; add(shot)
        app.buttons["Conversation actions"].tap()
        app.buttons["Project context"].tap()
        XCTAssertTrue(app.buttons["README.md"].waitForExistence(timeout: 10))
        app.buttons["README.md"].tap()
        XCTAssertTrue(app.buttons["Edit"].waitForExistence(timeout: 10))
        app.buttons["Edit"].tap()
        XCTAssertTrue(app.textViews["document-editor"].exists)
        capture(app, "compact-document-editor")
        app.textViews["document-editor"].tap(); app.textViews["document-editor"].typeText("\nUX review")
        app.buttons["Preview"].tap()
        XCTAssertTrue(app.buttons["Save"].exists)
        XCTAssertTrue(app.webViews.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "UX review")).firstMatch.waitForExistence(timeout: 15))
        capture(app, "compact-document-preview")
        app.buttons["Save"].tap()
        XCTAssertTrue(app.buttons["Edit"].waitForExistence(timeout: 10))
    }
    @MainActor func testProjectColorIsChosenAndKeptAcrossLaunches() throws {
        var app = launch()
        func choose(_ name: String) {
            XCTAssertTrue(app.buttons["Project actions"].waitForExistence(timeout: 10))
            app.buttons["Project actions"].tap()
            XCTAssertTrue(app.buttons["project-color"].waitForExistence(timeout: 5))
            app.buttons["project-color"].tap()
            XCTAssertTrue(app.buttons[name].waitForExistence(timeout: 5))
        }
        XCTAssertTrue(app.buttons["project.orb-test"].waitForExistence(timeout: 20))
        app.buttons["project.orb-test"].tap()
        choose("Green")
        XCTAssertEqual(app.buttons.matching(NSPredicate(format: "label IN %@", ["Default", "Blue", "Green", "Amber", "Rose", "Purple"])).count, 6)
        capture(app, "project-color-menu")
        app.buttons["Green"].tap()
        XCTAssertTrue(app.buttons["folder.Design/Images"].waitForExistence(timeout: 10))
        capture(app, "project-color-folders")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(app.buttons["project.orb-test"].waitForExistence(timeout: 10))
        capture(app, "project-color-projects")
        app.terminate()
        app = launch()
        XCTAssertTrue(app.buttons["project.orb-test"].waitForExistence(timeout: 20))
        app.buttons["project.orb-test"].tap()
        choose("Green")
        XCTAssertTrue(app.buttons["Green"].isSelected)
        XCTAssertFalse(app.buttons["Default"].isSelected)
        app.buttons["Default"].tap()
        choose("Default")
        XCTAssertTrue(app.buttons["Default"].isSelected)
        app.buttons["Default"].tap()
    }
    @MainActor func testRichChatGPTConversationAndReopen() throws {
        let app = launch()
        XCTAssertTrue(app.buttons["project.orb-test"].waitForExistence(timeout: 20))
        app.buttons["project.orb-test"].tap()
        let row = app.buttons["mission.rich-chatgpt"]
        for _ in 0..<12 { if row.isHittable { break }; app.swipeUp() }
        XCTAssertTrue(row.exists); row.tap()
        XCTAssertTrue(app.webViews.firstMatch.waitForExistence(timeout: 15))
        let title = app.webViews.staticTexts["Rendement annualisé"]
        XCTAssertTrue(title.waitForExistence(timeout: 15))
        let shot = XCTAttachment(screenshot: app.screenshot()); shot.name = "chatgpt-rich-conversation"; shot.lifetime = .keepAlways; add(shot)
        // Rendering stays available after the app process is stopped and restarted.
        app.terminate(); app.launch()
        app.buttons["project.orb-test"].tap()
        for _ in 0..<12 { if row.isHittable { break }; app.swipeUp() }
        row.tap()
        XCTAssertTrue(title.waitForExistence(timeout: 15))
        for _ in 0..<8 {
            if title.isHittable { break }
            app.swipeDown()
        }
        let artifact = app.webViews.buttons["Graphique généré"]
        for _ in 0..<12 {
            if artifact.exists && artifact.frame.midY > 120 && artifact.frame.midY < app.frame.height * 0.70 { break }
            app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.65)).press(forDuration: 0.05, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.25)))
        }
        XCTAssertTrue(artifact.exists); artifact.tap()
        XCTAssertTrue(app.buttons["Close preview"].waitForExistence(timeout: 15))
        XCTAssertTrue(app.images["artifact-image"].waitForExistence(timeout: 15))
        let previewShot = XCTAttachment(screenshot: app.screenshot()); previewShot.name = "chatgpt-image-preview"; previewShot.lifetime = .keepAlways; add(previewShot)
        app.buttons["Close preview"].tap()
        let dataFile = app.webViews.links["Télécharger les données"]
        for _ in 0..<6 {
            if dataFile.exists && dataFile.frame.midY < app.frame.height * 0.70 { break }
            app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.65)).press(forDuration: 0.05, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.25)))
        }
        dataFile.tap()
        XCTAssertTrue(app.staticTexts["artifact-text"].waitForExistence(timeout: 15))
        XCTAssertTrue(app.staticTexts["artifact-text"].label.contains("1999,243"))
        app.buttons["Close preview"].tap()
    }
    @MainActor func testCreateChatGPT() throws { try createCloud("ChatGPT", account: "chatgpt account") }
    @MainActor func testCreateCursorCloud() throws { try createCloud("Cursor Cloud", account: "cursor_cloud account") }
    @MainActor func testCreateGrokBot() throws { try createCloud("Grok Bot", account: "grok_bot account") }
    @MainActor private func createCloud(_ service: String, account: String) throws {
        let app = launch()
        XCTAssertTrue(app.buttons["project.orb-test"].waitForExistence(timeout: 20))
        app.buttons["project.orb-test"].tap(); app.buttons["new-agent"].tap(); app.buttons["agent-selection"].tap()
        app.buttons["picker.service"].tap(); app.buttons[service].tap()
        app.buttons["picker.account"].tap(); app.buttons[account].tap()
        if service == "Cursor Cloud" { app.buttons["picker.repository"].tap(); app.buttons["https://github.com/example/orb-test"].tap(); app.textFields["picker.git-ref"].tap(); app.textFields["picker.git-ref"].typeText("main") }
        app.buttons["Done"].tap()
        let input = app.textFields["composer"].exists ? app.textFields["composer"] : app.textViews["composer"]
        input.tap(); input.typeText("ORB_CLOUD_TEST")
        app.buttons["Send message"].tap()
        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "ORB_TEST_OK")).firstMatch.waitForExistence(timeout: 20))
        input.tap(); input.typeText("Follow up")
        app.buttons["Send message"].tap()
        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "FOLLOWUP_OK")).firstMatch.waitForExistence(timeout: 20))
    }
    @MainActor func testCreateClassicAgent() throws {
        let app = launch()
        XCTAssertTrue(app.buttons["project.orb-test"].waitForExistence(timeout: 20))
        app.buttons["project.orb-test"].tap()
        app.buttons["new-agent"].tap()
        app.buttons["agent-selection"].tap()
        app.buttons["picker.harness"].tap()
        XCTAssertFalse(app.buttons["ChatGPT UI"].exists)
        XCTAssertFalse(app.buttons["chatgpt_ui"].exists)
        app.buttons["Claude Code"].tap()
        app.buttons["Done"].tap()
        let input = app.textFields["composer"].exists ? app.textFields["composer"] : app.textViews["composer"]
        input.tap(); input.typeText("ORB_UI_CREATE")
        app.buttons["Send message"].tap()
        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "ORB_TEST_OK")).firstMatch.waitForExistence(timeout: 20))
    }
}
