import SwiftUI
import CryptoKit

enum OrbInboxAccount {
    // Cache identity only; Core still authenticates every request.
    static func scope(endpoint: String, token: String?) -> String {
        let token = token ?? ""
        let parts = token.split(separator: ".", omittingEmptySubsequences: false)
        var identity = token.isEmpty ? "anonymous" : "opaque:" + SHA256.hash(data: Data(token.utf8)).map { String(format: "%02x", $0) }.joined()
        if parts.count == 3 {
            var encoded = String(parts[1]).replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
            encoded += String(repeating: "=", count: (4 - encoded.count % 4) % 4)
            if let data = Data(base64Encoded: encoded), let claims = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
               let subject = (claims["sub"] as? String) ?? (claims["user_id"] as? String), !subject.isEmpty {
                identity = "subject:" + subject
            }
        }
        return endpoint.trimmingCharacters(in: CharacterSet(charactersIn: "/")) + ":" + identity
    }
    @MainActor static var current: String { scope(endpoint: OrbCore.shared.endpoint, token: APIService.shared.authToken) }
}

@MainActor
final class OrbSharedInboxState {
    static let shared = OrbSharedInboxState()
    private var mutation = 0
    private var pending = 0
    private var tail: Task<Void, Never>?
    private var scope = ""
    private var outbox: [String: OrbJSON] = [:]
    private var revisions: [String: Double] = [:]
    private(set) var seen: [String: Double] = [:]
    var applying = false
    private var account: String { OrbInboxAccount.current }
    static func outboxKey(_ account: String) -> String {
        "orb.inbox.outbox.v1:" + SHA256.hash(data: Data(account.utf8)).map { String(format: "%02x", $0) }.joined()
    }
    private func persistOutbox() {
        if let data = try? JSONEncoder().encode(outbox) { UserDefaults.standard.set(data, forKey: Self.outboxKey(scope)) }
    }
    private func ensureScope() {
        let current = account
        guard scope != current else { return }
        scope = current; seen = [:]; revisions = [:]
        OrbInboxSettings.shared.bindAccount(current)
        outbox = UserDefaults.standard.data(forKey: Self.outboxKey(current)).flatMap { try? JSONDecoder().decode([String: OrbJSON].self, from: $0) } ?? [:]
        for (path, body) in outbox where path.hasPrefix("seen/") {
            if let stamp = body["stamp"].doubleValue { seen[String(path.dropFirst(5)).removingPercentEncoding ?? String(path.dropFirst(5))] = stamp }
        }
    }
    func write(_ path: String, _ input: OrbJSON) {
        if applying { return }
        let expected = account
        ensureScope()
        var body = input
        if case var .object(fields) = input {
            fields.removeValue(forKey: "mutationAt")
            if fields["clientId"] == nil || fields["mutationSeq"] == nil {
                let client = UserDefaults.standard.string(forKey: "orb.inbox.client.v1") ?? UUID().uuidString
                let sequence = UserDefaults.standard.integer(forKey: "orb.inbox.sequence.v1") + 1
                UserDefaults.standard.set(client, forKey: "orb.inbox.client.v1")
                UserDefaults.standard.set(sequence, forKey: "orb.inbox.sequence.v1")
                let entry = path.hasPrefix("seen/") ? "seen:" + (String(path.dropFirst(5)).removingPercentEncoding ?? String(path.dropFirst(5))) : path
                fields["clientId"] = .string(client); fields["mutationSeq"] = .number(Double(sequence))
                fields["expectedVersion"] = .number(revisions[entry] ?? 0)
            }
            body = .object(fields)
        }
        outbox[path] = body
        persistOutbox()
        mutation += 1; pending += 1
        let previous = tail
        tail = Task {
            await previous?.value
            if expected == account {
                do {
                    _ = try await OrbCore.shared.call("/api/control/inbox-state/" + path, method: "PUT", body: body)
                    if expected == account && outbox[path] == body { outbox.removeValue(forKey: path); persistOutbox() }
                } catch {
                    if let http = error as? OrbHTTPError, http.status == 404, path.hasPrefix("seen/"), expected == account, outbox[path] == body {
                        outbox.removeValue(forKey: path); persistOutbox()
                    }
                    // Other failures retry before the next shared-state read.
                }
            }
            pending -= 1
        }
    }
    @discardableResult func writeSeen(_ id: String, stamp: Double, syncBackend: Bool = true) -> Bool {
        ensureScope()
        let rounded = stamp.rounded(.down)
        let changed = seen[id] != rounded
        seen[id] = rounded
        if syncBackend { write("seen/" + OrbCore.escape(id), .object(["stamp": .number(rounded)])) }
        return changed
    }
    func writePreferences() {
        let p = OrbInboxSettings.shared
        p.bindAccount(account)
        write("preferences", .object(["aiSummary": .bool(p.aiSummary), "includeAutonomous": .bool(p.includeAutonomous), "model": .string(p.model)]))
    }
    func unread(_ row: OrbRow) -> Bool? {
        ensureScope()
        guard scope == account, let stamp = seen[row.id] else { return nil }
        let turn = inboxTimestamp(row.updatedAt) ?? 0
        if stamp < 0 { return turn * 1000 <= abs(stamp) + 2000 ? true : nil }
        return turn * 1000 <= stamp + 2000 ? false : nil
    }
    func refresh() async {
        if pending > 0 { return }
        ensureScope()
        if !outbox.isEmpty {
            for (path, body) in outbox { write(path, body) }
            return
        }
        let expected = account, serial = mutation
        guard let raw = try? await OrbCore.shared.call("/api/control/inbox-state"), expected == account, serial == mutation else { return }
        if case let .object(values) = raw["_versions"] { revisions = values.compactMapValues { $0.doubleValue } }
        let changedAccount = scope != expected
        if changedAccount { seen = [:]; scope = expected }
        if case let .object(values) = raw {
            for (key, value) in values where key.hasPrefix("seen:") { if let n = value.doubleValue { seen[String(key.dropFirst(5))] = n } }
        }
        OrbInboxSettings.shared.bindAccount(expected)
        applying = true
        let prefs = raw["preferences"]
        if !prefs["model"].text.isEmpty {
            let p = OrbInboxSettings.shared
            p.aiSummary = prefs["aiSummary"].flag
            p.includeAutonomous = prefs["includeAutonomous"].flag
            p.model = prefs["model"].text
        }
        applying = false
        // An empty Core entry is an upgrade/migration, not an instruction to
        // discard the device's existing choices. Seed Core from those choices.
        if prefs["model"].text.isEmpty { writePreferences() }
        OrbMissionUnreadStore.shared.sharedStateChanged()
    }
}

nonisolated(unsafe) private let inboxIsoFractionalFormatter: ISO8601DateFormatter = {
    let f = ISO8601DateFormatter()
    f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
    return f
}()
nonisolated(unsafe) private let inboxIsoFallbackFormatter = ISO8601DateFormatter()

private func inboxTimestamp(_ text: String) -> Double? {
    guard !text.isEmpty else { return nil }
    return (inboxIsoFractionalFormatter.date(from: text) ?? inboxIsoFallbackFormatter.date(from: text))?.timeIntervalSince1970
}

struct OrbInboxModelPreset: Identifiable, Equatable, Sendable {
    let id: String
    let label: String
    let subtitle: String
}

@MainActor
@Observable
final class OrbInboxSettings: @unchecked Sendable {
    static let shared = OrbInboxSettings()

    static let defaultModel = "builtin/smart"
    static let modelPresets: [OrbInboxModelPreset] = [
        OrbInboxModelPreset(
            id: "builtin/smart",
            label: "Smart Router (builtin/smart)",
            subtitle: "Default router for crisp 2–3 sentence AI Overviews"
        ),
        OrbInboxModelPreset(
            id: "builtin/fast",
            label: "Fast Router (builtin/fast)",
            subtitle: "Lowest latency router"
        ),
        OrbInboxModelPreset(
            id: "builtin/reasoning",
            label: "Reasoning Router (builtin/reasoning)",
            subtitle: "Deeper technical synthesis"
        ),
    ]

    private let aiSummaryKey = "orb.inbox.aiSummary.v1"
    private let modelKey = "orb.inbox.model.v1"

    static let ownerKey = "orb.inbox.preferences.owner.v1"
    func bindAccount(_ account: String) {
        let owner = UserDefaults.standard.string(forKey: Self.ownerKey)
        guard owner != account else { return }
        let previous = OrbSharedInboxState.shared.applying
        OrbSharedInboxState.shared.applying = true
        if owner != nil { aiSummary = true; includeAutonomous = false; model = Self.defaultModel }
        UserDefaults.standard.set(account, forKey: Self.ownerKey)
        OrbSharedInboxState.shared.applying = previous
    }
    private(set) var version = 0
    var aiSummary: Bool {
        didSet {
            UserDefaults.standard.set(aiSummary, forKey: aiSummaryKey)
            version += 1
            if !OrbSharedInboxState.shared.applying { Task { @MainActor in OrbSharedInboxState.shared.writePreferences() } }
        }
    }
    var includeAutonomous: Bool = UserDefaults.standard.bool(forKey: "orb.inbox.includeAutonomous") {
        didSet { UserDefaults.standard.set(includeAutonomous, forKey: "orb.inbox.includeAutonomous"); version += 1; if !OrbSharedInboxState.shared.applying { Task { @MainActor in OrbSharedInboxState.shared.writePreferences() } } }
    }
    var model: String {
        didSet {
            let trimmed = model.trimmingCharacters(in: .whitespacesAndNewlines)
            let resolved = trimmed.isEmpty ? Self.defaultModel : trimmed
            if model != resolved {
                model = resolved
                return
            }
            UserDefaults.standard.set(resolved, forKey: modelKey)
            version += 1
            if !OrbSharedInboxState.shared.applying { Task { @MainActor in OrbSharedInboxState.shared.writePreferences() } }
        }
    }

    private init() {
        if UserDefaults.standard.object(forKey: aiSummaryKey) == nil {
            self.aiSummary = true
        } else {
            self.aiSummary = UserDefaults.standard.bool(forKey: aiSummaryKey)
        }
        let savedModel = (UserDefaults.standard.string(forKey: modelKey) ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        self.model = savedModel.isEmpty ? Self.defaultModel : savedModel
    }
}

struct OrbInboxDigest: Codable, Equatable, Sendable {
    let task: String
    let outcome: String
    let verdict: String
    let model: String
    let updatedAt: String
    var schemaVersion: Int? = nil
    var context: String? = nil
    var contextDetails: String? = nil
    var unresolved: String? = nil
    var decision: String? = nil
    var suggestions: [String]? = nil
    var sources: [OrbInboxSource]? = nil
    var sourceUpdatedAt: String? = nil
    var sourceRevision: String? = nil
}
struct OrbInboxSource: Codable, Equatable, Sendable {
    let quote: String
    var eventSequence: Int? = nil
}

@MainActor
@Observable
final class OrbInboxDigestStore {
    static let shared = OrbInboxDigestStore()

    private let diskKey = "inbox:digests:v7"
    private let maxConcurrent = 3
    private(set) var version = 0
    private var cache: [String: OrbInboxDigest] = [:]
    private var inFlight: Set<String> = []
    private var failedAt: [String: Date] = [:]
    private var activeCount = 0
    private var queue: [(priority: Int, work: () async -> Void)] = []

    private var loadedScope = ""
    private init() {}
    private func ensureScope() {
        let account = OrbInboxAccount.current
        guard loadedScope != account else { return }
        loadedScope = account
        let stored = OrbDisk.read(diskKey, as: [String: OrbInboxDigest].self, accountScope: account)
            ?? OrbDisk.read(diskKey, as: [String: OrbInboxDigest].self) ?? [:]
        let legacy = SHA256.hash(data: Data((OrbCore.shared.endpoint + ":" + (APIService.shared.authToken ?? "")).utf8)).map { String(format: "%02x", $0) }.joined() + "|"
        let current = scope + "|"
        cache = [:]
        for (key, digest) in stored {
            if key.hasPrefix(current) { cache[key] = digest }
            else if key.hasPrefix(legacy) { cache[current + String(key.dropFirst(legacy.count))] = digest }
        }
        OrbDisk.saveAsync(cache, key: diskKey, accountScope: account)
    }

    private var scope: String { SHA256.hash(data: Data(OrbInboxAccount.current.utf8)).map { String(format: "%02x", $0) }.joined() }
    private func cacheKey(missionID: String, updatedAt: String, model: String) -> String {
        "\(scope)|\(missionID)|\(updatedAt)|\(model)"
    }

    func get(row: OrbRow) -> OrbInboxDigest? {
        ensureScope()
        _ = version
        let settings = OrbInboxSettings.shared
        guard settings.aiSummary else { return nil }
        let key = cacheKey(missionID: row.id, updatedAt: row.updatedAt, model: settings.model)
        if let exact = cache[key] { return exact }
        let prefix = "\(scope)|\(row.id)|"
        return cache.filter { $0.key.hasPrefix(prefix) && $0.key.hasSuffix("|\(settings.model)") }.values.max { $0.updatedAt < $1.updatedAt }
    }

    func request(row: OrbRow, events: [StoredEvent], priority: Int = 10) {
        ensureScope()
        let settings = OrbInboxSettings.shared
        guard settings.aiSummary else { return }
        if ["active", "running", "starting", "pending", "queued", "resuming", "waiting_background"].contains(row.state) {
            return
        }
        let model = settings.model
        let key = cacheKey(missionID: row.id, updatedAt: row.updatedAt, model: model)
        if cache[key] != nil || inFlight.contains(key) { return }
        if let failDate = failedAt[key], Date().timeIntervalSince(failDate) < 45 { return }

        let endpoint = OrbCore.shared.endpoint
        let account = OrbInboxAccount.current

        inFlight.insert(key)
        queue.append((priority: priority, work: { [weak self] in
            guard let self else { return }
            defer { self.inFlight.remove(key); self.version += 1 }
            do {
                guard OrbCore.shared.endpoint == endpoint, OrbInboxAccount.current == account else { return }
                let answer = try await Self.fetchShared(missionID: row.id, model: model)
                guard OrbCore.shared.endpoint == endpoint, OrbInboxAccount.current == account else { return }
                if let digest = Self.parseDigest(answer, updatedAt: row.updatedAt, model: model) {
                    self.failedAt.removeValue(forKey: key)
                    self.cache[key] = digest
                    OrbDisk.saveAsync(self.cache, key: self.diskKey, accountScope: account)
                } else {
                    self.failedAt[key] = Date()
                }
            } catch {
                self.failedAt[key] = Date()
            }
        }))
        queue.sort { $0.priority < $1.priority }
        pumpQueue()
    }

    private func pumpQueue() {
        while activeCount < maxConcurrent, !queue.isEmpty {
            let next = queue.removeFirst()
            activeCount += 1
            Task { @MainActor in
                await next.work()
                self.activeCount -= 1
                self.pumpQueue()
            }
        }
    }

    func summaryState(row: OrbRow) -> String? {
        guard OrbInboxSettings.shared.aiSummary else { return nil }
        let key = cacheKey(missionID: row.id, updatedAt: row.updatedAt, model: OrbInboxSettings.shared.model)
        if inFlight.contains(key) { return "Generating summary…" }
        if failedAt[key] != nil { return "Summary unavailable" }
        return nil
    }

    private static func fetchShared(missionID: String, model: String) async throws -> String {
        guard let url = URL(string: "\(OrbCore.shared.endpoint)/api/control/missions/\(OrbCore.escape(missionID))/inbox-digest") else { throw URLError(.badURL) }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.timeoutInterval = 100
        request.setValue("Bearer \(APIService.shared.authToken ?? "")", forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONEncoder().encode(OrbJSON.object(["model": .string(model)]))
        let (data, response) = try await URLSession.shared.data(for: request)
        guard let response = response as? HTTPURLResponse, (200..<300).contains(response.statusCode) else { throw URLError(.badServerResponse) }
        return String(data: data, encoding: .utf8) ?? ""
    }

    private static func parseDigest(_ raw: String, updatedAt: String, model: String) -> OrbInboxDigest? {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let start = trimmed.firstIndex(of: "{"),
              let end = trimmed.lastIndex(of: "}"),
              start < end else { return nil }
        let slice = String(trimmed[start...end])
        guard let data = slice.data(using: .utf8),
              let json = try? JSONDecoder().decode(OrbJSON.self, from: data) else { return nil }
        guard json["schemaVersion"].doubleValue == 7, !json["outcome"].text.isEmpty, !json["sourceRevision"].text.isEmpty else { return nil }
        let sources = json["sources"].items.compactMap { source -> OrbInboxSource? in
            guard !source["quote"].text.isEmpty else { return nil }
            return OrbInboxSource(quote: source["quote"].text, eventSequence: source["eventSequence"].doubleValue.map { Int($0) })
        }
        guard !sources.isEmpty, let sourceTime = inboxTimestamp(json["sourceUpdatedAt"].text), sourceTime + 2 >= (inboxTimestamp(updatedAt) ?? 0) else { return nil }
        return OrbInboxDigest(task: "", outcome: json["outcome"].text, verdict: "waiting", model: json["model"].text.isEmpty ? model : json["model"].text, updatedAt: updatedAt,
            schemaVersion: 7, context: json["context"].text, contextDetails: json["contextDetails"].text, unresolved: json["unresolved"].text, decision: json["decision"].text,
            suggestions: json["suggestions"].items.map(\.text), sources: sources, sourceUpdatedAt: json["sourceUpdatedAt"].text, sourceRevision: json["sourceRevision"].text)
    }
}

struct OrbInboxSettingsView: View {
    @State private var settings = OrbInboxSettings.shared
    @State private var customModel: String = {
        let cur = OrbInboxSettings.shared.model
        let isPreset = OrbInboxSettings.modelPresets.contains(where: { $0.id == cur })
        return isPreset ? "" : cur
    }()

    var body: some View {
        List {
            Section {
                Toggle("Include autonomous agents", isOn: $settings.includeAutonomous)
                    .accessibilityIdentifier("settings.inbox.includeAutonomous")
                Toggle("AI Overview summaries", isOn: $settings.aiSummary)
                    .accessibilityIdentifier("settings.inbox.aiSummary")
            } footer: {
                Text("Summarizes your latest request and what the agent accomplished in 2–3 sentences using the configured router.")
            }

            if settings.aiSummary {
                Section("Overview Router Model") {
                    ForEach(OrbInboxSettings.modelPresets) { preset in
                        Button {
                            customModel = ""
                            settings.model = preset.id
                            OrbHaptics.selection()
                        } label: {
                            HStack {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(preset.label)
                                        .font(.subheadline.weight(.medium))
                                        .foregroundStyle(.primary)
                                    Text(preset.subtitle)
                                        .font(.caption)
                                        .foregroundStyle(OrbStyle.textSecondary)
                                }
                                Spacer()
                                if settings.model == preset.id {
                                    Image(systemName: "checkmark")
                                        .font(.caption.weight(.semibold))
                                        .foregroundStyle(Color.blue)
                                }
                            }
                        }
                        .buttonStyle(.plain)
                    }
                }

                Section("Custom Model Override") {
                    TextField("e.g. builtin/smart", text: $customModel)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .onChange(of: customModel) { _, newValue in
                            let trimmed = newValue.trimmingCharacters(in: .whitespacesAndNewlines)
                            settings.model = trimmed.isEmpty ? OrbInboxSettings.defaultModel : trimmed
                        }
                }
            }
        }
        .scrollContentBackground(.hidden)
        .background(OrbStyle.background)
        .navigationTitle("Inbox")
        .navigationBarTitleDisplayMode(.inline)
    }
}

@MainActor
@Observable
final class OrbMissionUnreadStore: @unchecked Sendable {
    static let shared = OrbMissionUnreadStore()

    private let defaultsKey = "orb.missionSeenAt.v2"
    private(set) var version = 0
    private var seenByMissionID: [String: String] = [:]
    private var manuallyUnreadIDs: Set<String> = []

    static let unreadResponseStates: Set<String> = [
        "awaiting_user", "waiting_user", "completed", "succeeded",
        "failed", "blocked", "not_feasible", "paused", "interrupted",
    ]

    private init() {
        if let stored = UserDefaults.standard.dictionary(forKey: defaultsKey) as? [String: String] {
            seenByMissionID = stored
        }
    }

    @MainActor func sharedStateChanged() { version += 1 }

    @MainActor func isUnread(row: OrbRow, hasInteraction: Bool = false) -> Bool {
        _ = version
        guard hasInteraction || Self.unreadResponseStates.contains(row.state) else {
            return false
        }
        if let shared = OrbSharedInboxState.shared.unread(row) { return shared }
        if manuallyUnreadIDs.contains(row.id) {
            return true
        }
        let firstViewed = row.raw["first_viewed_at"].text
        if !firstViewed.isEmpty {
            if row.updatedAt.isEmpty || firstViewed >= row.updatedAt {
                return false
            }
        }
        if let seen = seenByMissionID[row.id] {
            if row.updatedAt.isEmpty || seen >= row.updatedAt {
                return false
            }
        }
        return true
    }

    func markRead(_ row: OrbRow, hasInteraction: Bool = false, syncBackend: Bool = true) {
        guard hasInteraction || Self.unreadResponseStates.contains(row.state) else { return }
        markRead(id: row.id, updatedAt: row.updatedAt, syncBackend: syncBackend)
    }

    func markRead(id: String, updatedAt: String? = nil, status: String? = nil, hasInteraction: Bool = false, syncBackend: Bool = true) {
        guard !id.isEmpty else { return }
        if let status, !status.isEmpty, !hasInteraction, !Self.unreadResponseStates.contains(status) {
            return
        }
        let time = max(Date().timeIntervalSince1970, inboxTimestamp(updatedAt ?? "") ?? 0)
        let stamp = ISO8601DateFormatter().string(from: Date(timeIntervalSince1970: time))
        manuallyUnreadIDs.remove(id)
        let sharedChanged = OrbSharedInboxState.shared.writeSeen(id, stamp: time * 1000, syncBackend: syncBackend)
        if seenByMissionID[id] != stamp || sharedChanged {
            seenByMissionID[id] = stamp
            UserDefaults.standard.set(seenByMissionID, forKey: defaultsKey)
            version += 1
        }
        if syncBackend {
            Task { @MainActor in
                _ = try? await OrbCore.shared.call(
                    "/api/control/missions/\(OrbCore.escape(id))/opened",
                    method: "POST"
                )
            }
        }
    }

    func markUnread(id: String, updatedAt: String? = nil, syncBackend: Bool = true) {
        guard !id.isEmpty else { return }
        OrbSharedInboxState.shared.writeSeen(id, stamp: -max(Date().timeIntervalSince1970, inboxTimestamp(updatedAt ?? "") ?? 0) * 1000, syncBackend: syncBackend)
        version += 1
    }

    @MainActor func toggleUnread(_ row: OrbRow) {
        if isUnread(row: row) {
            markRead(row)
        } else {
            markUnread(id: row.id, updatedAt: row.updatedAt)
        }
    }

    func markAllRead(_ rows: [OrbRow], syncBackend: Bool = true) {
        guard !rows.isEmpty else { return }
        for row in rows { markRead(id: row.id, updatedAt: row.updatedAt, syncBackend: syncBackend) }
    }

}

enum OrbInboxTone: String, Equatable, Sendable {
    case amber
    case red
    case blue
    case green
    case muted

    var foreground: Color {
        switch self {
        case .amber: return OrbStyle.warning
        case .red: return OrbStyle.error
        case .blue: return Color(red: 112 / 255, green: 175 / 255, blue: 245 / 255)
        case .green: return OrbStyle.success
        case .muted: return OrbStyle.textSecondary
        }
    }

    var background: Color {
        switch self {
        case .amber: return OrbStyle.warning.opacity(0.14)
        case .red: return OrbStyle.error.opacity(0.14)
        case .blue: return Color(red: 112 / 255, green: 175 / 255, blue: 245 / 255).opacity(0.14)
        case .green: return OrbStyle.success.opacity(0.14)
        case .muted: return Color.white.opacity(0.06)
        }
    }
}

enum OrbInboxCategory: String, Equatable, Sendable {
    case needsYou
    case ready
    case working
    case hidden
}

struct OrbInboxOption: Identifiable, Equatable, Sendable {
    let id: String
    let label: String
    let isPrimary: Bool
    let payload: OrbJSON
}

struct OrbInboxInteraction: Equatable, Sendable {
    let callID: String
    let toolName: String
    let kind: String
    let prompt: String
    let commandPreview: String?
    let options: [OrbInboxOption]
}

struct OrbInboxPeekTurn: Identifiable, Equatable, Sendable {
    let id: String
    let role: String
    let text: String
    var workReceipt: String? = nil
}

struct OrbInboxChildFailure: Identifiable, Equatable {
    let id: String
    let title: String
    let row: OrbRow
}

struct OrbInboxChildSummary: Equatable {
    let total: Int
    let completed: Int
    let running: Int
    let failed: Int
    let failedChildren: [OrbInboxChildFailure]
    let hasUnreadFailure: Bool
}

struct OrbInboxItem: Identifiable, Equatable {
    let id: String
    let row: OrbRow
    let projectSlug: String
    let projectTitle: String
    let headline: String
    let summary: String
    let lastRequest: String?
    let workReceipt: String?
    var aiOverview: OrbInboxDigest? = nil
    let badge: String
    let tone: OrbInboxTone
    let category: OrbInboxCategory
    let machine: String
    let isGoal: Bool
    var unread: Bool
    var attention: Bool
    let canRetry: Bool
    let peekTurns: [OrbInboxPeekTurn]
    var childSummary: OrbInboxChildSummary?
    let updatedAt: String
    let interaction: OrbInboxInteraction?
}

enum OrbInboxModel {
    static let maxSummaryChars = 320

    private static let workingStatuses: Set<String> = [
        "active", "running", "starting", "pending", "queued", "resuming", "waiting_background",
    ]
    private static let hiddenStatuses: Set<String> = [
        "acknowledged", "archived", "deleted", "cancelled",
    ]
    private static let interactiveTools: Set<String> = [
        "ui_native_request", "AskUserQuestion", "question",
    ]

    static func isSyntheticUserMessage(_ raw: String) -> Bool {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return true }
        return trimmed.range(of: #"^\[SYSTEM:\s*AUTOMATIC[\s_]+RESUME"#, options: [.regularExpression, .caseInsensitive]) != nil ||
            trimmed.range(of: #"^\[SYSTEM:\s*BACKGROUND"#, options: [.regularExpression, .caseInsensitive]) != nil ||
            trimmed.range(of: #"^Continue from where you left off\.?$"#, options: [.regularExpression, .caseInsensitive]) != nil ||
            trimmed.range(of: #"^Continue and resolve the blocker/error\.?$"#, options: [.regularExpression, .caseInsensitive]) != nil
    }

    static func cleanChildTrackLabel(_ raw: String) -> String {
        let base = OrbStyle.displayTitle(raw).replacingOccurrences(
            of: #"\s*·\s*fork\s*$"#,
            with: "",
            options: [.regularExpression, .caseInsensitive]
        ).trimmingCharacters(in: .whitespacesAndNewlines)
        guard !base.isEmpty else { return "Worker track" }
        if base.range(of: #"^(i['’]ll|i will|let me|now i['’]ll|first,? i['’]ll)\b"#, options: [.regularExpression, .caseInsensitive]) != nil ||
            base.count > 68 {
            let clipped = clipToSentence(base, maxChars: 48)
            return clipped.isEmpty ? "Worker track" : clipped
        }
        return base
    }

    static func stripMarkdownToProse(_ raw: String) -> String {
        guard !raw.isEmpty else { return "" }
        var s = raw
        s = s.replacingOccurrences(of: #"```[\s\S]*?```"#, with: " ", options: .regularExpression)
        s = s.replacingOccurrences(of: #"(?m)^\s{0,3}#{1,6}\s+[^\n]*$"#, with: " ", options: .regularExpression)
        s = s.replacingOccurrences(of: #"(?m)^\s{0,3}>\s*"#, with: "", options: .regularExpression)
        s = s.replacingOccurrences(of: #"(?m)^\s*(?:[-*+]|\d+\.)\s+"#, with: "", options: .regularExpression)
        s = s.replacingOccurrences(of: #"\[([^\]]+)\]\([^)]+\)"#, with: "$1", options: .regularExpression)
        s = s.replacingOccurrences(of: #"`([^`]+)`"#, with: "$1", options: .regularExpression)
        s = s.replacingOccurrences(of: #"(\*\*|__)(.*?)\1"#, with: "$2", options: .regularExpression)
        s = s.replacingOccurrences(of: #"(\*|_)(.*?)\1"#, with: "$2", options: .regularExpression)
        s = s.replacingOccurrences(of: #"\s+"#, with: " ", options: .regularExpression)
        return s.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    static func clipToSentence(_ raw: String, maxChars: Int = maxSummaryChars) -> String {
        let prose = stripMarkdownToProse(raw)
        guard !prose.isEmpty else { return "" }
        if prose.count <= maxChars { return prose }

        var cutIndex: String.Index?
        for idx in prose.indices {
            let distFromStart = prose.distance(from: prose.startIndex, to: idx)
            if distFromStart >= maxChars { break }
            let ch = prose[idx]
            if ch == "." || ch == "?" || ch == "!" {
                let nextIdx = prose.index(after: idx)
                let isEnd = nextIdx == prose.endIndex || prose[nextIdx].isWhitespace
                let dist = prose.distance(from: prose.startIndex, to: nextIdx)
                if isEnd && dist >= 24 {
                    cutIndex = nextIdx
                }
            }
        }
        if let cutIndex {
            return String(prose[..<cutIndex]).trimmingCharacters(in: .whitespaces)
        }
        let prefix = String(prose.prefix(max(1, maxChars - 1)))
        if let lastSpace = prefix.lastIndex(of: " "),
           prefix.distance(from: prefix.startIndex, to: lastSpace) >= maxChars / 2 {
            return String(prefix[..<lastSpace]).trimmingCharacters(in: .whitespaces) + "…"
        }
        return prefix.trimmingCharacters(in: .whitespaces) + "…"
    }

    static func humanizeStatusText(_ raw: String) -> String {
        var trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return "" }
        if trimmed.range(of: #"^[a-z0-9_]+$"#, options: .regularExpression) != nil {
            return ""
        }
        trimmed = trimmed.replacingOccurrences(
            of: #"(?i);\s*error:\s*command exited with (?:Some\()?(-?\d+)\)?"#,
            with: "",
            options: .regularExpression
        )
        trimmed = trimmed.replacingOccurrences(
            of: #"\(exit Some\((-?\d+)\)\)"#,
            with: "(exit $1)",
            options: .regularExpression
        )
        trimmed = trimmed.replacingOccurrences(
            of: #"\bSome\((-?\d+)\)"#,
            with: "$1",
            options: .regularExpression
        )
        trimmed = trimmed.replacingOccurrences(
            of: #"finished with state 'failed'\s*"#,
            with: "failed ",
            options: [.regularExpression, .caseInsensitive]
        )
        trimmed = trimmed.replacingOccurrences(
            of: #"(?i)^Remote\s+(\S+)\s+job\s+[0-9a-f-]{36}\s+on\s+node\s+'([^']+)'\s+"#,
            with: "Remote $1 run on $2 ",
            options: .regularExpression
        )
        trimmed = trimmed.replacingOccurrences(
            of: #"(?i)^Job\s+[0-9a-f-]{36}\s+on\s+node\s+'([^']+)'\s+"#,
            with: "Remote run on $1 ",
            options: .regularExpression
        )
        return trimmed
    }

    static func extractLastRequest(row: OrbRow, events: [StoredEvent], headline: String) -> String? {
        for event in events.reversed() where event.eventType == "user_message" {
            let text = event.content.trimmingCharacters(in: .whitespacesAndNewlines)
            if !text.isEmpty && !isSyntheticUserMessage(text) {
                let clipped = clipToSentence(text, maxChars: 96)
                if clipped.caseInsensitiveCompare(headline) != .orderedSame {
                    return clipped
                }
                return nil
            }
        }
        let history = row.raw["history"].items
        if history.count > 1 {
            for entry in history.reversed() where entry["role"].text == "user" {
                let text = entry["content"].text.trimmingCharacters(in: .whitespacesAndNewlines)
                if !text.isEmpty && !isSyntheticUserMessage(text) {
                    let clipped = clipToSentence(text, maxChars: 96)
                    if clipped.caseInsensitiveCompare(headline) != .orderedSame {
                        return clipped
                    }
                    return nil
                }
            }
        }
        return nil
    }

    static func extractWorkReceipt(events: [StoredEvent]) -> String? {
        guard !events.isEmpty else { return nil }
        var commands = 0
        var edits = 0
        var reads = 0
        for ev in events where ev.eventType == "tool_call" {
            let name = (ev.toolName ?? "").lowercased()
            if ["bash", "run_command", "shell", "terminal", "exec_command"].contains(name) {
                commands += 1
            } else if name.contains("edit") || name.contains("write") || name.contains("patch") || name.contains("replace") {
                edits += 1
            } else if name.contains("read") || name.contains("view") || name.contains("grep") || name.contains("glob") {
                reads += 1
            }
        }
        var parts: [String] = []
        if commands > 0 { parts.append("\(commands) \(commands == 1 ? "command" : "commands")") }
        if edits > 0 { parts.append("Edited \(edits) \(edits == 1 ? "file" : "files")") }
        if parts.isEmpty && reads > 0 { parts.append("Read \(reads) \(reads == 1 ? "file" : "files")") }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }

    static func extractInteraction(
        row: OrbRow,
        events: [StoredEvent],
        answered: Set<String> = []
    ) -> OrbInboxInteraction? {
        if hiddenStatuses.contains(row.state) || ["completed", "succeeded", "failed", "not_feasible"].contains(row.state) {
            return nil
        }
        let resolvedCalls = Set(events.compactMap { $0.eventType == "tool_result" ? $0.toolCallId : nil })
        guard let callEvent = events.reversed().first(where: { event in
            guard event.eventType == "tool_call",
                  let name = event.toolName,
                  interactiveTools.contains(name),
                  let callID = event.toolCallId,
                  !callID.isEmpty else { return false }
            return !answered.contains(callID) && !resolvedCalls.contains(callID)
        }),
        let callID = callEvent.toolCallId,
        let data = callEvent.content.data(using: .utf8),
        let request = try? JSONDecoder().decode(OrbJSON.self, from: data) else {
            return nil
        }

        let toolName = callEvent.toolName ?? "ui_native_request"
        let method = request["method"].text.isEmpty
            ? (toolName == "AskUserQuestion" ? "claude_questions" : "question")
            : request["method"].text
        let params = request["params"] == .null ? request : request["params"]

        if method == "permission" {
            let desc = [
                params["input"]["description"].text,
                params["input"]["command"].text,
                params["input"]["file_path"].text,
                params["tool"].text,
            ].first(where: { !$0.isEmpty }) ?? "Allow this tool action?"
            let cmd = params["input"]["command"].text.isEmpty ? nil : params["input"]["command"].text
            return OrbInboxInteraction(
                callID: callID,
                toolName: toolName,
                kind: "permission",
                prompt: clipToSentence(desc),
                commandPreview: cmd,
                options: [
                    OrbInboxOption(
                        id: "1",
                        label: "Approve",
                        isPrimary: true,
                        payload: .object(["action": .string("accept")])
                    ),
                    OrbInboxOption(
                        id: "2",
                        label: "Decline",
                        isPrimary: false,
                        payload: .object(["action": .string("revise")])
                    ),
                ]
            )
        }

        if method == "plan" {
            let planText = params["plan"].text.isEmpty
                ? "Review the proposed implementation plan."
                : params["plan"].text
            return OrbInboxInteraction(
                callID: callID,
                toolName: toolName,
                kind: "plan",
                prompt: clipToSentence(planText),
                commandPreview: nil,
                options: [
                    OrbInboxOption(
                        id: "1",
                        label: "Approve plan",
                        isPrimary: true,
                        payload: .object(["action": .string("accept")])
                    ),
                    OrbInboxOption(
                        id: "2",
                        label: "Revise",
                        isPrimary: false,
                        payload: .object(["action": .string("revise")])
                    ),
                ]
            )
        }

        let questions = params["questions"].items
        let firstQ = questions.first ?? .null
        let qPrompt = firstQ["question"].text.isEmpty
            ? "Waiting for your answer."
            : firstQ["question"].text
        let canQuickPick = questions.count == 1 && !firstQ["multiSelect"].flag && !firstQ["options"].items.isEmpty
        let claudeFormat = method == "claude_questions" || toolName == "AskUserQuestion"
        let qKey = firstQ["id"].text.isEmpty ? "0" : firstQ["id"].text

        var options: [OrbInboxOption] = []
        if canQuickPick {
            for (idx, opt) in firstQ["options"].items.prefix(3).enumerated() {
                let label = opt["label"].text.trimmingCharacters(in: .whitespacesAndNewlines)
                guard !label.isEmpty else { continue }
                let mapped: [String: OrbJSON] = claudeFormat
                    ? [firstQ["question"].text: .string(label)]
                    : [qKey: .object(["answers": .array([.string(label)])])]
                options.append(
                    OrbInboxOption(
                        id: String(idx + 1),
                        label: label,
                        isPrimary: idx == 0,
                        payload: .object(["answers": .object(mapped)])
                    )
                )
            }
        }

        return OrbInboxInteraction(
            callID: callID,
            toolName: toolName,
            kind: "question",
            prompt: clipToSentence(qPrompt),
            commandPreview: nil,
            options: options
        )
    }

    static func isSubagent(row: OrbRow, includeAutonomous: Bool = false) -> Bool {
        if !includeAutonomous && (!row.raw["parent_mission_id"].text.isEmpty || !row.raw["callback_parent_mission_id"].text.isEmpty) { return true }
        let tags = row.raw["tags"].items.map(\.text)
        if tags.contains("superseded") || tags.contains(where: { $0.hasPrefix("superseded-by:") }) { return true }
        if includeAutonomous { return false }
        if row.raw["origin"].text == "hermes" || tags.contains("origin:hermes") || tags.contains("origin:hermes-assistant") { return true }
        if row.raw["tags"].items.contains(where: {
            $0.text.hasPrefix("worker-dispatch:") || $0.text == "superseded" || $0.text.hasPrefix("superseded-by:")
        }) {
            return true
        }
        let rawTitle = row.name.trimmingCharacters(in: .whitespacesAndNewlines)
        if rawTitle.range(of: #"^you are a sub-?agent\b"#, options: [.regularExpression, .caseInsensitive]) != nil {
            return true
        }
        return false
    }

    static func classify(row: OrbRow, interaction: OrbInboxInteraction?, includeAutonomous: Bool = false) -> OrbInboxCategory {
        guard row.mobile else { return .hidden }
        let status = row.state
        if hiddenStatuses.contains(status) { return .hidden }
        if isSubagent(row: row, includeAutonomous: includeAutonomous) { return .hidden }
        if interaction != nil { return .needsYou }
        if workingStatuses.contains(status) { return .working }
        if ["blocked", "failed", "not_feasible", "awaiting_user", "waiting_user"].contains(status) {
            return .needsYou
        }
        if ["completed", "succeeded", "paused", "interrupted"].contains(status) {
            return .ready
        }
        return .hidden
    }

    static func extractSummary(
        row: OrbRow,
        events: [StoredEvent],
        interaction: OrbInboxInteraction?
    ) -> String {
        if let prompt = interaction?.prompt, !prompt.isEmpty {
            return prompt
        }

        for event in events.reversed() {
            if event.eventType == "error" {
                let clean = humanizeStatusText(event.content)
                if !clean.isEmpty { return clipToSentence(clean) }
            }
            if event.eventType == "assistant_message" || event.eventType == "assistant_message_canonical" {
                let clean = humanizeStatusText(event.content)
                if !clean.isEmpty { return clipToSentence(clean) }
            }
        }

        for entry in row.raw["history"].items.reversed() where entry["role"].text == "assistant" {
            let clean = humanizeStatusText(entry["content"].text)
            if !clean.isEmpty { return clipToSentence(clean) }
        }

        let remoteErr = humanizeStatusText(row.raw["remote_job"]["error"].text)
        if !remoteErr.isEmpty { return clipToSentence(remoteErr) }

        let statusMsg = humanizeStatusText(row.raw["status_message"].text)
        if !statusMsg.isEmpty { return clipToSentence(statusMsg) }

        let termReason = humanizeStatusText(row.raw["terminal_reason"].text)
        if !termReason.isEmpty { return clipToSentence(termReason) }

        switch row.state {
        case "completed", "succeeded":
            return "Finished the task and is ready for your review."
        case "awaiting_user", "waiting_user":
            return "Finished the turn and is waiting for your follow-up."
        case "blocked":
            return "Blocked and needs your input to continue."
        case "failed", "not_feasible":
            return "Stopped with an error — open to inspect or resume."
        case "active", "running", "starting":
            return "Working in the background…"
        default:
            return "Ready for your review."
        }
    }

    static func extractPeekTurns(
        row: OrbRow,
        events: [StoredEvent],
        summaryFallback: String
    ) -> [OrbInboxPeekTurn] {
        var turns: [OrbInboxPeekTurn] = []
        var pendingTools: [StoredEvent] = []
        let clipTurn: (String) -> String = { raw in
            let prose = stripMarkdownToProse(raw)
            if prose.count <= 800 { return prose }
            return String(prose.prefix(799)).trimmingCharacters(in: .whitespaces) + "…"
        }

        if !events.isEmpty {
            for (idx, event) in events.enumerated() {
                if event.eventType == "tool_call" {
                    pendingTools.append(event)
                } else if event.eventType == "user_message" {
                    if !isSyntheticUserMessage(event.content) {
                        let text = clipTurn(event.content)
                        if !text.isEmpty {
                            turns.append(OrbInboxPeekTurn(id: "ev-\(idx)", role: "user", text: text))
                            pendingTools.removeAll()
                        }
                    }
                } else if event.eventType == "assistant_message" || event.eventType == "assistant_message_canonical" {
                    let text = clipTurn(humanizeStatusText(event.content))
                    if !text.isEmpty {
                        let receipt = extractWorkReceipt(events: pendingTools)
                        pendingTools.removeAll()
                        if let last = turns.last, last.role == "assistant" {
                            turns[turns.count - 1] = OrbInboxPeekTurn(
                                id: last.id,
                                role: "assistant",
                                text: text,
                                workReceipt: receipt ?? last.workReceipt
                            )
                        } else {
                            turns.append(OrbInboxPeekTurn(id: "ev-\(idx)", role: "assistant", text: text, workReceipt: receipt))
                        }
                    }
                } else if event.eventType == "error" {
                    let text = clipTurn(humanizeStatusText(event.content))
                    if !text.isEmpty {
                        let isProse = text.count >= 220 && text.range(of: #"^(error|failed|exception|panic):"#, options: [.regularExpression, .caseInsensitive]) == nil
                        let role = isProse ? "assistant" : "error"
                        if role == "assistant", let last = turns.last, last.role == "assistant" {
                            turns[turns.count - 1] = OrbInboxPeekTurn(id: last.id, role: "assistant", text: text, workReceipt: last.workReceipt)
                        } else {
                            turns.append(OrbInboxPeekTurn(id: "ev-\(idx)", role: role, text: text))
                        }
                    }
                }
            }
        }

        if !turns.isEmpty && !turns.contains(where: { $0.role == "user" }) {
            let promptFallback = OrbStyle.displayTitle(row.raw["title"].text)
            if !promptFallback.isEmpty {
                turns.insert(OrbInboxPeekTurn(id: "init-user", role: "user", text: promptFallback), at: 0)
            }
        }

        if turns.isEmpty {
            for (idx, entry) in row.raw["history"].items.enumerated() {
                let role = entry["role"].text == "user" ? "user" : "assistant"
                let rawContent = entry["content"].text
                if role == "user" && isSyntheticUserMessage(rawContent) { continue }
                let text = clipTurn(humanizeStatusText(rawContent))
                if !text.isEmpty {
                    turns.append(OrbInboxPeekTurn(id: "hist-\(idx)", role: role, text: text))
                }
            }
        }

        if turns.isEmpty {
            let role = ["failed", "not_feasible"].contains(row.state) ? "error" : "assistant"
            turns.append(
                OrbInboxPeekTurn(
                    id: "fallback",
                    role: role,
                    text: summaryFallback,
                    workReceipt: extractWorkReceipt(events: events)
                )
            )
        }

        return Array(turns.suffix(6))
    }

    static func resolveBadgeAndTone(
        row: OrbRow,
        summary: String,
        interaction: OrbInboxInteraction?
    ) -> (badge: String, tone: OrbInboxTone) {
        if let interaction {
            switch interaction.kind {
            case "permission": return ("Approval", .amber)
            case "plan": return ("Plan review", .amber)
            default: return ("Question", .amber)
            }
        }
        switch row.state {
        case "blocked":
            return ("Blocked", .amber)
        case "failed":
            return ("Failed", .red)
        case "not_feasible":
            return ("Not feasible", .red)
        case "awaiting_user", "waiting_user":
            return summary.trimmingCharacters(in: .whitespaces).hasSuffix("?")
                ? ("Question", .blue)
                : ("Waiting", .blue)
        case "completed", "succeeded":
            return ("Completed", .green)
        case "paused", "interrupted":
            return ("Paused", .muted)
        default:
            return ("Working", .muted)
        }
    }

    static func resolveMachine(row: OrbRow) -> String {
        if row.raw["tags"].items.contains(where: { $0.text == "placement:client" }) {
            return "This computer"
        }
        if row.backend.hasPrefix("cloud_") {
            return OrbStyle.serviceName(row.backend)
        }
        let nodeID = [
            row.raw["remote_job"]["node_id"].text,
            row.raw["remote_node_id"].text,
        ].first(where: { !$0.isEmpty }) ?? ""
        if !nodeID.isEmpty { return nodeID }
        return ""
    }

    @MainActor
    static func unreadCount(
        missions: [OrbRow],
        projects: [OrbRow]
    ) -> Int {
        var projectsBySlug: Set<String> = ["default"]
        for p in projects {
            projectsBySlug.insert(p.id)
        }
        let includeAuto = OrbInboxSettings.shared.includeAutonomous
        let unreadStore = OrbMissionUnreadStore.shared
        var childrenByParent: [String: [OrbRow]] = [:]
        var seenChildren: Set<String> = []
        for child in missions where !seenChildren.contains(child.id) {
            seenChildren.insert(child.id)
            let parentID = !child.raw["parent_mission_id"].text.isEmpty
                ? child.raw["parent_mission_id"].text
                : child.raw["callback_parent_mission_id"].text
            guard !parentID.isEmpty, !hiddenStatuses.contains(child.state) else { continue }
            childrenByParent[parentID, default: []].append(child)
        }

        var count = 0
        var seen: Set<String> = []
        for row in missions where !seen.contains(row.id) {
            seen.insert(row.id)
            let rawSlug = row.raw["project"].text.trimmingCharacters(in: .whitespacesAndNewlines)
            if !projects.isEmpty {
                let isClient = row.raw["tags"].items.contains(where: { $0.text == "placement:client" })
                if rawSlug.isEmpty && !isClient { continue }
                let slug = rawSlug.isEmpty ? "default" : rawSlug
                if !projectsBySlug.contains(slug) { continue }
            }
            let cat = classify(row: row, interaction: nil, includeAutonomous: includeAuto)
            guard cat == .needsYou || cat == .ready else { continue }
            var isRowUnread = unreadStore.isUnread(row: row)
            if !isRowUnread, let children = childrenByParent[row.id] {
                for child in children where ["failed", "not_feasible", "blocked"].contains(child.state) {
                    if unreadStore.isUnread(row: child) {
                        isRowUnread = true
                        break
                    }
                }
            }
            if isRowUnread {
                count += 1
            }
        }
        return count
    }

    @MainActor
    static func buildItem(
        row: OrbRow,
        projectsBySlug: [String: String],
        events: [StoredEvent] = [],
        answered: Set<String> = [],
        includePeekTurns: Bool = false
    ) -> OrbInboxItem? {
        let interaction = extractInteraction(row: row, events: events, answered: answered)
        let category = classify(row: row, interaction: interaction, includeAutonomous: OrbInboxSettings.shared.includeAutonomous)
        guard category != .hidden else { return nil }

        let slug = row.raw["project"].text.isEmpty ? "default" : row.raw["project"].text
        let projectTitle = projectsBySlug[slug] ?? (slug == "default" ? "Default" : slug)

        let rawTitle = OrbStyle.displayTitle(row.raw["title"].text)
        let firstUser = row.raw["history"].items.first(where: { $0["role"].text == "user" })?["content"].text ?? ""
        let headline = !rawTitle.isEmpty
            ? rawTitle
            : (!firstUser.isEmpty ? clipToSentence(firstUser, maxChars: 54) : "Untitled conversation")

        let isWorking = category == .working
        let rawSummary = isWorking ? "Working in the background…" : extractSummary(row: row, events: events, interaction: interaction)
        let digest = isWorking ? nil : OrbInboxDigestStore.shared.get(row: row)
        let summary = (interaction == nil && !(digest?.outcome.isEmpty ?? true)) ? (digest?.outcome ?? rawSummary) : rawSummary
        let lastRequest: String? = {
            if isWorking { return nil }
            if let dt = digest?.task, !dt.isEmpty {
                return dt.caseInsensitiveCompare(headline) == .orderedSame ? nil : dt
            }
            return extractLastRequest(row: row, events: events, headline: headline)
        }()
        let workReceipt = (includePeekTurns && !isWorking) ? extractWorkReceipt(events: events) : nil

        let (badge, tone) = resolveBadgeAndTone(row: row, summary: summary, interaction: interaction)
        let isGoal = row.raw["goal_mode"].flag || OrbStyle.goalObjective(row.raw["title"].text) != nil
        let unread = OrbMissionUnreadStore.shared.isUnread(row: row, hasInteraction: interaction != nil)
        let attention = interaction != nil || ["blocked", "failed", "not_feasible"].contains(row.state)
        let canRetry = ["failed", "not_feasible", "interrupted", "blocked"].contains(row.state)
        let peekTurns = includePeekTurns ? extractPeekTurns(row: row, events: events, summaryFallback: summary) : []

        return OrbInboxItem(
            id: row.id,
            row: row,
            projectSlug: slug,
            projectTitle: projectTitle,
            headline: headline,
            summary: summary,
            lastRequest: lastRequest,
            workReceipt: workReceipt,
            aiOverview: digest,
            badge: badge,
            tone: tone,
            category: category,
            machine: resolveMachine(row: row),
            isGoal: isGoal,
            unread: unread,
            attention: attention,
            canRetry: canRetry,
            peekTurns: peekTurns,
            childSummary: nil,
            updatedAt: row.updatedAt,
            interaction: interaction
        )
    }

    private static func urgencyRank(_ item: OrbInboxItem) -> Int {
        if item.interaction != nil { return 0 }
        switch item.row.state {
        case "blocked": return 1
        case "awaiting_user", "waiting_user": return 2
        case "failed", "not_feasible": return 3
        default: return 4
        }
    }

    @MainActor
    static func buildSections(
        missions: [OrbRow],
        projects: [OrbRow],
        eventsByMission: [String: [StoredEvent]] = [:],
        answeredCallIDs: Set<String> = [],
        dismissedIDs: Set<String> = [],
        peekedIDs: Set<String> = []
    ) -> (needsYou: [OrbInboxItem], ready: [OrbInboxItem], working: [OrbInboxItem]) {
        var projectsBySlug: [String: String] = ["default": "Default"]
        for p in projects {
            projectsBySlug[p.id] = p.name
        }

        var childrenByParent: [String: [OrbRow]] = [:]
        var seenChildren: Set<String> = []
        for child in missions where !seenChildren.contains(child.id) {
            seenChildren.insert(child.id)
            let parentID = !child.raw["parent_mission_id"].text.isEmpty
                ? child.raw["parent_mission_id"].text
                : child.raw["callback_parent_mission_id"].text
            guard !parentID.isEmpty, !hiddenStatuses.contains(child.state) else { continue }
            childrenByParent[parentID, default: []].append(child)
        }

        var needsYou: [OrbInboxItem] = []
        var ready: [OrbInboxItem] = []
        var working: [OrbInboxItem] = []
        var seen: Set<String> = []

        for row in missions where !seen.contains(row.id) && !dismissedIDs.contains(row.id) {
            seen.insert(row.id)
            let rawSlug = row.raw["project"].text.trimmingCharacters(in: .whitespacesAndNewlines)
            if !projects.isEmpty {
                let isClient = row.raw["tags"].items.contains(where: { $0.text == "placement:client" })
                if rawSlug.isEmpty && !isClient { continue }
                let slug = rawSlug.isEmpty ? "default" : rawSlug
                if projectsBySlug[slug] == nil { continue }
            }
            let events = eventsByMission[row.id] ?? []
            guard var item = buildItem(
                row: row,
                projectsBySlug: projectsBySlug,
                events: events,
                answered: answeredCallIDs,
                includePeekTurns: peekedIDs.contains(row.id)
            ) else { continue }

            if let children = childrenByParent[row.id], !children.isEmpty {
                var completed = 0
                var running = 0
                var failed = 0
                var failedChildren: [OrbInboxChildFailure] = []
                var hasUnreadFailure = false

                for child in children {
                    let st = child.state
                    if ["completed", "succeeded"].contains(st) {
                        completed += 1
                    } else if workingStatuses.contains(st) {
                        running += 1
                    } else if ["failed", "not_feasible", "blocked"].contains(st) {
                        failed += 1
                        let childTitle = cleanChildTrackLabel(child.raw["title"].text)
                        failedChildren.append(
                            OrbInboxChildFailure(
                                id: child.id,
                                title: childTitle,
                                row: child
                            )
                        )
                        if OrbMissionUnreadStore.shared.isUnread(row: child) {
                            hasUnreadFailure = true
                        }
                    }
                }

                item.childSummary = OrbInboxChildSummary(
                    total: children.count,
                    completed: completed,
                    running: running,
                    failed: failed,
                    failedChildren: failedChildren,
                    hasUnreadFailure: hasUnreadFailure
                )
                if hasUnreadFailure {
                    item.unread = true
                    item.attention = true
                }
            }

            switch item.category {
            case .needsYou: needsYou.append(item)
            case .ready: ready.append(item)
            case .working: working.append(item)
            case .hidden: break
            }
        }

        needsYou.sort { a, b in
            let ua = urgencyRank(a)
            let ub = urgencyRank(b)
            if ua != ub { return ua < ub }
            return a.updatedAt > b.updatedAt
        }
        ready.sort { $0.updatedAt > $1.updatedAt }
        working.sort { $0.updatedAt > $1.updatedAt }

        return (needsYou, ready, working)
    }
}

enum OrbInboxFilterMode: String, CaseIterable {
    case unread
    case attention
    case all

    var title: String {
        switch self {
        case .unread: return "Unread"
        case .attention: return "Attention"
        case .all: return "All"
        }
    }
}

struct OrbInboxView: View {
    let projects: [OrbRow]
    @Binding var actionableCount: Int
    let onOpenMission: (OrbRow) -> Void

    @State private var missions: [OrbRow] = []
    @State private var cachedSections: (needsYou: [OrbInboxItem], ready: [OrbInboxItem], working: [OrbInboxItem]) = ([], [], [])
    @State private var loading = true
    @State private var error = ""
    @State private var filterMode: OrbInboxFilterMode = .unread
    @State private var selectedProject: String?
    @State private var showWorking = false
    @State private var dismissedIDs: Set<String> = []
    @State private var busyIDs: Set<String> = []
    @State private var answeredCallIDs: Set<String> = []
    @State private var eventsByMission: [String: [StoredEvent]] = [:]
    @State private var replyingMissionID: String?
    @State private var replyDraft = ""
    @State private var peekedIDs: Set<String> = []
    @FocusState private var replyFocused: Bool
    @State private var undoItem: (id: String, title: String)?

    private let api = OrbCore.shared
    private let appearance = OrbProjectAppearance.shared
    private let unreadStore = OrbMissionUnreadStore.shared
    private let digestStore = OrbInboxDigestStore.shared
    private let inboxSettings = OrbInboxSettings.shared

    private func recomputeSections() {
        let built = OrbInboxModel.buildSections(
            missions: missions,
            projects: projects,
            eventsByMission: eventsByMission,
            answeredCallIDs: answeredCallIDs,
            dismissedIDs: dismissedIDs,
            peekedIDs: peekedIDs
        )
        cachedSections = built
        let newUnread = (built.needsYou + built.ready).filter(\.unread).count
        if actionableCount != newUnread {
            actionableCount = newUnread
        }
    }

    private func markItemAndChildrenRead(_ item: OrbInboxItem) {
        unreadStore.markRead(item.row)
        if let cs = item.childSummary {
            for child in cs.failedChildren {
                unreadStore.markRead(child.row)
            }
        }
        recomputeSections()
    }

    private func togglePeek(_ item: OrbInboxItem) {
        withAnimation(.snappy(duration: 0.2)) {
            if peekedIDs.contains(item.id) {
                peekedIDs.remove(item.id)
                recomputeSections()
            } else {
                peekedIDs.insert(item.id)
                let diskCached = OrbReadCache.readEvents(item.id)
                if !diskCached.isEmpty && eventsByMission[item.id] == nil {
                    eventsByMission[item.id] = diskCached
                }
                recomputeSections()
                if eventsByMission[item.id] == nil {
                    Task {
                        if let batch = try? await APIService.shared.getMissionEventsWithMeta(id: item.id, limit: 120, sinceSeq: nil) {
                            OrbReadCache.saveEvents(item.id, events: batch.events)
                            eventsByMission[item.id] = batch.events
                            recomputeSections()
                        }
                    }
                }
            }
        }
    }

    private var computed: (needsYou: [OrbInboxItem], ready: [OrbInboxItem], working: [OrbInboxItem]) {
        cachedSections
    }

    private func matchesFilter(_ item: OrbInboxItem, mode: OrbInboxFilterMode? = nil) -> Bool {
        switch mode ?? filterMode {
        case .unread: return item.unread
        case .attention: return item.attention
        case .all: return true
        }
    }

    private var unreadCount: Int {
        (computed.needsYou + computed.ready).filter(\.unread).count
    }

    private var attentionCount: Int {
        (computed.needsYou + computed.ready).filter(\.attention).count
    }

    private var totalActionableCount: Int {
        computed.needsYou.count + computed.ready.count
    }

    private var modeFilteredNeedsYou: [OrbInboxItem] {
        computed.needsYou.filter { matchesFilter($0) }
    }

    private var modeFilteredReady: [OrbInboxItem] {
        computed.ready.filter { matchesFilter($0) }
    }

    private var filteredNeedsYou: [OrbInboxItem] {
        guard let slug = selectedProject else { return modeFilteredNeedsYou }
        return modeFilteredNeedsYou.filter { $0.projectSlug == slug }
    }

    private var filteredReady: [OrbInboxItem] {
        guard let slug = selectedProject else { return modeFilteredReady }
        return modeFilteredReady.filter { $0.projectSlug == slug }
    }

    private var projectFilters: [(slug: String, title: String, count: Int)] {
        var counts: [String: (title: String, count: Int)] = [:]
        for item in modeFilteredNeedsYou + modeFilteredReady {
            let current = counts[item.projectSlug] ?? (item.projectTitle, 0)
            counts[item.projectSlug] = (item.projectTitle, current.count + 1)
        }
        return counts
            .map { (slug: $0.key, title: $0.value.title, count: $0.value.count) }
            .sorted { $0.count != $1.count ? $0.count > $1.count : $0.title < $1.title }
    }

    var body: some View {
        ZStack(alignment: .bottom) {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 12) {
                    modeFilterBar

                    if projectFilters.count > 1 {
                        projectFilterChips
                    }

                    if !error.isEmpty {
                        OrbNotice(message: error)
                    }

                    if showWorking && !computed.working.isEmpty {
                        workingSection
                    }

                    if loading && missions.isEmpty {
                        inboxSkeletons
                    } else if filteredNeedsYou.isEmpty && filteredReady.isEmpty {
                        emptyInboxState
                    } else {
                        if !filteredNeedsYou.isEmpty {
                            sectionHeader(title: "Needs you", count: filteredNeedsYou.count)
                            VStack(spacing: 8) {
                                ForEach(filteredNeedsYou) { item in
                                    inboxCard(item)
                                }
                            }
                        }

                        if !filteredReady.isEmpty {
                            HStack {
                                sectionHeader(title: "Ready for review", count: filteredReady.count)
                                Spacer()
                                Button {
                                    OrbHaptics.success()
                                    Task { await markAllReadyDone() }
                                } label: {
                                    Text("Mark all done")
                                        .font(.caption.weight(.medium))
                                        .foregroundStyle(OrbStyle.textSecondary)
                                }
                                .buttonStyle(.plain)
                                .accessibilityIdentifier("inbox.markAllDone")
                            }
                            VStack(spacing: 8) {
                                ForEach(filteredReady) { item in
                                    inboxCard(item)
                                }
                            }
                        }
                    }
                }
                .padding(.horizontal, 16)
                .padding(.top, 8)
                .padding(.bottom, undoItem != nil ? 76 : 24)
            }
            .background(OrbStyle.background)
            .refreshable { await load(force: true) }

            if let undo = undoItem {
                undoToast(undo)
                    .padding(.horizontal, 18)
                    .padding(.bottom, 14)
                    .transition(.move(edge: .bottom).combined(with: .opacity))
            }
        }
        .task {
            await load(force: false)
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(10))
                if !Task.isCancelled {
                    await load(force: true)
                }
            }
        }
        .onChange(of: projects) { _, _ in recomputeSections() }
        .onChange(of: unreadStore.version) { _, _ in recomputeSections() }
        .onChange(of: digestStore.version) { _, _ in recomputeSections() }
        .onChange(of: inboxSettings.version) { _, _ in recomputeSections() }
    }

    private var modeFilterBar: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 6) {
                HStack(spacing: 2) {
                    ForEach(OrbInboxFilterMode.allCases, id: \.rawValue) { mode in
                        let active = filterMode == mode
                        let count = mode == .unread ? unreadCount : (mode == .attention ? attentionCount : totalActionableCount)
                        Button {
                            withAnimation(.snappy(duration: 0.2)) {
                                filterMode = mode
                            }
                            OrbHaptics.selection()
                        } label: {
                            HStack(spacing: 4) {
                                if mode == .unread {
                                    Circle()
                                        .fill(Color.blue)
                                        .frame(width: 6, height: 6)
                                }
                                Text(mode.title)
                                    .font(.caption.weight(.medium))
                                    .foregroundStyle(active ? .primary : OrbStyle.textSecondary)
                                    .lineLimit(1)
                                    .fixedSize(horizontal: true, vertical: false)
                                Text("\(count)")
                                    .font(.caption2)
                                    .foregroundStyle(OrbStyle.textMuted)
                                    .monospacedDigit()
                                    .lineLimit(1)
                                    .fixedSize(horizontal: true, vertical: false)
                            }
                            .padding(.horizontal, 8)
                            .frame(minHeight: 44)
                            .contentShape(Rectangle())
                            .overlay(alignment: .bottom) {
                                Rectangle().fill(active ? OrbStyle.textSecondary : Color.clear).frame(height: 2)
                            }
                        }
                        .buttonStyle(.plain)
                        .accessibilityIdentifier("inbox.filter.\(mode.rawValue)")
                        .accessibilityAddTraits(active ? .isSelected : [])
                    }
                }
                .padding(3)


                if !computed.working.isEmpty {
                    let compactWorking = unreadCount > 0
                    Button {
                        withAnimation(.snappy(duration: 0.22)) {
                            showWorking.toggle()
                        }
                        OrbHaptics.selection()
                    } label: {
                        HStack(spacing: 5) {
                            OrbRunningDots(size: 11)
                            Text(compactWorking ? "\(computed.working.count)" : "\(computed.working.count) working")
                                .font(.caption.weight(.medium))
                                .foregroundStyle(.primary)
                                .monospacedDigit()
                                .lineLimit(1)
                                .fixedSize(horizontal: true, vertical: false)
                        }
                        .padding(.horizontal, 9)
                        .frame(minHeight: 44)
                        .contentShape(Rectangle())
                        .background(
                            showWorking ? OrbStyle.elevated : Color.clear,
                            in: RoundedRectangle(cornerRadius: OrbStyle.controlRadius)
                        )
                        .overlay(
                            RoundedRectangle(cornerRadius: OrbStyle.controlRadius).stroke(showWorking ? OrbStyle.borderStrong : OrbStyle.border, lineWidth: 1)
                        )
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("inbox.workingPill")
                }

                if unreadCount > 0 {
                    Button {
                        OrbHaptics.selection()
                        withAnimation(.snappy(duration: 0.2)) {
                            for item in (computed.needsYou + computed.ready).filter(\.unread) {
                                markItemAndChildrenRead(item)
                            }
                        }
                    } label: {
                        HStack(spacing: 4) {
                            Image(systemName: "checkmark")
                                .font(.system(size: 10, weight: .semibold))
                            Text("Read all")
                                .font(.caption.weight(.medium))
                                .lineLimit(1)
                                .fixedSize(horizontal: true, vertical: false)
                        }
                        .foregroundStyle(OrbStyle.textSecondary)
                        .padding(.horizontal, 9)
                        .frame(minHeight: 44)
                        .contentShape(Rectangle())

                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("inbox.markAllRead")
                }
            }
        }
    }

    private var projectFilterChips: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 6) {
                Button {
                    withAnimation(.snappy(duration: 0.2)) { selectedProject = nil }
                    OrbHaptics.selection()
                } label: {
                    Text("All projects")
                        .font(.caption.weight(.medium))
                        .foregroundStyle(selectedProject == nil ? .primary : OrbStyle.textSecondary)
                        .lineLimit(1)
                        .fixedSize(horizontal: true, vertical: false)
                        .padding(.horizontal, 11)
                        .frame(minHeight: 44)
                        .contentShape(Rectangle())
                        .background(
                            selectedProject == nil ? OrbStyle.elevated : Color.clear,
                            in: RoundedRectangle(cornerRadius: OrbStyle.controlRadius)
                        )
                        .overlay(
                            RoundedRectangle(cornerRadius: OrbStyle.controlRadius).stroke(selectedProject == nil ? OrbStyle.borderStrong : OrbStyle.border, lineWidth: 1)
                        )
                }
                .buttonStyle(.plain)

                ForEach(projectFilters, id: \.slug) { proj in
                    let active = selectedProject == proj.slug
                    Button {
                        withAnimation(.snappy(duration: 0.2)) {
                            selectedProject = active ? nil : proj.slug
                        }
                        OrbHaptics.selection()
                    } label: {
                        HStack(spacing: 6) {
                            Circle()
                                .fill(appearance.color(proj.slug) ?? OrbStyle.icon)
                                .frame(width: 6, height: 6)
                            Text(proj.title)
                                .font(.caption.weight(.medium))
                                .foregroundStyle(active ? .primary : OrbStyle.textSecondary)
                                .lineLimit(1)
                                .truncationMode(.tail)
                                .frame(maxWidth: 160)
                            Text("\(proj.count)")
                                .font(.caption2)
                                .foregroundStyle(OrbStyle.textMuted)
                                .monospacedDigit()
                                .lineLimit(1)
                                .fixedSize(horizontal: true, vertical: false)
                        }
                        .padding(.horizontal, 10)
                        .frame(minHeight: 44)
                        .contentShape(Rectangle())
                        .background(
                            active ? OrbStyle.elevated : Color.clear,
                            in: RoundedRectangle(cornerRadius: OrbStyle.controlRadius)
                        )
                        .overlay(
                            RoundedRectangle(cornerRadius: OrbStyle.controlRadius).stroke(active ? OrbStyle.borderStrong : OrbStyle.border, lineWidth: 1)
                        )
                    }
                    .buttonStyle(.plain)
                }
            }
        }
    }

    private var workingSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            sectionHeader(title: "Working in background", count: computed.working.count)
            VStack(spacing: 6) {
                ForEach(computed.working) { item in
                    Button {
                        OrbHaptics.selection()
                        onOpenMission(item.row)
                    } label: {
                        HStack(spacing: 10) {
                            OrbRunningDots(size: 11)
                            Circle()
                                .fill(appearance.color(item.projectSlug) ?? OrbStyle.icon)
                                .frame(width: 6, height: 6)
                            Text(item.projectTitle)
                                .font(.caption.weight(.medium))
                                .foregroundStyle(OrbStyle.textSecondary)
                                .lineLimit(1)
                                .truncationMode(.tail)
                            Text("·")
                                .font(.caption)
                                .foregroundStyle(OrbStyle.textMuted)
                                .lineLimit(1)
                                .fixedSize(horizontal: true, vertical: false)
                            Text(item.headline)
                                .font(.footnote.weight(.medium))
                                .foregroundStyle(.primary)
                                .lineLimit(1)
                                .truncationMode(.tail)
                                .layoutPriority(1)
                            Spacer(minLength: 4)
                            if !item.updatedAt.isEmpty {
                                Text(OrbStyle.relativeTime(item.updatedAt))
                                    .font(.caption2)
                                    .foregroundStyle(OrbStyle.textMuted)
                                    .monospacedDigit()
                                    .lineLimit(1)
                                    .fixedSize(horizontal: true, vertical: false)
                            }
                        }
                        .padding(.horizontal, 12)
                        .padding(.vertical, 10)
                        .frame(minHeight: 44)
                        .contentShape(Rectangle())
                        .overlay(alignment: .top) {
                            Rectangle().fill(OrbStyle.border).frame(height: 1)
                        }
                    }
                    .buttonStyle(OrbPressButtonStyle())
                }
            }
        }
        .transition(.opacity.combined(with: .move(edge: .top)))
    }

    private func sectionHeader(title: String, count: Int) -> some View {
        HStack(spacing: 6) {
            Text(title)
                .font(.system(size: 12, weight: .medium))
                .foregroundStyle(OrbStyle.textMuted)
                .lineLimit(1)
                .fixedSize(horizontal: true, vertical: false)
            Text("\(count)")
                .font(.system(size: 11, weight: .medium))
                .foregroundStyle(OrbStyle.textMuted)
                .monospacedDigit()
                .lineLimit(1)
                .fixedSize(horizontal: true, vertical: false)
        }
        .padding(.top, 2)
    }

    @ViewBuilder private func summarySection(_ label: String, _ value: String?) -> some View {
        if let value, !value.isEmpty {
            VStack(alignment: .leading, spacing: 4) {
                Text(label).font(.caption2.weight(.medium)).foregroundStyle(OrbStyle.textMuted)
                Text(value).font(.footnote).foregroundStyle(OrbStyle.textSecondary).textSelection(.enabled)
            }
        }
    }

    private func inboxCard(_ item: OrbInboxItem) -> some View {
        let isReplying = replyingMissionID == item.id
        let isPeeked = peekedIDs.contains(item.id)
        let isBusy = busyIDs.contains(item.id)
        let showBadge = !(item.category == .ready && item.badge == "Completed")

        return VStack(alignment: .leading, spacing: 8) {
            Button {
                OrbHaptics.selection()
                togglePeek(item)
            } label: {
                VStack(alignment: .leading, spacing: 5) {
                    HStack(spacing: 6) {
                        Circle()
                            .fill(appearance.color(item.projectSlug) ?? OrbStyle.icon)
                            .frame(width: 5, height: 5)
                        Text(item.projectTitle)
                            .font(.caption)
                            .foregroundStyle(OrbStyle.textSecondary)
                            .lineLimit(1)
                        Spacer(minLength: 8)
                        if showBadge {
                            Text(item.badge)
                                .font(.caption2)
                                .foregroundStyle(item.tone.foreground)
                        }
                        if !item.updatedAt.isEmpty {
                            Text(OrbStyle.relativeTime(item.updatedAt))
                                .font(.caption2)
                                .foregroundStyle(OrbStyle.textMuted)
                                .monospacedDigit()
                        }
                    }
                    Text(item.headline)
                        .font(.subheadline.weight(item.unread ? .medium : .regular))
                        .foregroundStyle(.primary)
                        .lineLimit(2)
                        .frame(maxWidth: .infinity, alignment: .leading)

                    // Follow-up request line ("Asked: ...") when distinct from mission headline
                    if (isPeeked || isReplying), let lastReq = item.lastRequest, !lastReq.isEmpty {
                        HStack(spacing: 5) {
                            Text("Asked:")
                                .font(.caption2.weight(.semibold))
                                .foregroundStyle(OrbStyle.textMuted)
                                .lineLimit(1)
                                .fixedSize(horizontal: true, vertical: false)
                            Text(lastReq)
                                .font(.caption)
                                .foregroundStyle(OrbStyle.textSecondary)
                                .lineLimit(1)
                                .truncationMode(.tail)
                        }
                    }

                    // Keep the collapsed summary short; the complete response is available in Peek.
                    Text(item.summary)
                        .font(.footnote)
                        .foregroundStyle(OrbStyle.textSecondary)
                        .lineLimit(isPeeked ? nil : 3)
                        .multilineTextAlignment(.leading)
                        .frame(maxWidth: .infinity, alignment: .leading)

                    if isPeeked, let receipt = item.workReceipt, !receipt.isEmpty {
                        Text(receipt)
                            .font(.system(size: 11, design: .monospaced))
                            .foregroundStyle(OrbStyle.textMuted)
                            .lineLimit(1)
                    }

                    if let cmd = item.interaction?.commandPreview, !cmd.isEmpty {
                        Text(cmd)
                            .font(.system(size: 11, design: .monospaced))
                            .foregroundStyle(OrbStyle.textSecondary)
                            .lineLimit(1)
                            .padding(.horizontal, 8)
                            .padding(.vertical, 5)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(Color.black.opacity(0.28), in: RoundedRectangle(cornerRadius: 7, style: .continuous))
                    }
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)

            if let state = digestStore.summaryState(row: item.row) {
                Text(state).font(.caption2).foregroundStyle(OrbStyle.textMuted)
            }
            if let digest = item.aiOverview {
                Text("AI summary · \(digest.model)").font(.caption2).foregroundStyle(OrbStyle.textMuted)
                let current = digest.updatedAt == item.row.updatedAt && (inboxTimestamp(digest.sourceUpdatedAt ?? "") ?? 0) + 2 >= (inboxTimestamp(item.row.updatedAt) ?? .infinity)
                if !current { Text("Summary is out of date").font(.caption2).foregroundStyle(OrbStyle.textMuted) }
                if isPeeked && current {
                    VStack(alignment: .leading, spacing: 10) {
                        summarySection("Context", digest.context)
                        summarySection("Scope", digest.contextDetails)
                        summarySection("Unresolved", digest.unresolved)
                        summarySection("To decide", digest.decision)
                        if let sources = digest.sources, !sources.isEmpty {
                            DisclosureGroup("Sources") {
                                ForEach(sources.indices, id: \.self) { index in
                                    Button { markItemAndChildrenRead(item); onOpenMission(item.row) } label: {
                                        Text(sources[index].quote).font(.caption).multilineTextAlignment(.leading).frame(maxWidth: .infinity, alignment: .leading).padding(.vertical, 8)
                                    }.buttonStyle(.plain)
                                }
                            }.font(.caption).foregroundStyle(OrbStyle.textSecondary)
                        }
                    }.padding(12).background(OrbStyle.elevated, in: RoundedRectangle(cornerRadius: OrbStyle.controlRadius))
                }
                if (isPeeked || isReplying), current, item.interaction == nil, let suggestions = digest.suggestions, !suggestions.isEmpty {
                    VStack(alignment: .leading, spacing: 6) {
                        Text("Suggested actions")
                            .font(.caption2.weight(.medium))
                            .foregroundStyle(OrbStyle.textMuted)
                        ForEach(Array(suggestions.enumerated()), id: \.offset) { idx, suggestion in
                            Button {
                                OrbHaptics.selection()
                                replyingMissionID = item.id
                                replyDraft = suggestion
                                replyFocused = true
                            } label: {
                                HStack(alignment: .top, spacing: 7) {
                                    Text("\(idx + 1)")
                                        .font(.system(size: 10.5, weight: .medium))
                                        .foregroundStyle(OrbStyle.textMuted)
                                        .frame(minWidth: 16, minHeight: 16)
                                        .padding(.horizontal, 3)
                                        .background(Color.white.opacity(0.08), in: RoundedRectangle(cornerRadius: 4, style: .continuous))
                                        .padding(.top, 1)
                                    Text(suggestion)
                                        .font(.caption)
                                        .foregroundStyle(.primary)
                                        .multilineTextAlignment(.leading)
                                        .frame(maxWidth: .infinity, alignment: .leading)
                                }
                                .padding(.horizontal, 9)
                                .padding(.vertical, 7)
                                .background(OrbStyle.elevated.opacity(0.75), in: RoundedRectangle(cornerRadius: OrbStyle.controlRadius, style: .continuous))
                                .overlay(
                                    RoundedRectangle(cornerRadius: OrbStyle.controlRadius, style: .continuous)
                                        .stroke(OrbStyle.border, lineWidth: 1)
                                )
                            }
                            .buttonStyle(.plain)
                            .disabled(isBusy)
                        }
                    }
                }
            }

            // Actionable child tracks only (failed or running)
            if let cs = item.childSummary, (!cs.failedChildren.isEmpty || cs.running > 0) {
                HStack(spacing: 6) {
                    if let firstFailed = cs.failedChildren.first {
                        Button {
                            OrbHaptics.selection()
                            markItemAndChildrenRead(item)
                            onOpenMission(firstFailed.row)
                        } label: {
                            HStack(spacing: 6) {
                                Circle()
                                    .fill(OrbStyle.error)
                                    .frame(width: 6, height: 6)
                                Text("\(cs.failed) \(cs.failed == 1 ? "track" : "tracks") failed: \(firstFailed.title)")
                                    .font(.caption2.weight(.medium))
                                    .foregroundStyle(OrbStyle.error)
                                    .lineLimit(1)
                                    .truncationMode(.tail)
                                Image(systemName: "arrow.right")
                                    .font(.system(size: 9, weight: .semibold))
                                    .foregroundStyle(OrbStyle.error)
                            }
                            .padding(.horizontal, 8)
                            .padding(.vertical, 4)
                            .background(OrbStyle.error.opacity(0.12), in: RoundedRectangle(cornerRadius: 6, style: .continuous))
                            .overlay(
                                RoundedRectangle(cornerRadius: 6, style: .continuous)
                                    .stroke(OrbStyle.error.opacity(0.28), lineWidth: 1)
                            )
                        }
                        .buttonStyle(.plain)
                    }
                    if cs.running > 0 {
                        Text("\(cs.running) \(cs.running == 1 ? "track" : "tracks") running")
                            .font(.caption2.weight(.medium))
                            .foregroundStyle(Color(red: 112 / 255, green: 175 / 255, blue: 245 / 255))
                            .lineLimit(1)
                            .fixedSize(horizontal: true, vertical: false)
                            .padding(.horizontal, 8)
                            .padding(.vertical, 3.5)
                            .background(Color(red: 112 / 255, green: 175 / 255, blue: 245 / 255).opacity(0.12), in: RoundedRectangle(cornerRadius: 6, style: .continuous))
                    }
                }
            }

            // Quick-pick interaction options in their own scrollable row so they never crowd utility buttons
            if let interaction = item.interaction, !interaction.options.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 6) {
                        ForEach(interaction.options) { opt in
                            Button {
                                OrbHaptics.light()
                                Task { await pickOption(item: item, interaction: interaction, option: opt) }
                            } label: {
                                Text(opt.label)
                                    .font(.caption.weight(opt.isPrimary ? .semibold : .medium))
                                    .foregroundStyle(opt.isPrimary ? OrbStyle.background : .primary)
                                    .lineLimit(1)
                                    .truncationMode(.tail)
                                    .frame(maxWidth: 220)
                                    .padding(.horizontal, 11)
                                    .padding(.vertical, 5.5)
                                    .background(
                                        opt.isPrimary ? Color.white : Color.white.opacity(0.06),
                                        in: Capsule()
                                    )
                                    .overlay(
                                        Capsule().stroke(opt.isPrimary ? Color.clear : OrbStyle.border, lineWidth: 1)
                                    )
                            }
                            .buttonStyle(.plain)
                            .disabled(isBusy)
                        }
                    }
                }
            }

            // Minimalist quick actions row (Retry / Peek / Reply / Done)
            HStack(spacing: 6) {
                if item.canRetry {
                    Button {
                        OrbHaptics.light()
                        Task { await retryMission(item) }
                    } label: {
                        HStack(spacing: 4) {
                            Image(systemName: "arrow.clockwise")
                                .font(.system(size: 10, weight: .semibold))
                            Text("Retry")
                                .font(.caption.weight(.medium))
                                .lineLimit(1)
                                .fixedSize(horizontal: true, vertical: false)
                        }
                        .foregroundStyle(OrbStyle.textSecondary)
                        .padding(.horizontal, 9)
                        .frame(minHeight: 44)
                        .contentShape(Rectangle())

                    }
                    .buttonStyle(.plain)
                    .disabled(isBusy)
                    .accessibilityIdentifier("inbox.retry.\(item.id)")
                }

                Button {
                    OrbHaptics.selection()
                    togglePeek(item)
                } label: {
                    Text("Peek")
                        .font(.caption.weight(.medium))
                        .foregroundStyle(isPeeked ? .primary : OrbStyle.textSecondary)
                        .lineLimit(1)
                        .fixedSize(horizontal: true, vertical: false)
                        .padding(.horizontal, 9)
                        .frame(minHeight: 44)
                        .contentShape(Rectangle())
                        .background(isPeeked ? OrbStyle.elevated : Color.clear, in: RoundedRectangle(cornerRadius: OrbStyle.controlRadius))

                }
                .buttonStyle(.plain)
                .disabled(isBusy)
                .accessibilityIdentifier("inbox.peek.\(item.id)")

                Spacer()

                Button {
                    OrbHaptics.selection()
                    withAnimation(.snappy(duration: 0.2)) {
                        if isReplying {
                            replyingMissionID = nil
                            replyDraft = ""
                            replyFocused = false
                        } else {
                            replyingMissionID = item.id
                            replyDraft = ""
                            replyFocused = true
                        }
                    }
                } label: {
                    HStack(spacing: 4) {
                        Image(systemName: "arrowshape.turn.up.left")
                            .font(.system(size: 10, weight: .semibold))
                        Text("Reply")
                            .font(.caption.weight(.medium))
                            .lineLimit(1)
                            .fixedSize(horizontal: true, vertical: false)
                    }
                    .foregroundStyle(isReplying ? .primary : OrbStyle.textSecondary)
                    .padding(.horizontal, 10)
                    .frame(minHeight: 44)
                    .contentShape(Rectangle())
                    .background(
                        isReplying ? OrbStyle.elevated : Color.clear,
                        in: RoundedRectangle(cornerRadius: OrbStyle.controlRadius)
                    )

                }
                .buttonStyle(.plain)
                .disabled(isBusy)
                .accessibilityIdentifier("inbox.reply.\(item.id)")

                Button {
                    OrbHaptics.success()
                    Task { await markDone(item) }
                } label: {
                    HStack(spacing: 4) {
                        Image(systemName: "checkmark")
                            .font(.system(size: 10, weight: .semibold))
                        Text("Done")
                            .font(.caption.weight(.medium))
                            .lineLimit(1)
                            .fixedSize(horizontal: true, vertical: false)
                    }
                    .foregroundStyle(OrbStyle.textSecondary)
                    .padding(.horizontal, 10)
                    .frame(minHeight: 44)
                    .contentShape(Rectangle())

                }
                .buttonStyle(.plain)
                .disabled(isBusy)
                .accessibilityIdentifier("inbox.done.\(item.id)")
            }

            if isPeeked {
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(item.peekTurns) { turn in
                        VStack(alignment: .leading, spacing: 4) {
                            HStack(alignment: .top, spacing: 8) {
                                Text(turn.role == "user" ? "YOU" : (turn.role == "error" ? "ERROR" : "AGENT"))
                                    .font(.system(size: 10, weight: .semibold))
                                    .foregroundStyle(
                                        turn.role == "error"
                                            ? OrbStyle.error
                                            : (turn.role == "assistant"
                                                ? Color(red: 112 / 255, green: 175 / 255, blue: 245 / 255)
                                                : OrbStyle.textSecondary)
                                    )
                                    .lineLimit(1)
                                    .fixedSize(horizontal: true, vertical: false)
                                    .frame(width: 42, alignment: .leading)
                                Text(turn.text)
                                    .font(.caption)
                                    .foregroundStyle(OrbStyle.textSecondary)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                            }
                            if let receipt = turn.workReceipt, !receipt.isEmpty {
                                Text(receipt)
                                    .font(.system(size: 10.5, design: .monospaced))
                                    .foregroundStyle(OrbStyle.textMuted)
                                    .padding(.leading, 50)
                            }
                        }
                        .padding(.horizontal, 9)
                        .padding(.vertical, 7)
                        .background(
                            turn.role == "error" ? OrbStyle.error.opacity(0.08) : Color.black.opacity(0.24),
                            in: RoundedRectangle(cornerRadius: 8, style: .continuous)
                        )
                    }
                    HStack {
                        Spacer()
                        Button {
                            OrbHaptics.selection()
                            markItemAndChildrenRead(item)
                            onOpenMission(item.row)
                        } label: {
                            Text("Open full conversation →")
                                .font(.caption2.weight(.medium))
                                .foregroundStyle(OrbStyle.textSecondary)
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(.top, 2)
                .transition(.opacity.combined(with: .move(edge: .top)))
            }

            if isReplying {
                HStack(spacing: 8) {
                    TextField(
                        item.interaction != nil ? "Reply to \(item.headline)…" : "Send follow-up…",
                        text: $replyDraft
                    )
                    .font(.footnote)
                    .focused($replyFocused)
                    .submitLabel(.send)
                    .onSubmit {
                        Task { await submitInlineReply(item) }
                    }
                    .padding(.horizontal, 11)
                    .padding(.vertical, 8)
                    .frame(minHeight: 44)
                    .background(Color.black.opacity(0.28), in: RoundedRectangle(cornerRadius: OrbStyle.controlRadius, style: .continuous))
                    .overlay(
                        RoundedRectangle(cornerRadius: OrbStyle.controlRadius, style: .continuous)
                            .stroke(OrbStyle.borderStrong, lineWidth: 1)
                    )

                    Button {
                        OrbHaptics.light()
                        Task { await submitInlineReply(item) }
                    } label: {
                        Text(isBusy ? "…" : "Send")
                            .font(.caption.weight(.semibold))
                            .lineLimit(1)
                            .fixedSize(horizontal: true, vertical: false)
                            .foregroundStyle(
                                replyDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                                    ? OrbStyle.textMuted
                                    : OrbStyle.background
                            )
                            .padding(.horizontal, 12)
                            .padding(.vertical, 8)
                            .frame(minHeight: 44)
                            .contentShape(Rectangle())
                            .background(
                                replyDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                                    ? Color.white.opacity(0.08)
                                    : Color.white,
                                in: RoundedRectangle(cornerRadius: OrbStyle.controlRadius)
                            )
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("inbox.send.\(item.id)")
                    .disabled(isBusy || replyDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
                .transition(.opacity.combined(with: .move(edge: .top)))
            }
        }
        .padding(14)
        .background(OrbStyle.surface, in: RoundedRectangle(cornerRadius: OrbStyle.panelRadius, style: .continuous))
        .overlay { RoundedRectangle(cornerRadius: OrbStyle.panelRadius, style: .continuous).stroke(OrbStyle.border, lineWidth: 1) }
        .opacity(isBusy ? 0.6 : 1.0)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("inbox.row.\(item.id)")
        .accessibilityValue(item.unread ? "Unread" : "Read")
        .accessibilityAction(named: "Mark read") { markItemAndChildrenRead(item) }
        .contextMenu {
            Button {
                markItemAndChildrenRead(item)
                onOpenMission(item.row)
            } label: {
                Label("Open conversation", systemImage: "bubble.left.and.bubble.right")
            }
            if item.canRetry {
                Button {
                    Task { await retryMission(item) }
                } label: {
                    Label("Retry / resume", systemImage: "arrow.clockwise")
                }
            }
            Button {
                unreadStore.toggleUnread(item.row)
            } label: {
                Label(item.unread ? "Mark as read" : "Mark as unread", systemImage: item.unread ? "envelope.open" : "envelope.badge")
            }
            Button {
                replyingMissionID = item.id
                replyFocused = true
            } label: {
                Label("Quick reply", systemImage: "arrowshape.turn.up.left")
            }
            Button {
                Task { await markDone(item) }
            } label: {
                Label("Mark done", systemImage: "checkmark.circle")
            }
        }
    }

    private var emptyInboxSubtitle: String {
        if filterMode == .unread && totalActionableCount > 0 {
            let noun = totalActionableCount == 1 ? "conversation is" : "conversations are"
            return "You’ve opened every recent agent response. \(totalActionableCount) earlier \(noun) in All."
        }
        if !computed.working.isEmpty {
            let noun = computed.working.count == 1 ? "agent is" : "agents are"
            return "\(computed.working.count) \(noun) working quietly in the background."
        }
        return "When an agent needs a decision or finishes a run, it will surface here."
    }

    private var emptyInboxState: some View {
        VStack(spacing: 10) {
            ZStack {
                Circle()
                    .fill(OrbStyle.success.opacity(0.12))
                    .frame(width: 44, height: 44)
                Image(systemName: "checkmark")
                    .font(.system(size: 18, weight: .semibold))
                    .foregroundStyle(OrbStyle.success)
            }
            Text(
                selectedProject != nil
                    ? "Nothing in this project"
                    : (filterMode == .unread ? "All caught up on unread responses" : "Inbox zero")
            )
            .font(.subheadline.weight(.semibold))
            .foregroundStyle(.primary)
            Text(emptyInboxSubtitle)
            .font(.footnote)
            .foregroundStyle(OrbStyle.textSecondary)
            .multilineTextAlignment(.center)
            .padding(.horizontal, 24)

            if filterMode != .all && totalActionableCount > 0 {
                Button {
                    withAnimation(.snappy(duration: 0.2)) { filterMode = .all }
                    OrbHaptics.selection()
                } label: {
                    Text("View all (\(totalActionableCount))")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.primary)
                        .padding(.horizontal, 12)
                        .padding(.vertical, 6)
                        .background(OrbStyle.elevated, in: Capsule())
                        .overlay(Capsule().stroke(OrbStyle.borderStrong, lineWidth: 1))
                }
                .buttonStyle(.plain)
            }
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 48)
        .background(OrbStyle.surface.opacity(0.6), in: RoundedRectangle(cornerRadius: 16, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 16, style: .continuous)
                .stroke(OrbStyle.border, lineWidth: 1)
        )
        .accessibilityIdentifier("inbox.empty")
    }

    private var inboxSkeletons: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(0..<4, id: \.self) { idx in
                VStack(alignment: .leading, spacing: 8) {
                    HStack(spacing: 8) {
                        Circle()
                            .fill(Color.white.opacity(0.14))
                            .frame(width: 7, height: 7)
                        Text(idx.isMultiple(of: 2) ? "Project · Refactor mission title" : "Project · Verify proof build")
                            .font(.subheadline.weight(.semibold))
                            .redacted(reason: .placeholder)
                        Spacer()
                        Text("12m")
                            .font(.caption2)
                            .redacted(reason: .placeholder)
                    }
                    Text("Finished the requested changes and verified the test suite passes cleanly.")
                        .font(.footnote)
                        .redacted(reason: .placeholder)
                }
                .padding(.horizontal, 13)
                .padding(.vertical, 12)
                .background(OrbStyle.surface, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
                .overlay(
                    RoundedRectangle(cornerRadius: 14, style: .continuous)
                        .stroke(OrbStyle.border, lineWidth: 1)
                )
            }
        }
        .orbShimmer(active: true)
        .allowsHitTesting(false)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Loading inbox")
        .accessibilityIdentifier("inbox.loading")
    }

    private func undoToast(_ undo: (id: String, title: String)) -> some View {
        HStack(spacing: 12) {
            Text("Marked “\(undo.title)” as done")
                .font(.footnote)
                .foregroundStyle(.primary)
                .lineLimit(1)
            Spacer()
            Button {
                OrbHaptics.light()
                Task { await undoLastDone() }
            } label: {
                Text("Undo")
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(OrbStyle.background)
                    .lineLimit(1)
                    .fixedSize(horizontal: true, vertical: false)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 5)
                    .background(Color.white, in: Capsule())
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("inbox.undo")
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .background(OrbStyle.elevated, in: Capsule())
        .overlay(Capsule().stroke(OrbStyle.borderStrong, lineWidth: 1))
        .shadow(color: .black.opacity(0.35), radius: 12, y: 4)
    }

    private func load(force: Bool) async {
        defer { loading = false }
        if missions.isEmpty, let cached = OrbDisk.read("inbox:missions", as: OrbJSON.self) {
            OrbReadCache.seedFromGlobalMissions(cached.items)
            let rows = cached.items.map { OrbRow($0) }.filter(\.mobile)
            missions = rows
            seedCachedEvents(for: rows)
            recomputeSections()
        }
        // Cached rows paint immediately even when shared Core state is offline.
        await OrbSharedInboxState.shared.refresh()
        do {
            let raw = try await api.call("/api/control/missions?limit=100&all=true")
            OrbReadCache.seedFromGlobalMissions(raw.items)
            let rows = raw.items.map { OrbRow($0) }.filter(\.mobile)
            missions = rows
            OrbDisk.saveAsync(raw, key: "inbox:missions")
            seedCachedEvents(for: rows)
            recomputeSections()
            error = ""
            await prefetchActiveEvents(for: rows)
        } catch {
            if missions.isEmpty {
                self.error = error.localizedDescription
            }
        }
    }

    private func seedCachedEvents(for rows: [OrbRow]) {
        var next = eventsByMission
        var changed = false
        for row in rows {
            if next[row.id] != nil { continue }
            let mem = OrbReadCache.readMemoryEvents(row.id)
            if !mem.isEmpty {
                next[row.id] = mem
                changed = true
            }
        }
        if changed {
            eventsByMission = next
        }
    }

    private func prefetchActiveEvents(for rows: [OrbRow]) async {
        let candidates = rows.filter {
            [
                "awaiting_user", "waiting_user", "blocked", "failed", "not_feasible",
                "completed", "succeeded", "paused", "interrupted",
            ].contains($0.state) && !OrbInboxModel.isSubagent(row: $0, includeAutonomous: OrbInboxSettings.shared.includeAutonomous)
        }
        .sorted { $0.updatedAt > $1.updatedAt }
        .prefix(16)

        for (idx, row) in candidates.enumerated() {
            guard !Task.isCancelled else { return }
            var events = eventsByMission[row.id] ?? []
            if events.isEmpty {
                let diskCached = OrbReadCache.readEvents(row.id)
                if !diskCached.isEmpty {
                    eventsByMission[row.id] = diskCached
                    events = diskCached
                } else if idx < 10 {
                    if let batch = try? await APIService.shared.getMissionEventsWithMeta(id: row.id, limit: 120, sinceSeq: nil) {
                        OrbReadCache.saveEvents(row.id, events: batch.events)
                        eventsByMission[row.id] = batch.events
                        events = batch.events
                    }
                }
            }
            let priority = unreadStore.isUnread(row: row) ? idx : idx + 20
            digestStore.request(row: row, events: events, priority: priority)
        }
        recomputeSections()
    }

    private func markDone(_ item: OrbInboxItem) async {
        guard !busyIDs.contains(item.id) else { return }
        busyIDs.insert(item.id)
        defer { busyIDs.remove(item.id) }
        withAnimation(.snappy(duration: 0.22)) {
            _ = dismissedIDs.insert(item.id)
            if replyingMissionID == item.id {
                replyingMissionID = nil
                replyDraft = ""
            }
            undoItem = (id: item.id, title: item.headline)
            markItemAndChildrenRead(item)
        }
        do {
            _ = try await api.call(
                "/api/control/missions/\(OrbCore.escape(item.id))/status",
                method: "POST",
                body: .object(["status": .string("acknowledged")])
            )
            OrbReadCache.invalidate("project:\(item.projectSlug)")
        } catch {
            withAnimation(.snappy(duration: 0.2)) {
                dismissedIDs.remove(item.id)
                undoItem = nil
                recomputeSections()
            }
            self.error = error.localizedDescription
        }
    }

    private func markAllReadyDone() async {
        let items = filteredReady
        guard !items.isEmpty else { return }
        let ids = items.map(\.id)
        withAnimation(.snappy(duration: 0.22)) {
            for id in ids { dismissedIDs.insert(id) }
            for item in items { markItemAndChildrenRead(item) }
            recomputeSections()
        }
        do {
            for item in items {
                _ = try await api.call(
                    "/api/control/missions/\(OrbCore.escape(item.id))/status",
                    method: "POST",
                    body: .object(["status": .string("acknowledged")])
                )
            }
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func undoLastDone() async {
        guard let last = undoItem else { return }
        withAnimation(.snappy(duration: 0.22)) {
            undoItem = nil
            dismissedIDs.remove(last.id)
            unreadStore.markUnread(id: last.id, updatedAt: missions.first(where: { $0.id == last.id })?.updatedAt)
            recomputeSections()
        }
        do {
            _ = try await api.call(
                "/api/control/missions/\(OrbCore.escape(last.id))/status",
                method: "POST",
                body: .object(["status": .string("paused")])
            )
            await load(force: true)
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func pickOption(
        item: OrbInboxItem,
        interaction: OrbInboxInteraction,
        option: OrbInboxOption
    ) async {
        guard !busyIDs.contains(item.id) else { return }
        busyIDs.insert(item.id)
        defer { busyIDs.remove(item.id) }
        do {
            let result = try await api.call(
                "/api/control/tool_result",
                method: "POST",
                body: .object([
                    "tool_call_id": .string(interaction.callID),
                    "name": .string(interaction.toolName),
                    "result": option.payload,
                ])
            )
            guard result["delivered"].flag else {
                throw OrbHTTPError(status: 409, detail: "This request has expired. Open the conversation to refresh.")
            }
            withAnimation(.snappy(duration: 0.22)) {
                _ = answeredCallIDs.insert(interaction.callID)
                markItemAndChildrenRead(item)
            }
            await load(force: true)
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func retryMission(_ item: OrbInboxItem) async {
        guard !busyIDs.contains(item.id) else { return }
        busyIDs.insert(item.id)
        defer { busyIDs.remove(item.id) }
        do {
            var body: [String: OrbJSON] = [
                "mission_id": .string(item.id),
                "content": .string("Continue from where you left off."),
                "queue_followup": .bool(true),
                "client_message_id": .string(UUID().uuidString.lowercased()),
            ]
            if let identity = OrbContinuation.identity(for: item.row.raw) {
                body["continue_identity"] = identity
            }
            _ = try await api.call("/api/control/message", method: "POST", body: .object(body))
            OrbHaptics.success()
            withAnimation(.snappy(duration: 0.22)) {
                _ = dismissedIDs.insert(item.id)
                markItemAndChildrenRead(item)
            }
            await load(force: true)
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func submitInlineReply(_ item: OrbInboxItem) async {
        let text = replyDraft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, !busyIDs.contains(item.id) else { return }
        busyIDs.insert(item.id)
        defer { busyIDs.remove(item.id) }
        do {
            var body: [String: OrbJSON] = [
                "mission_id": .string(item.id),
                "content": .string(text),
                "queue_followup": .bool(true),
                "client_message_id": .string(UUID().uuidString.lowercased()),
            ]
            if let identity = OrbContinuation.identity(for: item.row.raw) {
                body["continue_identity"] = identity
            }
            _ = try await api.call("/api/control/message", method: "POST", body: .object(body))
            OrbHaptics.success()
            withAnimation(.snappy(duration: 0.22)) {
                replyingMissionID = nil
                replyDraft = ""
                replyFocused = false
                _ = dismissedIDs.insert(item.id)
                markItemAndChildrenRead(item)
            }
            await load(force: true)
        } catch {
            self.error = error.localizedDescription
        }
    }
}
