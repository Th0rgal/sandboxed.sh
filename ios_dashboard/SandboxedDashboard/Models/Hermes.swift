import Foundation

// Models for Hermes sessions — the assistant gateway's conversations, reached
// through the backend proxy at `/api/assistant/hermes/api/sessions*` (which
// swaps the dashboard JWT for Hermes' own API key server-side).
//
// A Hermes session is a conversation like a mission is, but owned by Hermes:
// it can run on any platform (Telegram, cron, this app) and can spawn missions
// as workers (`Mission.originSessionId`).

// MARK: - Session

struct HermesSession: Codable, Identifiable, Hashable {
    let id: String
    let source: String?
    let model: String?
    let title: String?
    let startedAt: Double?
    let endedAt: Double?
    let lastActive: Double?
    let preview: String?
    let messageCount: Int?
    let parentSessionId: String?

    enum CodingKeys: String, CodingKey {
        case id, source, model, title, preview
        case startedAt = "started_at"
        case endedAt = "ended_at"
        case lastActive = "last_active"
        case messageCount = "message_count"
        case parentSessionId = "parent_session_id"
    }

    func hash(into hasher: inout Hasher) { hasher.combine(id) }
    static func == (lhs: HermesSession, rhs: HermesSession) -> Bool { lhs.id == rhs.id }

    /// Hermes leaves `title` null until its titler runs, so fall back to the
    /// first-message preview before the raw id.
    var displayTitle: String {
        if let title, !title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            return title
        }
        if let preview, !preview.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            return preview.count > 60 ? String(preview.prefix(60)) + "…" : preview
        }
        return "Session \(id.prefix(8))"
    }

    var lastActiveDate: Date? {
        guard let seconds = lastActive ?? startedAt else { return nil }
        return Date(timeIntervalSince1970: seconds)
    }
}

/// Envelope of `GET /api/assistant/hermes/api/sessions`.
///
/// Hermes has shipped this list under both `sessions` and `data` depending on
/// the version; the web client accepts either, so decode both here too rather
/// than let a gateway upgrade silently empty the session list (which reads as
/// "Hermes not available" and hides the whole feature).
struct HermesSessionList: Codable {
    let sessions: [HermesSession]

    enum CodingKeys: String, CodingKey {
        case sessions, data
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessions =
            try container.decodeIfPresent([HermesSession].self, forKey: .sessions)
            ?? container.decodeIfPresent([HermesSession].self, forKey: .data)
            ?? []
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(sessions, forKey: .sessions)
    }
}

/// Envelope of `POST /api/assistant/hermes/api/sessions`.
struct HermesSessionEnvelope: Codable {
    let session: HermesSession
}

// MARK: - Message

struct HermesMessage: Codable, Identifiable {
    /// Hermes numbers persisted messages; synthesized rows have no id.
    let messageId: Int?
    let sessionId: String?
    let role: String
    let content: String?
    let toolCallId: String?
    let toolName: String?
    let timestamp: Double?
    let reasoning: String?
    let reasoningContent: String?
    let toolCalls: [HermesToolCall]?

    var id: String {
        if let messageId { return "hermes-msg-\(messageId)" }
        return "hermes-msg-\(sessionId ?? "?")-\(role)-\(timestamp ?? 0)"
    }

    enum CodingKeys: String, CodingKey {
        case role, content, timestamp, reasoning
        case messageId = "id"
        case sessionId = "session_id"
        case toolCallId = "tool_call_id"
        case toolName = "tool_name"
        case reasoningContent = "reasoning_content"
        case toolCalls = "tool_calls"
    }

    var reasoningText: String? {
        let text = (reasoningContent ?? reasoning ?? "")
            .trimmingCharacters(in: .whitespacesAndNewlines)
        return text.isEmpty ? nil : text
    }
}

/// A tool call attached to an assistant message (OpenAI-shaped).
struct HermesToolCall: Codable {
    let id: String?
    let function: HermesToolFunction?

    struct HermesToolFunction: Codable {
        let name: String?
        /// JSON-encoded argument object.
        let arguments: String?
    }

    var toolName: String { function?.name ?? "tool" }
}

/// Envelope of `GET /api/assistant/hermes/api/sessions/:id/messages`.
///
/// Same dual-key tolerance as `HermesSessionList` — a transcript that decodes
/// to nothing would render as an empty conversation with no error.
struct HermesMessageList: Codable {
    let sessionId: String?
    let messages: [HermesMessage]

    enum CodingKeys: String, CodingKey {
        case messages, data
        case sessionId = "session_id"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionId = try container.decodeIfPresent(String.self, forKey: .sessionId)
        messages =
            try container.decodeIfPresent([HermesMessage].self, forKey: .messages)
            ?? container.decodeIfPresent([HermesMessage].self, forKey: .data)
            ?? []
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encodeIfPresent(sessionId, forKey: .sessionId)
        try container.encode(messages, forKey: .messages)
    }
}

// MARK: - Chat stream

/// One named SSE event from `POST …/sessions/:id/chat/stream`.
///
/// Unlike the Ask stream (which carries a `type` field in the payload), Hermes
/// puts the discriminator in the SSE `event:` line, so the name is attached
/// while parsing rather than decoded.
struct HermesStreamEvent {
    let name: String
    let text: String?
    let toolName: String?
    let preview: String?
    let message: String?

    /// Decodable half of the frame; `name` comes from the `event:` line.
    struct Payload: Decodable {
        let text: String?
        let content: String?
        let delta: String?
        let toolName: String?
        let preview: String?
        let message: String?
        let error: String?

        enum CodingKeys: String, CodingKey {
            case text, content, delta, preview, message, error
            case toolName = "tool_name"
        }
    }

    init(name: String, payload: Payload) {
        self.name = name
        self.text = payload.text ?? payload.content ?? payload.delta
        self.toolName = payload.toolName
        self.preview = payload.preview
        self.message = payload.message ?? payload.error
    }
}

/// Error carrying a Hermes stream failure, so it surfaces through the throw path.
struct HermesStreamError: LocalizedError {
    let message: String
    var errorDescription: String? { message }
}
