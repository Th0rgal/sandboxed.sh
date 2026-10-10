import Foundation
import Security
import CryptoKit

/// Lossless wire values for provider-specific capabilities and model parameters.
indirect enum OrbJSON: Codable, Sendable, Equatable {
    case object([String: OrbJSON]), array([OrbJSON]), string(String), number(Double), bool(Bool), null
    init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() { self = .null }
        else if let v = try? c.decode(Bool.self) { self = .bool(v) }
        else if let v = try? c.decode(Double.self) { self = .number(v) }
        else if let v = try? c.decode(String.self) { self = .string(v) }
        else if let v = try? c.decode([OrbJSON].self) { self = .array(v) }
        else { self = .object(try c.decode([String: OrbJSON].self)) }
    }
    func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .object(let v): try c.encode(v)
        case .array(let v): try c.encode(v)
        case .string(let v): try c.encode(v)
        case .number(let v): try c.encode(v)
        case .bool(let v): try c.encode(v)
        case .null: try c.encodeNil()
        }
    }
    subscript(_ key: String) -> OrbJSON { if case .object(let v) = self { return v[key] ?? .null }; return .null }
    var text: String { if case .string(let v) = self { return v }; return "" }
    var items: [OrbJSON] { if case .array(let v) = self { return v }; return [] }
    var flag: Bool { if case .bool(let v) = self { return v }; return false }
    var doubleValue: Double? { if case .number(let v) = self { return v }; return nil }
}

/// Remote placement can be reported by either the mission or its current job.
enum OrbContinuation {
    static func identity(for mission: OrbJSON) -> OrbJSON? {
        guard !mission["track"].text.isEmpty,
              mission["remote_node_id"].text.isEmpty,
              mission["remote_job"] == .null,
              !mission["execution"]["scope_unit"].text.hasPrefix("remote-node:") else { return nil }
        return .object(["project": mission["project"], "track": mission["track"], "github_pr": mission["github_pr"]])
    }
}

struct OrbRow: Identifiable, Hashable, Sendable {
    let id: String
    let name: String
    let state: String
    let folder: String
    let backend: String
    let cloud: Bool
    let updatedAt: String
    let raw: OrbJSON
    static func == (lhs: Self, rhs: Self) -> Bool { lhs.id == rhs.id && lhs.raw == rhs.raw }
    func hash(into h: inout Hasher) { h.combine(id) }
    init(_ value: OrbJSON, project: Bool = false) {
        raw = value
        id = value[project ? "slug" : "id"].text
        name = value["title"].text.isEmpty ? id : value["title"].text
        state = value["status"].text
        folder = value["tags"].items.map(\.text).first(where: { $0.hasPrefix("orb-folder:") }).map { String($0.dropFirst(11)) } ?? ""
        backend = value["backend"].text
        cloud = value["backend"].text.hasPrefix("cloud_") || value["execution_kind"].text == "cloud" || value["tags"].items.contains(where: { $0.text.hasPrefix("cloud:") }) || value["cloud"] != .null
        updatedAt = value["updated_at"].text.isEmpty ? value["created_at"].text : value["updated_at"].text
    }
    var parentMissionID: String? {
        let p = raw["parent_mission_id"].text
        if !p.isEmpty { return p }
        let cp = raw["callback_parent_mission_id"].text
        return cp.isEmpty ? nil : cp
    }
    var mobile: Bool { !raw["tags"].items.contains(where: { $0.text.hasPrefix("btw-parent:") }) }
    var active: Bool { ["active", "pending", "running", "starting"].contains(state) }
}

struct OrbNestedMission: Identifiable, Hashable {
    let mission: OrbRow
    var children: [OrbNestedMission]
    var id: String { mission.id }
    init(mission: OrbRow, children: [OrbNestedMission] = []) {
        self.mission = mission
        self.children = children
    }
}

enum OrbMissionTree {
    /// Keep archived/completed parents as context for visible child workers without restoring all archived missions.
    static func treeRows(_ missions: [OrbRow], visible: (OrbRow) -> Bool) -> [OrbRow] {
        var byID: [String: OrbRow] = [:]
        for m in missions where byID[m.id] == nil { byID[m.id] = m }
        var retained = Set(missions.filter(visible).map(\.id))
        for mission in missions where visible(mission) {
            var seen: Set<String> = [mission.id]
            var parent = mission.parentMissionID
            while let p = parent, !seen.contains(p) {
                seen.insert(p)
                guard let row = byID[p] else { break }
                retained.insert(p)
                parent = row.parentMissionID
            }
        }
        return missions.filter { retained.contains($0.id) }
    }

    /// Roots keep the order of the list, and so do the children of each mission.
    static func nest(_ missions: [OrbRow]) -> [OrbNestedMission] {
        var byID: [String: OrbRow] = [:]
        for mission in missions where byID[mission.id] == nil {
            byID[mission.id] = mission
        }
        var childrenByParent: [String: [OrbRow]] = [:]
        var roots: [OrbRow] = []
        for mission in missions {
            if let parentID = mission.parentMissionID,
               let parent = byID[parentID],
               parent.id != mission.id,
               !reaches(byID: byID, from: parent, target: mission.id) {
                childrenByParent[parentID, default: []].append(mission)
            } else {
                roots.append(mission)
            }
        }
        func build(_ row: OrbRow, visited: Set<String>) -> OrbNestedMission {
            var nextVisited = visited
            nextVisited.insert(row.id)
            let kids = (childrenByParent[row.id] ?? []).filter { !nextVisited.contains($0.id) }.map { build($0, visited: nextVisited) }
            return OrbNestedMission(mission: row, children: kids)
        }
        return roots.map { build($0, visited: []) }
    }

    private static func reaches(byID: [String: OrbRow], from: OrbRow, target: String) -> Bool {
        var seen: Set<String> = []
        var current: OrbRow? = from
        while let cur = current, !seen.contains(cur.id) {
            if cur.id == target { return true }
            seen.insert(cur.id)
            if let p = cur.parentMissionID {
                current = byID[p]
            } else {
                current = nil
            }
        }
        return false
    }

    static func countNested(_ node: OrbNestedMission, matches: (OrbRow) -> Bool = { _ in true }) -> Int {
        node.children.reduce(0) { sum, child in
            sum + (matches(child.mission) ? 1 : 0) + countNested(child, matches: matches)
        }
    }
}

struct OrbHTTPError: LocalizedError {
    let status: Int
    let detail: String
    var errorDescription: String? { "\(status): \(detail)" }
}

enum OrbKeychain {
    struct Credentials: Codable { let password: String; let username: String? }
    static func credentials(for endpoint: String) -> Credentials? {
        guard let text = token(for: endpoint, service: "md.thomas.orb.credentials"), let data = text.data(using: .utf8) else { return nil }
        return try? JSONDecoder().decode(Credentials.self, from: data)
    }
    @discardableResult static func saveCredentials(_ value: Credentials?, for endpoint: String) -> Bool {
        guard let value else { return save(nil, for: endpoint, service: "md.thomas.orb.credentials") }
        guard let data = try? JSONEncoder().encode(value), let text = String(data: data, encoding: .utf8) else { return false }
        return save(text, for: endpoint, service: "md.thomas.orb.credentials")
    }
    static func token(for endpoint: String, service: String = "md.thomas.orb.core") -> String? {
        var query = base(endpoint, service: service)
        query[kSecReturnData] = true
        query[kSecMatchLimit] = kSecMatchLimitOne
        var result: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess, let data = result as? Data else { return nil }
        return String(data: data, encoding: .utf8)
    }
    @discardableResult static func save(_ value: String?, for endpoint: String, service: String = "md.thomas.orb.core") -> Bool {
        let query = base(endpoint, service: service)
        guard let value else { let code = SecItemDelete(query as CFDictionary); return code == errSecSuccess || code == errSecItemNotFound }
        let attributes: [CFString: Any] = [kSecValueData: Data(value.utf8)]
        let result = SecItemUpdate(query as CFDictionary, attributes as CFDictionary)
        if result == errSecSuccess { return true }
        guard result == errSecItemNotFound else { return false }
        var insert = query
        insert[kSecValueData] = Data(value.utf8)
        insert[kSecAttrAccessible] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        return SecItemAdd(insert as CFDictionary, nil) == errSecSuccess
    }
    private static func base(_ endpoint: String, service: String) -> [CFString: Any] {
        [kSecClass: kSecClassGenericPassword, kSecAttrService: service, kSecAttrAccount: endpoint]
    }
}

@MainActor
final class OrbCore {
    static let shared = OrbCore()
    var endpoint: String { APIService.shared.baseURL.trimmingCharacters(in: CharacterSet(charactersIn: "/")) }
    func call(_ path: String, method: String = "GET", body: OrbJSON? = nil) async throws -> OrbJSON {
        guard let url = URL(string: endpoint + path) else { throw URLError(.badURL) }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.timeoutInterval = 45
        request.setValue("Bearer \(APIService.shared.authToken ?? "")", forHTTPHeaderField: "Authorization")
        if let body { request.httpBody = try JSONEncoder().encode(body); request.setValue("application/json", forHTTPHeaderField: "Content-Type") }
        let (data, response) = try await URLSession.shared.data(for: request)
        guard let http = response as? HTTPURLResponse else { throw URLError(.badServerResponse) }
        if http.statusCode == 401 {
            let api = APIService.shared
            if url.absoluteString.hasPrefix(endpoint + "/"),
               request.value(forHTTPHeaderField: "Authorization") == "Bearer \(api.authToken ?? "")" {
                api.markSessionExpired()
            }
            throw APIError.unauthorized
        }
        guard (200..<300).contains(http.statusCode) else { throw OrbHTTPError(status: http.statusCode, detail: String(data: data, encoding: .utf8) ?? "Request failed") }
        return data.isEmpty ? .null : try JSONDecoder().decode(OrbJSON.self, from: data)
    }
    static func escape(_ value: String) -> String { value.addingPercentEncoding(withAllowedCharacters: .alphanumerics) ?? "" }
    func missions(_ project: String, includeArchived: Bool = false) async throws -> [OrbRow] {
        var rows: [OrbRow] = [], offset = 0
        while true {
            let page = try await call("/api/control/missions?project=\(Self.escape(project))&all=true&limit=100&offset=\(offset)").items
            for raw in page {
                let mid = raw["id"].text
                if !mid.isEmpty {
                    OrbReadCache.seedMemory("mission:\(mid)", value: raw)
                }
            }
            let fresh = page.map { OrbRow($0) }.filter { row in !rows.contains(where: { $0.id == row.id }) }
            rows += fresh
            if !includeArchived || page.count < 100 || fresh.isEmpty { break }
            offset += page.count
        }
        return rows.filter(\.mobile)
    }
}

/// Persist the exact request before submitting. An uncertain response never creates a new identity.
struct OrbPending: Codable, Sendable {
    let path: String
    let body: OrbJSON
}

private actor OrbDiskWriter {
    static let shared = OrbDiskWriter()
    func write(_ data: Data, to url: URL) throws {
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try data.write(to: url, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
    }
    func read(from url: URL) -> Data? {
        try? Data(contentsOf: url)
    }
}

@MainActor
enum OrbDisk {
    private static var urlCache: [String: URL] = [:]
    static func url(_ key: String, accountScope: String? = nil) -> URL {
        let scope = (accountScope ?? (OrbCore.shared.endpoint + ":" + (APIService.shared.authToken ?? ""))) + ":" + key
        if let cached = urlCache[scope] { return cached }
        let root = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0].appendingPathComponent("Orb")
        try? FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let name = SHA256.hash(data: Data(scope.utf8)).map { String(format: "%02x", $0) }.joined()
        let result = root.appendingPathComponent(name + ".json")
        if urlCache.count > 1024 { urlCache.removeAll(keepingCapacity: true) }
        urlCache[scope] = result
        return result
    }
    static func read<T: Decodable>(_ key: String, as type: T.Type, accountScope: String? = nil) -> T? {
        guard let data = try? Data(contentsOf: url(key, accountScope: accountScope)) else { return nil }
        return try? JSONDecoder().decode(type, from: data)
    }
    static func readAsync<T: Decodable & Sendable>(_ key: String, as type: T.Type) async -> T? {
        let target = url(key)
        guard let data = await OrbDiskWriter.shared.read(from: target) else { return nil }
        return await Task.detached(priority: .utility) {
            try? JSONDecoder().decode(type, from: data)
        }.value
    }
    static func save<T: Encodable>(_ value: T, key: String) throws {
        try JSONEncoder().encode(value).write(to: url(key), options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
    }
    static func saveAsync<T: Encodable & Sendable>(_ value: T, key: String, accountScope: String? = nil) {
        let target = url(key, accountScope: accountScope)
        Task.detached(priority: .utility) {
            guard let data = try? JSONEncoder().encode(value) else { return }
            try? await OrbDiskWriter.shared.write(data, to: target)
        }
    }
    static func remove(_ key: String) { try? FileManager.default.removeItem(at: url(key)) }
}

/// Account-scoped, bounded read-through cache. In-flight loads are shared with navigation.
@MainActor
enum OrbReadCache {
    private struct Entry { let value: OrbJSON; let date: Date }
    private static var values: [String: Entry] = [:]
    private static var eventValues: [String: [StoredEvent]] = [:]
    private struct Pending { let id = UUID(); let task: Task<OrbJSON, Error> }
    private static var pending: [String: Pending] = [:]
    private static func key(_ name: String) -> String { OrbDisk.url(name).absoluteString }
    static func read(_ name: String) -> OrbJSON? {
        let scope = key(name)
        if let cached = values[scope]?.value { return cached }
        if let disk = OrbDisk.read(name, as: OrbJSON.self) {
            values[scope] = Entry(value: disk, date: .distantPast)
            return disk
        }
        return nil
    }
    static func seedMemory(_ name: String, value: OrbJSON) {
        let scope = key(name)
        if values.count >= 256, let oldest = values.min(by: { $0.value.date < $1.value.date })?.key { values[oldest] = nil }
        values[scope] = Entry(value: value, date: Date())
    }
    static func seed(_ name: String, value: OrbJSON) {
        seedMemory(name, value: value)
        OrbDisk.saveAsync(value, key: name)
    }
    static func seedFromGlobalMissions(_ rawMissions: [OrbJSON]) {
        var byProject: [String: [OrbJSON]] = [:]
        for item in rawMissions {
            let mid = item["id"].text
            if !mid.isEmpty {
                seedMemory("mission:\(mid)", value: item)
            }
            let proj = item["project"].text
            if !proj.isEmpty {
                byProject[proj, default: []].append(item)
            }
        }
        for (proj, list) in byProject {
            let cacheName = "project:\(proj)"
            let scope = key(cacheName)
            if values[scope] == nil {
                let existingManifest = read(cacheName)?["manifest"] ?? .null
                values[scope] = Entry(
                    value: .object(["missions": .array(list), "manifest": existingManifest]),
                    date: Date().addingTimeInterval(-25) // allow quick display while still refreshing
                )
            }
        }
    }
    static func readMemoryEvents(_ missionID: String) -> [StoredEvent] {
        let cacheName = "events:\(missionID)"
        let scope = key(cacheName)
        return eventValues[scope] ?? []
    }
    static func readEvents(_ missionID: String) -> [StoredEvent] {
        let cacheName = "events:\(missionID)"
        let scope = key(cacheName)
        if let cached = eventValues[scope] { return cached }
        if let disk = OrbDisk.read(cacheName, as: [StoredEvent].self) {
            eventValues[scope] = disk
            return disk
        }
        return []
    }
    static func saveEvents(_ missionID: String, events: [StoredEvent]) {
        let cacheName = "events:\(missionID)"
        let scope = key(cacheName)
        if eventValues.count >= 48, let firstKey = eventValues.keys.first {
            eventValues.removeValue(forKey: firstKey)
        }
        let capped = events.count > 250 ? Array(events.suffix(250)) : events
        eventValues[scope] = capped
        OrbDisk.saveAsync(capped, key: cacheName)
    }
    static func load(_ name: String, ttl: TimeInterval = 30, force: Bool = false, fetch: @escaping @MainActor () async throws -> OrbJSON) async throws -> OrbJSON {
        let scope = key(name)
        if !force, let entry = values[scope], Date().timeIntervalSince(entry.date) < ttl { return entry.value }
        if let item = pending[scope] {
            let value = try await item.task.value
            guard scope == key(name), !item.task.isCancelled else { throw CancellationError() }
            return value
        }
        let task = Task { try await fetch() }
        let item = Pending(task: task)
        pending[scope] = item
        defer { if pending[scope]?.id == item.id { pending[scope] = nil } }
        let value = try await task.value
        guard scope == key(name), pending[scope]?.id == item.id else { throw CancellationError() }
        if values.count >= 256, let oldest = values.min(by: { $0.value.date < $1.value.date })?.key { values[oldest] = nil }
        values[scope] = Entry(value: value, date: Date())
        OrbDisk.saveAsync(value, key: name)
        return value
    }
    static func invalidate(_ name: String) {
        let scope = key(name)
        pending[scope]?.task.cancel()
        pending[scope] = nil
        if let old = values[scope] { values[scope] = Entry(value: old.value, date: .distantPast) }
    }
    static func project(_ id: String, includeArchived: Bool = false, force: Bool = false) async throws -> OrbJSON {
        let cacheKey = includeArchived ? "project:\(id):all" : "project:\(id)"
        return try await load(cacheKey, force: force) {
            async let missions = OrbCore.shared.missions(id, includeArchived: includeArchived)
            async let manifest = OrbCore.shared.call("/api/projects/\(OrbCore.escape(id))/context/manifest")
            return try await .object(["missions": .array(missions.map(\.raw)), "manifest": manifest])
        }
    }
    static func conversation(_ id: String, force: Bool = false) async throws -> OrbJSON {
        try await load("mission:\(id)", force: force) {
            try await OrbCore.shared.call("/api/control/missions/\(OrbCore.escape(id))")
        }
    }
    static func cloud(_ id: String, force: Bool = false) async throws -> OrbJSON {
        try await load("cloud:\(id)", force: force) {
            try await OrbCore.shared.call("/api/control/missions/\(OrbCore.escape(id))/cloud")
        }
    }
    static func agentCatalog(force: Bool = false) async throws -> OrbJSON {
        try await load("catalog:agent", ttl: 120, force: force) {
            async let backends = OrbCore.shared.call("/api/backends")
            async let nodes = OrbCore.shared.call("/api/remote-nodes")
            async let models = OrbCore.shared.call("/api/providers/backend-models")
            return try await .object(["backends": backends, "nodes": nodes, "models": models])
        }
    }
    static func cloudAccounts(force: Bool = false) async throws -> OrbJSON {
        try await load("catalog:cloud:accounts", ttl: 90, force: force) {
            try await OrbCore.shared.call("/api/cloud/accounts")
        }
    }
    static func cloudOptions(_ endpoint: String, force: Bool = false) async throws -> OrbJSON {
        try await load("catalog:cloud:\(endpoint)", ttl: 120, force: force) {
            try await OrbCore.shared.call("/api/cloud/\(endpoint)/options")
        }
    }
    static func prefetch(_ rows: [OrbRow]) async {
        let tasks: [Task<Void, Never>] = rows.prefix(5).map { row in
            Task { @MainActor in
                guard !Task.isCancelled else { return }
                if row.raw != .null {
                    seedMemory("mission:\(row.id)", value: row.raw)
                } else {
                    _ = try? await conversation(row.id)
                }
                if row.cloud {
                    _ = try? await cloud(row.id)
                } else if readEvents(row.id).isEmpty {
                    if let batch = try? await APIService.shared.getMissionEventsWithMeta(id: row.id, limit: 150, sinceSeq: nil) {
                        saveEvents(row.id, events: batch.events)
                    }
                }
            }
        }
        for t in tasks {
            await t.value
        }
    }
}
