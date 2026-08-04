import Foundation

/// A project row from `/api/projects/overview`.
///
/// The board joins three sources server-side (trackers, missions, Hermes
/// deliveries); the phone consumes the result rather than re-deriving it, so
/// there is exactly one definition of "which track is stuck" across web, iOS
/// and desktop.
struct ProjectSummary: Codable, Identifiable, Hashable {
    var id: String { slug }

    let slug: String
    /// `attention` | `active` | `paused` | anything else the server adds.
    let bucket: String
    let attentionReasons: [String]
    let updatesCount: Int
    let health: ProjectHealth?
    let conversation: ProjectConversation?
    let latestUpdate: ProjectUpdate?

    enum CodingKeys: String, CodingKey {
        case slug, bucket, health, conversation
        case attentionReasons = "attention_reasons"
        case updatesCount = "updates_count"
        case latestUpdate = "latest_update"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        slug = try container.decode(String.self, forKey: .slug)
        bucket = try container.decodeIfPresent(String.self, forKey: .bucket) ?? "active"
        attentionReasons = try container.decodeIfPresent([String].self, forKey: .attentionReasons) ?? []
        updatesCount = try container.decodeIfPresent(Int.self, forKey: .updatesCount) ?? 0
        health = try container.decodeIfPresent(ProjectHealth.self, forKey: .health)
        conversation = try container.decodeIfPresent(ProjectConversation.self, forKey: .conversation)
        latestUpdate = try container.decodeIfPresent(ProjectUpdate.self, forKey: .latestUpdate)
    }

    /// The conversation to open, but only when it is a real declared binding.
    ///
    /// An inferred conversation is almost always a cron tick's throwaway
    /// session, already ended — offering it as something to tap would hand the
    /// user a dead thread. The server labels the difference; honour it.
    var boundSessionId: String? {
        guard let conversation, conversation.source == "binding" else { return nil }
        return conversation.sessionId
    }

    var needsAttention: Bool {
        bucket == "attention" || (health?.tracksNeedingAttention ?? 0) > 0
    }
}

struct ProjectConversation: Codable, Hashable {
    let sessionId: String
    /// `"binding"` when explicitly declared, `"latest_update"` when guessed.
    let source: String
    let boundAt: String?

    enum CodingKeys: String, CodingKey {
        case source
        case sessionId = "session_id"
        case boundAt = "bound_at"
    }
}

struct ProjectUpdate: Codable, Hashable {
    let headline: String
    let at: String
    let blocker: String?
}

/// Per-track rollup. Mirrors the server's `ProjectHealth`.
struct ProjectHealth: Codable, Hashable {
    let missions: Int
    let active: Int
    let failed: Int
    let overdue: Int
    let tracksNeedingAttention: Int
    let tracks: [TrackHealth]

    enum CodingKeys: String, CodingKey {
        case missions, active, failed, overdue, tracks
        case tracksNeedingAttention = "tracks_needing_attention"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        missions = try container.decodeIfPresent(Int.self, forKey: .missions) ?? 0
        active = try container.decodeIfPresent(Int.self, forKey: .active) ?? 0
        failed = try container.decodeIfPresent(Int.self, forKey: .failed) ?? 0
        overdue = try container.decodeIfPresent(Int.self, forKey: .overdue) ?? 0
        tracksNeedingAttention =
            try container.decodeIfPresent(Int.self, forKey: .tracksNeedingAttention) ?? 0
        tracks = try container.decodeIfPresent([TrackHealth].self, forKey: .tracks) ?? []
    }

    /// The tracks worth showing first. The server already sorts worst-first.
    var worstTracks: [TrackHealth] {
        Array(tracks.prefix(3))
    }
}

struct TrackHealth: Codable, Hashable, Identifiable {
    var id: String { track ?? "" }

    let track: String?
    let verdict: TrackVerdict
    let missions: Int
    let active: Int
    let failed: Int
    let completed: Int
    let overdue: Int
    let lastActivityAt: String?

    enum CodingKeys: String, CodingKey {
        case track, verdict, missions, active, failed, completed, overdue
        case lastActivityAt = "last_activity_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        track = try container.decodeIfPresent(String.self, forKey: .track)
        verdict = try container.decodeIfPresent(TrackVerdict.self, forKey: .verdict) ?? .idle
        missions = try container.decodeIfPresent(Int.self, forKey: .missions) ?? 0
        active = try container.decodeIfPresent(Int.self, forKey: .active) ?? 0
        failed = try container.decodeIfPresent(Int.self, forKey: .failed) ?? 0
        completed = try container.decodeIfPresent(Int.self, forKey: .completed) ?? 0
        overdue = try container.decodeIfPresent(Int.self, forKey: .overdue) ?? 0
        lastActivityAt = try container.decodeIfPresent(String.self, forKey: .lastActivityAt)
    }

    var displayTrack: String { track ?? "untracked" }
}

/// What a track needs from a human, if anything.
///
/// Decoding is lenient: a verdict this build does not know about becomes
/// `.idle` rather than failing the whole projects list. A server that learns a
/// new verdict must not blank the board on every phone that has not updated.
enum TrackVerdict: String, Codable, Hashable {
    case failing
    case overdue
    case active
    case done
    case idle

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = TrackVerdict(rawValue: raw) ?? .idle
    }

    var needsAttention: Bool {
        self == .failing || self == .overdue
    }
}

/// One state a project reported, from `/api/projects/:slug/state`.
struct ProjectState: Codable, Hashable, Identifiable {
    var id: String { firstSeenAt }

    let signature: String
    let headline: String?
    let firstSeenAt: String
    let lastSeenAt: String
    /// How many deliveries reported this same state in a row.
    let observations: Int

    enum CodingKeys: String, CodingKey {
        case signature, headline, observations
        case firstSeenAt = "first_seen_at"
        case lastSeenAt = "last_seen_at"
    }
}
