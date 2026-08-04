//
//  HermesConversationTests.swift
//  SandboxedDashboardTests
//
//  Hermes session decoding, transcript mapping, and deep links.
//

import XCTest

@testable import sandboxed_sh

final class HermesConversationTests: XCTestCase {

    private func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T {
        try JSONDecoder().decode(type, from: Data(json.utf8))
    }

    // MARK: - Decoding

    func testSessionDecodingFromListEnvelope() throws {
        let list = try decode(
            HermesSessionList.self,
            """
            {"object": "list", "data": [{
                "id": "api-94765bde00d93d7f",
                "source": "api_server",
                "model": "gpt-5.6-sol",
                "title": null,
                "started_at": 1785410947.08,
                "ended_at": null,
                "message_count": 21,
                "parent_session_id": null,
                "preview": "Reconcile Lido SRv3 closure"
            }]}
            """
        )
        let session = try XCTUnwrap(list.data.first)
        XCTAssertEqual(session.id, "api-94765bde00d93d7f")
        XCTAssertEqual(session.messageCount, 21)
        // Hermes leaves `title` null until its titler runs; the preview stands in.
        XCTAssertEqual(session.displayTitle, "Reconcile Lido SRv3 closure")
    }

    func testDisplayTitleFallsBackToShortIdWhenBlank() throws {
        let session = try decode(
            HermesSession.self,
            #"{"id": "api-0123456789abcdef", "title": "   ", "preview": null}"#
        )
        XCTAssertEqual(session.displayTitle, "Session api-0123")
    }

    // MARK: - Transcript mapping

    func testHistoryMapsRolesOntoChatMessages() {
        let history = [
            makeMessage(id: 1, role: "user", content: "hello"),
            makeMessage(id: 2, role: "assistant", content: "hi", reasoning: "pondering"),
        ]

        let items = HermesTranscript.chatMessages(from: history)

        XCTAssertEqual(items.count, 3)
        XCTAssertTrue(items[0].isUser)
        XCTAssertEqual(items[0].content, "hello")
        // Reasoning renders as a completed thinking row, ahead of the answer.
        XCTAssertTrue(items[1].isThinking)
        XCTAssertEqual(items[1].content, "pondering")
        XCTAssertTrue(items[2].isAssistant)
    }

    func testEmptyAssistantContentDoesNotProduceABubble() {
        let items = HermesTranscript.chatMessages(from: [
            makeMessage(id: 1, role: "assistant", content: "   ")
        ])
        XCTAssertTrue(items.isEmpty)
    }

    func testToolResultIsFoldedIntoItsCall() throws {
        let history = [
            makeMessage(
                id: 1, role: "assistant", content: "",
                toolCalls: [
                    HermesToolCall(
                        id: "call-1",
                        function: .init(name: "terminal", arguments: #"{"command":"ls"}"#)
                    )
                ]
            ),
            makeMessage(id: 2, role: "tool", content: "file.txt", toolCallId: "call-1"),
        ]

        let items = HermesTranscript.chatMessages(from: history)

        XCTAssertEqual(items.count, 1, "the tool result fills in the call, it is not a new row")
        let tool = try XCTUnwrap(items.first?.toolData)
        XCTAssertEqual(tool.name, "terminal")
        XCTAssertEqual(tool.args["command"] as? String, "ls")
        XCTAssertEqual(tool.resultString, "file.txt")
        XCTAssertEqual(tool.state, .success)
    }

    func testToolCallWithoutAPersistedResultIsNotLeftRunning() throws {
        // Regression: a call whose result row never got persisted used to
        // render as "running for N days" (nil result reads as in-flight).
        let items = HermesTranscript.chatMessages(from: [
            makeMessage(
                id: 1, role: "assistant", content: "",
                toolCalls: [HermesToolCall(id: "call-9", function: .init(name: "search", arguments: nil))]
            )
        ])
        let tool = try XCTUnwrap(items.first?.toolData)
        XCTAssertNotNil(tool.result)
        XCTAssertNotNil(tool.endTime)
        XCTAssertTrue(tool.state.isComplete)
    }

    func testOrphanToolResultStillRenders() throws {
        let items = HermesTranscript.chatMessages(from: [
            makeMessage(id: 7, role: "tool", content: "done", toolCallId: "unknown", toolName: "bash")
        ])
        let tool = try XCTUnwrap(items.first?.toolData)
        XCTAssertEqual(tool.name, "bash")
        XCTAssertEqual(tool.resultString, "done")
    }

    // MARK: - Stream events

    func testStreamEventReadsDeltaAndContentInterchangeably() throws {
        let delta = HermesStreamEvent(
            name: "assistant.delta",
            payload: try decode(HermesStreamEvent.Payload.self, #"{"delta": "par"}"#)
        )
        XCTAssertEqual(delta.text, "par")

        let completed = HermesStreamEvent(
            name: "assistant.completed",
            payload: try decode(HermesStreamEvent.Payload.self, #"{"content": "partial"}"#)
        )
        XCTAssertEqual(completed.text, "partial")

        let failure = HermesStreamEvent(
            name: "error",
            payload: try decode(HermesStreamEvent.Payload.self, #"{"message": "boom"}"#)
        )
        XCTAssertEqual(failure.message, "boom")
    }

    // MARK: - Mission origin

    func testMissionDecodesOriginSessionLink() throws {
        let mission = try decode(
            Mission.self,
            """
            {
                "id": "m-1", "status": "active", "history": [],
                "created_at": "2026-08-04T00:00:00Z", "updated_at": "2026-08-04T00:00:00Z",
                "origin": "hermes", "origin_session_id": "api-1234"
            }
            """
        )
        XCTAssertEqual(mission.origin, "hermes")
        XCTAssertEqual(mission.originSessionId, "api-1234")
    }

    func testMissionWithoutOriginStaysNil() throws {
        let mission = try decode(
            Mission.self,
            """
            {
                "id": "m-2", "status": "active", "history": [],
                "created_at": "2026-08-04T00:00:00Z", "updated_at": "2026-08-04T00:00:00Z"
            }
            """
        )
        XCTAssertNil(mission.origin)
        XCTAssertNil(mission.originSessionId)
    }

    // MARK: - Deep links

    @MainActor
    func testDeepLinkRoutesSessionsAndMissions() throws {
        let nav = NavigationState.shared

        nav.handle(url: try XCTUnwrap(URL(string: "sandboxed://session/api-abc")))
        XCTAssertEqual(nav.pendingHermesSessionId, "api-abc")
        XCTAssertEqual(nav.selectedTab, .control)
        nav.pendingHermesSessionId = nil

        nav.handle(url: try XCTUnwrap(URL(string: "sandboxed://mission/m-1")))
        XCTAssertEqual(nav.pendingMissionId, "m-1")
        nav.pendingMissionId = nil

        // Unknown hosts and foreign schemes are ignored.
        nav.handle(url: try XCTUnwrap(URL(string: "sandboxed://workspace/w-1")))
        nav.handle(url: try XCTUnwrap(URL(string: "https://example.com/session/x")))
        XCTAssertNil(nav.pendingHermesSessionId)
        XCTAssertNil(nav.pendingMissionId)
    }

    // MARK: - Helpers

    private func makeMessage(
        id: Int,
        role: String,
        content: String,
        toolCallId: String? = nil,
        toolName: String? = nil,
        reasoning: String? = nil,
        toolCalls: [HermesToolCall]? = nil
    ) -> HermesMessage {
        HermesMessage(
            messageId: id,
            sessionId: "api-test",
            role: role,
            content: content,
            toolCallId: toolCallId,
            toolName: toolName,
            timestamp: 1_785_410_947,
            reasoning: reasoning,
            reasoningContent: nil,
            toolCalls: toolCalls
        )
    }
}
