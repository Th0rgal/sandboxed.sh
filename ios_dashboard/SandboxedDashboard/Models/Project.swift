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
    /// Roster title, when set — shown instead of the raw slug.
    let title: String?
    /// The controller's declared next step, from the roster record.
    let nextAction: String?
    /// `attention` | `active` | `paused` | anything else the server adds.
    let bucket: String
    let attentionReasons: [String]
    let updatesCount: Int
    let health: ProjectHealth?
    let conversation: ProjectConversation?
    let latestUpdate: ProjectUpdate?
    /// Live missions grouped under this project.
    let missions: [ProjectMissionChip]

    enum CodingKeys: String, CodingKey {
        case slug, title, bucket, health, conversation, missions
        case nextAction = "next_action"
        case attentionReasons = "attention_reasons"
        case updatesCount = "updates_count"
        case latestUpdate = "latest_update"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        slug = try container.decode(String.self, forKey: .slug)
        title = try container.decodeIfPresent(String.self, forKey: .title)
        nextAction = try container.decodeIfPresent(String.self, forKey: .nextAction)
        bucket = try container.decodeIfPresent(String.self, forKey: .bucket) ?? "active"
        attentionReasons = try container.decodeIfPresent([String].self, forKey: .attentionReasons) ?? []
        updatesCount = try container.decodeIfPresent(Int.self, forKey: .updatesCount) ?? 0
        health = try container.decodeIfPresent(ProjectHealth.self, forKey: .health)
        conversation = try container.decodeIfPresent(ProjectConversation.self, forKey: .conversation)
        latestUpdate = try container.decodeIfPresent(ProjectUpdate.self, forKey: .latestUpdate)
        missions = try container.decodeIfPresent([ProjectMissionChip].self, forKey: .missions) ?? []
    }

    /// What to render as the project's name: the roster title when one was
    /// set, the slug otherwise.
    var displayName: String {
        if let title, !title.isEmpty { return title }
        return slug
    }

    /// The controller-reported mode from the latest delivery: `active`,
    /// `blocked`, or `paused`. Nil when the controller hasn't reported one.
    var mode: ControllerMode? { latestUpdate?.controllerMode }

    /// Live missions worth showing as "working": active or awaiting the user.
    var liveMissions: [ProjectMissionChip] {
        missions.filter { $0.status == "active" || $0.status == "awaiting_user" }
    }

    /// Missions that need the operator — the attention rail.
    var missionsNeedingAttention: [ProjectMissionChip] {
        missions.filter { $0.status == "awaiting_user" }
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
    /// The `[CTRL: … mode=… ]` mode the controller reported: `active`,
    /// `blocked[:cause]`, or `paused[:reason]`. Absent for controllers that
    /// predate the trailer — render nothing rather than a guessed state.
    let mode: String?

    var controllerMode: ControllerMode? { ControllerMode(raw: mode) }
}

/// A controller's regime, parsed from the `mode` string (`blocked:transport-cap`
/// splits into base + cause). Absent input yields nil — never an invented state.
struct ControllerMode: Hashable {
    enum Base: String { case active, blocked, paused }
    let base: Base
    let cause: String?

    init?(raw: String?) {
        guard let raw = raw?.trimmingCharacters(in: .whitespaces).lowercased(),
              !raw.isEmpty else { return nil }
        let parts = raw.split(separator: ":", maxSplits: 1).map(String.init)
        guard let base = Base(rawValue: parts[0]) else { return nil }
        self.base = base
        self.cause = parts.count > 1 ? parts[1] : nil
    }

    var label: String { cause.map { "\(base.rawValue): \($0)" } ?? base.rawValue }
}

/// A live mission under a project, from the overview's `missions` chips.
struct ProjectMissionChip: Codable, Hashable, Identifiable {
    let id: String
    let status: String
    let title: String?
    let updatedAt: String?
    let githubPr: String?

    enum CodingKeys: String, CodingKey {
        case id, status, title
        case updatedAt = "updated_at"
        case githubPr = "github_pr"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decodeIfPresent(String.self, forKey: .id) ?? UUID().uuidString
        status = try c.decodeIfPresent(String.self, forKey: .status) ?? "unknown"
        title = try c.decodeIfPresent(String.self, forKey: .title)
        updatedAt = try c.decodeIfPresent(String.self, forKey: .updatedAt)
        githubPr = try c.decodeIfPresent(String.self, forKey: .githubPr)
    }

    var displayTitle: String { title ?? String(id.prefix(8)) }
    var needsAttention: Bool { status == "awaiting_user" }
}

// MARK: - Project detail (`/api/projects/:slug`)

/// The structured project object from `/api/projects/:slug`: record + grant +
/// tracks + open decisions + bound conversation. Powers the project detail
/// view, where the controller ↔ project ↔ sessions link is made visible.
struct ProjectDetail: Codable, Hashable {
    let project: ProjectRecord
    let grant: ProjectGrant?
    let tracks: [ProjectTrack]
    let openDecisions: [ProjectDecision]
    let conversation: ProjectConversation?

    enum CodingKeys: String, CodingKey {
        case project, grant, tracks, conversation
        case openDecisions = "open_decisions"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        project = try c.decode(ProjectRecord.self, forKey: .project)
        grant = try c.decodeIfPresent(ProjectGrant.self, forKey: .grant)
        tracks = try c.decodeIfPresent([ProjectTrack].self, forKey: .tracks) ?? []
        openDecisions = try c.decodeIfPresent([ProjectDecision].self, forKey: .openDecisions) ?? []
        conversation = try c.decodeIfPresent(ProjectConversation.self, forKey: .conversation)
    }

    /// The bound control conversation — only when explicitly declared, not a
    /// throwaway cron tick guessed from the latest delivery.
    var boundSessionId: String? {
        guard let conversation, conversation.source == "binding" else { return nil }
        return conversation.sessionId
    }
}

/// The authoritative project record.
struct ProjectRecord: Codable, Hashable {
    let slug: String
    let title: String?
    let objective: String?
    let status: String
    /// `active` | `blocked` | `paused` — the controller's regime.
    let mode: String?
    let waitTicks: Int
    let nextAction: String?
    let blocker: String?
    /// Which cron controller drives this project. The controller ↔ project link.
    let controllerCronId: String?
    let repository: String?

    enum CodingKeys: String, CodingKey {
        case slug, title, objective, status, mode, blocker, repository
        case waitTicks = "wait_ticks"
        case nextAction = "next_action"
        case controllerCronId = "controller_cron_id"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        slug = try c.decode(String.self, forKey: .slug)
        title = try c.decodeIfPresent(String.self, forKey: .title)
        objective = try c.decodeIfPresent(String.self, forKey: .objective)
        status = try c.decodeIfPresent(String.self, forKey: .status) ?? "active"
        mode = try c.decodeIfPresent(String.self, forKey: .mode)
        waitTicks = try c.decodeIfPresent(Int.self, forKey: .waitTicks) ?? 0
        nextAction = try c.decodeIfPresent(String.self, forKey: .nextAction)
        blocker = try c.decodeIfPresent(String.self, forKey: .blocker)
        controllerCronId = try c.decodeIfPresent(String.self, forKey: .controllerCronId)
        repository = try c.decodeIfPresent(String.self, forKey: .repository)
    }

    var controllerMode: ControllerMode? { ControllerMode(raw: mode) }
    var displayTitle: String { title ?? slug }
}

/// The autonomy grant.
struct ProjectGrant: Codable, Hashable {
    let mergeAuthority: String?
    let budgetPerTick: String?
    let pauseReason: String?
    let resumeCondition: String?
    let materialBar: String?

    enum CodingKeys: String, CodingKey {
        case mergeAuthority = "merge_authority"
        case budgetPerTick = "budget_per_tick"
        case pauseReason = "pause_reason"
        case resumeCondition = "resume_condition"
        case materialBar = "material_bar"
    }
}

/// One workstream from the detail endpoint.
struct ProjectTrack: Codable, Hashable, Identifiable {
    var id: String { track }
    let track: String
    let desiredState: String?
    let status: String?

    enum CodingKeys: String, CodingKey {
        case track, status
        case desiredState = "desired_state"
    }
}

/// An open question the controller batched for the operator.
struct ProjectDecision: Codable, Hashable, Identifiable {
    var id: String { at }
    let at: String
    let question: String
    let rationale: String?
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

// MARK: - Unread tracking

/// What the user had seen of a project the last time they opened its detail:
/// the delivery count and the newest delivery timestamp at that moment.
struct ProjectLastSeen: Codable, Equatable {
    let updatesCount: Int
    let latestAt: String?
}

/// New deliveries since a project was last opened. Pure so it is testable;
/// mirrors the web board's `unreadCountFor`.
///
/// - Never opened → every update is unread.
/// - Count grew → the delta.
/// - Count flat but `latest_update.at` newer → at least 1 (the updates window
///   is rolling: newer deliveries can replace older ones without moving the
///   count).
enum ProjectUnread {
    static func count(updatesCount: Int, latestAt: String?, seen: ProjectLastSeen?) -> Int {
        guard let seen else { return max(0, updatesCount) }
        let delta = updatesCount - seen.updatesCount
        if delta > 0 { return delta }
        guard let latestAt else { return 0 }
        guard let seenAt = seen.latestAt else { return 1 }
        guard let latest = parseDate(latestAt), let previous = parseDate(seenAt) else { return 0 }
        return latest > previous ? 1 : 0
    }

    private static let isoWithFractional: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f
    }()
    private static let iso = ISO8601DateFormatter()

    static func parseDate(_ raw: String) -> Date? {
        isoWithFractional.date(from: raw) ?? iso.date(from: raw)
    }
}

/// Client-side "seen" state for the projects board, persisted in UserDefaults
/// keyed by project slug — the backend has no per-user read state.
final class ProjectUnreadStore {
    static let shared = ProjectUnreadStore()

    private let defaults: UserDefaults
    private let storageKey = "projects.lastSeen.v1"

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    func unreadCount(for project: ProjectSummary) -> Int {
        ProjectUnread.count(
            updatesCount: project.updatesCount,
            latestAt: project.latestUpdate?.at,
            seen: lastSeen()[project.slug]
        )
    }

    /// Opening the project detail marks everything current as seen.
    func markSeen(_ project: ProjectSummary) {
        var map = lastSeen()
        map[project.slug] = ProjectLastSeen(
            updatesCount: project.updatesCount,
            latestAt: project.latestUpdate?.at
        )
        if let data = try? JSONEncoder().encode(map) {
            defaults.set(data, forKey: storageKey)
        }
    }

    private func lastSeen() -> [String: ProjectLastSeen] {
        guard let data = defaults.data(forKey: storageKey),
              let map = try? JSONDecoder().decode([String: ProjectLastSeen].self, from: data)
        else { return [:] }
        return map
    }
}
