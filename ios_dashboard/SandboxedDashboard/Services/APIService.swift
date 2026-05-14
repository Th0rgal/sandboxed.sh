//
//  APIService.swift
//  SandboxedDashboard
//
//  HTTP API client for the sandboxed.sh backend
//

import Foundation
import Observation

@MainActor
@Observable
final class APIService {
    static let shared = APIService()
    nonisolated init() {}

    /// Per-request timeout for ordinary JSON calls. Anything that hasn't started
    /// returning data within this window is treated as failed. Default
    /// `URLSession.shared` ships with 60s, which is far too long for a chat
    /// app's cold-start path — the user sits at "Connecting…" with no feedback.
    nonisolated static let requestTimeout: TimeInterval = 15

    /// Full-transfer cap for ordinary JSON calls. Default is 7 days; a stalled
    /// large download (e.g. a multi-MB event tail over a flaky cellular link)
    /// would block the call until the user kills the app.
    nonisolated static let resourceTimeout: TimeInterval = 60

    /// SSE inactivity threshold. If no bytes arrive for this long the stream is
    /// considered dead and the caller's reconnect logic re-runs. Covers the
    /// silent-half-open-socket case (cell→wifi handoff, NAT idle reset).
    nonisolated static let streamInactivityTimeout: TimeInterval = 30

    /// Hard cap on the SSE line-buffer (1 MiB). A pathological server that
    /// never emits a newline must not be allowed to balloon memory.
    nonisolated static let streamMaxBufferBytes: Int = 1 << 20

    /// Dedicated session for JSON calls. `URLSession.shared`'s defaults
    /// (60s request, 7d resource, infinite cache) are wrong for a chat app on
    /// bad networks — short timeouts surface failures fast and let the UI
    /// retry instead of holding a spinner indefinitely.
    nonisolated private static let jsonSession: URLSession = {
        let config = URLSessionConfiguration.default
        config.timeoutIntervalForRequest = requestTimeout
        config.timeoutIntervalForResource = resourceTimeout
        config.waitsForConnectivity = false
        config.requestCachePolicy = .reloadIgnoringLocalCacheData
        config.httpAdditionalHeaders = ["Accept": "application/json"]
        return URLSession(configuration: config)
    }()

    /// Session used for SSE. `timeoutIntervalForRequest` here doubles as the
    /// stream inactivity timeout: per URLSession semantics it's the maximum
    /// gap between bytes received, so a healthy stream emitting events
    /// regularly resets the clock and stays alive indefinitely, while a
    /// silent half-open socket (cell→wifi handoff, NAT idle reset) fails
    /// within `streamInactivityTimeout` and triggers the caller's reconnect
    /// loop. `timeoutIntervalForResource` stays effectively unbounded so
    /// long-running missions aren't capped.
    nonisolated private static let streamSession: URLSession = {
        let config = URLSessionConfiguration.default
        config.timeoutIntervalForRequest = streamInactivityTimeout
        config.timeoutIntervalForResource = TimeInterval.greatestFiniteMagnitude
        config.waitsForConnectivity = false
        config.requestCachePolicy = .reloadIgnoringLocalCacheData
        return URLSession(configuration: config)
    }()

    // Configuration
    var baseURL: String {
        get { UserDefaults.standard.string(forKey: "api_base_url") ?? "" }
        set { UserDefaults.standard.set(newValue, forKey: "api_base_url") }
    }

    /// Whether the server URL has been configured
    var isConfigured: Bool {
        !baseURL.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }
    
    private var jwtToken: String? {
        get { UserDefaults.standard.string(forKey: "jwt_token") }
        set { UserDefaults.standard.set(newValue, forKey: "jwt_token") }
    }
    
    var authToken: String? {
        jwtToken
    }

    var isAuthenticated: Bool {
        jwtToken != nil
    }
    
    var authRequired: Bool = false
    var authMode: AuthMode = .singleTenant

    enum AuthMode: String {
        case disabled = "disabled"
        case singleTenant = "single_tenant"
        case multiUser = "multi_user"
    }
    
    
    // MARK: - Authentication
    
    func login(password: String, username: String? = nil) async throws -> Bool {
        struct LoginRequest: Encodable {
            let password: String
            let username: String?
        }
        
        struct LoginResponse: Decodable {
            let token: String
            let exp: Int
        }
        
        let response: LoginResponse = try await post("/api/auth/login", body: LoginRequest(password: password, username: username), authenticated: false)
        jwtToken = response.token
        return true
    }
    
    func logout() {
        jwtToken = nil
    }
    
    func checkHealth() async throws -> Bool {
        struct HealthResponse: Decodable {
            let status: String
            let authRequired: Bool
            let authMode: String?
            
            enum CodingKeys: String, CodingKey {
                case status
                case authRequired = "auth_required"
                case authMode = "auth_mode"
            }
        }
        
        let response: HealthResponse = try await get("/api/health", authenticated: false)
        authRequired = response.authRequired
        if let modeRaw = response.authMode, let mode = AuthMode(rawValue: modeRaw) {
            authMode = mode
        } else {
            authMode = authRequired ? .singleTenant : .disabled
        }
        return response.status == "ok"
    }
    
    // MARK: - Missions
    
    func listMissions() async throws -> [Mission] {
        try await get("/api/control/missions")
    }
    
    func getMission(id: String) async throws -> Mission {
        try await get("/api/control/missions/\(id)")
    }
    
    func getCurrentMission() async throws -> Mission? {
        try await get("/api/control/missions/current")
    }
    
    func createMission(
        workspaceId: String? = nil,
        title: String? = nil,
        agent: String? = nil,
        modelOverride: String? = nil,
        backend: String? = nil
    ) async throws -> Mission {
        struct CreateMissionRequest: Encodable {
            let workspaceId: String?
            let title: String?
            let agent: String?
            let modelOverride: String?
            let backend: String?

            enum CodingKeys: String, CodingKey {
                case workspaceId = "workspace_id"
                case title
                case agent
                case modelOverride = "model_override"
                case backend
            }
        }
        return try await post("/api/control/missions", body: CreateMissionRequest(
            workspaceId: workspaceId,
            title: title,
            agent: agent,
            modelOverride: modelOverride,
            backend: backend
        ))
    }
    
    func loadMission(id: String) async throws -> Mission {
        try await post("/api/control/missions/\(id)/load", body: EmptyBody())
    }
    
    func setMissionStatus(id: String, status: MissionStatus) async throws {
        struct StatusRequest: Encodable {
            let status: String
        }
        let _: EmptyResponse = try await post("/api/control/missions/\(id)/status", body: StatusRequest(status: status.rawValue))
    }
    
    func resumeMission(id: String) async throws -> Mission {
        try await post("/api/control/missions/\(id)/resume", body: EmptyBody())
    }
    
    func cancelMission(id: String) async throws {
        let _: EmptyResponse = try await post("/api/control/missions/\(id)/cancel", body: EmptyBody())
    }

    func deleteMission(id: String) async throws -> Bool {
        struct DeleteResponse: Decodable {
            let ok: Bool
            let deleted: String
        }
        let response: DeleteResponse = try await delete("/api/control/missions/\(id)")
        return response.ok
    }

    func getMissionEvents(id: String, types: [String]? = nil, limit: Int? = nil, offset: Int? = nil, latest: Bool = false) async throws -> [StoredEvent] {
        try await getMissionEventsWithMeta(
            id: id,
            types: types,
            limit: limit,
            offset: offset,
            latest: latest
        ).events
    }

    /// Fetch mission events along with the response metadata.
    ///
    /// `maxSequence` is parsed from the `X-Max-Sequence` response header. Backends
    /// that do not advertise this header return `nil` — callers should treat that
    /// as "delta resume not supported" and not seed any high-water mark.
    ///
    /// `sinceSeq` requests only events with `sequence > sinceSeq` (delta path used
    /// on SSE reconnect / scene-phase active). `latest` requests the tail page.
    func getMissionEventsWithMeta(
        id: String,
        types: [String]? = nil,
        limit: Int? = nil,
        offset: Int? = nil,
        latest: Bool = false,
        sinceSeq: Int64? = nil
    ) async throws -> MissionEventsResult {
        var queryItems: [URLQueryItem] = []
        if let types = types {
            queryItems.append(URLQueryItem(name: "types", value: types.joined(separator: ",")))
        }
        if let limit = limit {
            queryItems.append(URLQueryItem(name: "limit", value: String(limit)))
        }
        if let offset = offset {
            queryItems.append(URLQueryItem(name: "offset", value: String(offset)))
        }
        if latest {
            queryItems.append(URLQueryItem(name: "latest", value: "true"))
        }
        if let sinceSeq = sinceSeq {
            queryItems.append(URLQueryItem(name: "since_seq", value: String(sinceSeq)))
        }

        var urlString = "/api/control/missions/\(id)/events"
        if !queryItems.isEmpty {
            var components = URLComponents(string: urlString)
            components?.queryItems = queryItems
            if let fullPath = components?.string {
                urlString = fullPath
            }
        }

        let (events, response): ([StoredEvent], HTTPURLResponse) = try await getWithResponse(urlString)
        let maxSequence = (response.value(forHTTPHeaderField: "X-Max-Sequence") ?? response.value(forHTTPHeaderField: "x-max-sequence"))
            .flatMap { Int64($0) }
        return MissionEventsResult(events: events, maxSequence: maxSequence)
    }

    /// Get child (worker) missions for a boss mission.
    /// Filters the full mission list by parent_mission_id on the client side,
    /// since the backend includes parent_mission_id in the mission response.
    func getChildMissions(parentId: String) async throws -> [Mission] {
        let all: [Mission] = try await get("/api/control/missions?limit=200&offset=0")
        return all.filter { $0.parentMissionId == parentId }
    }

    func searchMissions(query: String, limit: Int? = nil) async throws -> [MissionSearchResult] {
        var queryItems: [URLQueryItem] = [URLQueryItem(name: "q", value: query)]
        if let limit {
            queryItems.append(URLQueryItem(name: "limit", value: String(limit)))
        }

        var components = URLComponents(string: "/api/control/missions/search")
        components?.queryItems = queryItems
        return try await get(components?.string ?? "/api/control/missions/search")
    }

    func searchMissionMoments(
        query: String,
        limit: Int? = nil,
        missionId: String? = nil
    ) async throws -> [MissionMomentSearchResult] {
        var queryItems: [URLQueryItem] = [URLQueryItem(name: "q", value: query)]
        if let limit {
            queryItems.append(URLQueryItem(name: "limit", value: String(limit)))
        }
        if let missionId, !missionId.isEmpty {
            queryItems.append(URLQueryItem(name: "mission_id", value: missionId))
        }

        var components = URLComponents(string: "/api/control/missions/search/moments")
        components?.queryItems = queryItems
        return try await get(components?.string ?? "/api/control/missions/search/moments")
    }

    func cleanupEmptyMissions() async throws -> Int {
        struct CleanupResponse: Decodable {
            let ok: Bool
            let deletedCount: Int

            enum CodingKeys: String, CodingKey {
                case ok
                case deletedCount = "deleted_count"
            }
        }
        let response: CleanupResponse = try await post("/api/control/missions/cleanup", body: EmptyBody())
        return response.deletedCount
    }
    
    // MARK: - Parallel Missions
    
    func getRunningMissions() async throws -> [RunningMissionInfo] {
        try await get("/api/control/running")
    }
    
    func startMissionParallel(id: String, content: String, model: String? = nil) async throws {
        struct ParallelRequest: Encodable {
            let content: String
            let model: String?
        }
        let _: EmptyResponse = try await post("/api/control/missions/\(id)/parallel", body: ParallelRequest(content: content, model: model))
    }
    
    func getParallelConfig() async throws -> ParallelConfig {
        try await get("/api/control/parallel/config")
    }

    // MARK: - Automations

    func listMissionAutomations(missionId: String) async throws -> [Automation] {
        try await get("/api/control/missions/\(missionId)/automations")
    }

    func createMissionAutomation(missionId: String, request: CreateAutomationRequest) async throws -> Automation {
        try await post("/api/control/missions/\(missionId)/automations", body: request)
    }

    func updateAutomation(id: String, request: UpdateAutomationRequest) async throws -> Automation {
        try await patch("/api/control/automations/\(id)", body: request)
    }

    func deleteAutomation(id: String) async throws {
        let _: EmptyResponse = try await delete("/api/control/automations/\(id)")
    }
    
    // MARK: - Control
    
    func sendMessage(content: String) async throws -> (id: String, queued: Bool) {
        struct MessageRequest: Encodable {
            let content: String
        }
        
        struct MessageResponse: Decodable {
            let id: String
            let queued: Bool
        }
        
        let response: MessageResponse = try await post("/api/control/message", body: MessageRequest(content: content))
        return (response.id, response.queued)
    }
    
    func cancelControl() async throws {
        let _: EmptyResponse = try await post("/api/control/cancel", body: EmptyBody())
    }

    // MARK: - Queue Management

    func getQueue() async throws -> [QueuedMessage] {
        try await get("/api/control/queue")
    }

    func removeFromQueue(messageId: String) async throws {
        let _: EmptyResponse = try await delete("/api/control/queue/\(messageId)")
    }

    func clearQueue() async throws -> Int {
        struct ClearResponse: Decodable {
            let cleared: Int
        }
        let response: ClearResponse = try await delete("/api/control/queue")
        return response.cleared
    }

    // MARK: - Tasks
    
    func listTasks() async throws -> [TaskState] {
        try await get("/api/tasks")
    }
    
    // MARK: - Runs
    
    func listRuns(limit: Int = 20, offset: Int = 0) async throws -> [Run] {
        struct RunsResponse: Decodable {
            let runs: [Run]
        }
        let response: RunsResponse = try await get("/api/runs?limit=\(limit)&offset=\(offset)")
        return response.runs
    }
    
    // MARK: - File System
    
    func listDirectory(path: String) async throws -> [FileEntry] {
        try await get("/api/fs/list?path=\(path.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? path)")
    }
    
    func createDirectory(path: String) async throws {
        struct MkdirRequest: Encodable {
            let path: String
        }
        let _: EmptyResponse = try await post("/api/fs/mkdir", body: MkdirRequest(path: path))
    }
    
    func deleteFile(path: String, recursive: Bool = false) async throws {
        struct RmRequest: Encodable {
            let path: String
            let recursive: Bool
        }
        let _: EmptyResponse = try await post("/api/fs/rm", body: RmRequest(path: path, recursive: recursive))
    }
    
    func downloadURL(path: String) -> URL? {
        guard var components = URLComponents(string: baseURL) else { return nil }
        components.path = "/api/fs/download"
        components.queryItems = [URLQueryItem(name: "path", value: path)]
        return components.url
    }
    
    func uploadFile(data: Data, fileName: String, directory: String) async throws -> String {
        guard let url = URL(string: "\(baseURL)/api/fs/upload?path=\(directory.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? directory)") else {
            throw APIError.invalidURL
        }
        
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        
        if let token = jwtToken {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        
        let boundary = UUID().uuidString
        request.setValue("multipart/form-data; boundary=\(boundary)", forHTTPHeaderField: "Content-Type")
        
        var body = Data()
        body.append("--\(boundary)\r\n".data(using: .utf8)!)
        body.append("Content-Disposition: form-data; name=\"file\"; filename=\"\(fileName)\"\r\n".data(using: .utf8)!)
        body.append("Content-Type: application/octet-stream\r\n\r\n".data(using: .utf8)!)
        body.append(data)
        body.append("\r\n--\(boundary)--\r\n".data(using: .utf8)!)
        
        request.httpBody = body
        
        let (responseData, response) = try await Self.jsonSession.data(for: request)

        guard let httpResponse = response as? HTTPURLResponse else {
            throw APIError.invalidResponse
        }

        if httpResponse.statusCode == 401 {
            logout()
            throw APIError.unauthorized
        }

        guard httpResponse.statusCode >= 200 && httpResponse.statusCode < 300 else {
            throw APIError.httpError(httpResponse.statusCode, String(data: responseData, encoding: .utf8))
        }

        struct UploadResponse: Decodable {
            let path: String
        }

        let uploadResponse = try JSONDecoder().decode(UploadResponse.self, from: responseData)
        return uploadResponse.path
    }
    
    // MARK: - Backends
    
    func listBackends() async throws -> [Backend] {
        try await get("/api/backends")
    }
    
    func getBackend(id: String) async throws -> Backend {
        try await get("/api/backends/\(id)")
    }

    // MARK: - Slash commands

    /// Fetch the per-backend list of built-in slash commands. Codex 0.128.0+
    /// surfaces `/goal <objective>` here; older binaries return an empty
    /// `codex` array and pre-/goal builds omit the field entirely.
    func getBuiltinCommands() async throws -> BuiltinCommandsResponse {
        try await get("/api/library/builtin-commands")
    }
    
    func listBackendAgents(backendId: String) async throws -> [BackendAgent] {
        try await get("/api/backends/\(backendId)/agents")
    }
    
    func getBackendConfig(backendId: String) async throws -> BackendConfig {
        try await get("/api/backends/\(backendId)/config")
    }
    
    // MARK: - Providers
    
    func listProviders(includeAll: Bool = false) async throws -> ProvidersResponse {
        let path = includeAll ? "/api/providers?include_all=true" : "/api/providers"
        return try await get(path)
    }
    
    // MARK: - Workspaces

    func listWorkspaces() async throws -> [Workspace] {
        try await get("/api/workspaces")
    }

    func listWorkspaces(completion: @escaping (Result<[Workspace], Error>) -> Void) {
        Task {
            do {
                let workspaces: [Workspace] = try await get("/api/workspaces")
                completion(.success(workspaces))
            } catch {
                completion(.failure(error))
            }
        }
    }

    func createWorkspace(name: String, type: WorkspaceType, completion: @escaping (Result<Workspace, Error>) -> Void) {
        Task {
            do {
                struct CreateWorkspaceRequest: Encodable {
                    let name: String
                    let workspace_type: String
                }
                let workspace: Workspace = try await post("/api/workspaces", body: CreateWorkspaceRequest(name: name, workspace_type: type.rawValue))
                completion(.success(workspace))
            } catch {
                completion(.failure(error))
            }
        }
    }

    func getWorkspace(id: String) async throws -> Workspace {
        try await get("/api/workspaces/\(id)")
    }

    // MARK: - FIDO Signing

    func fidoRespond(requestId: String, approved: Bool) async throws {
        struct FidoRespondBody: Encodable {
            let request_id: String
            let approved: Bool
        }
        let _: EmptyResponse = try await post(
            "/api/fido/respond",
            body: FidoRespondBody(request_id: requestId, approved: approved)
        )
    }

    // MARK: - SSE Streaming

    func streamControl(onEvent: @escaping (String, [String: Any]) -> Void) -> Task<Void, Never> {
        let base = baseURL
        let token = jwtToken
        let maxBuffer = Self.streamMaxBufferBytes
        let inactivity = Self.streamInactivityTimeout
        let session = Self.streamSession

        return Task { [weak self] in
            guard let url = URL(string: "\(base)/api/control/stream") else { return }

            var request = URLRequest(url: url)
            request.setValue("text/event-stream", forHTTPHeaderField: "Accept")
            request.setValue("no-cache", forHTTPHeaderField: "Cache-Control")
            if let token {
                request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
            }

            do {
                let (rawStream, response) = try await session.bytes(for: request)

                // Reject HTTP errors up front. The previous code happily fed
                // a 401 HTML body through the SSE parser, which silently
                // dropped events and left the user staring at "Reconnecting…".
                if let http = response as? HTTPURLResponse {
                    guard (200..<300).contains(http.statusCode) else {
                        if http.statusCode == 401 {
                            await MainActor.run { self?.logout() }
                        }
                        onEvent("error", [
                            "message": "Stream rejected by server (HTTP \(http.statusCode))",
                            "status": http.statusCode
                        ])
                        return
                    }
                }

                // `rawStream.lines` decodes chunks as UTF-8 and yields per
                // line. Replaces the previous byte-by-byte
                // `String(bytes: [byte], encoding: .utf8)` loop which silently
                // dropped any byte belonging to a multi-byte UTF-8 codepoint —
                // every emoji or non-ASCII character in the agent stream was
                // being corrupted.
                //
                // Inactivity is enforced by `streamSession`'s
                // `timeoutIntervalForRequest`: per URLSession semantics that
                // is the max gap between received bytes, so a healthy stream
                // resets it on every event and a silent half-open socket
                // errors out within `streamInactivityTimeout`. No manual
                // watchdog needed.
                var eventType = "message"
                var dataLines: [String] = []
                var bufferedBytes = 0

                for try await line in rawStream.lines {
                    if Task.isCancelled { return }

                    // Hard cap on per-event payload — a server that never
                    // emits a blank line must not balloon this buffer.
                    bufferedBytes += line.utf8.count + 1
                    if bufferedBytes > maxBuffer {
                        onEvent("error", ["message": "Stream event exceeded \(maxBuffer) bytes — dropping connection"])
                        return
                    }

                    if line.isEmpty {
                        Self.dispatchSSEEvent(
                            eventType: eventType,
                            dataLines: dataLines,
                            onEvent: onEvent
                        )
                        eventType = "message"
                        dataLines.removeAll(keepingCapacity: true)
                        bufferedBytes = 0
                        continue
                    }

                    if line.hasPrefix(":") { continue }  // comment

                    guard let colonIdx = line.firstIndex(of: ":") else { continue }
                    let field = line[..<colonIdx]
                    var value = line[line.index(after: colonIdx)...]
                    // SSE spec: strip exactly one leading space.
                    if value.first == " " { value = value.dropFirst() }

                    switch field {
                    case "event":
                        eventType = String(value)
                    case "data":
                        dataLines.append(String(value))
                    default:
                        break  // ignore id/retry/unknown
                    }
                }
            } catch is CancellationError {
                return
            } catch let urlError as URLError where urlError.code == .timedOut {
                onEvent("error", [
                    "message": "Stream idle (no data for \(Int(inactivity))s) — reconnecting",
                    "reason": "inactivity"
                ])
            } catch {
                if !Task.isCancelled {
                    onEvent("error", ["message": "Stream connection failed: \(error.localizedDescription)"])
                }
            }
        }
    }

    /// Flush a complete SSE event. `dataLines` are joined with `\n` per spec —
    /// the previous code joined with empty string, mangling multi-line JSON
    /// payloads. Only object-shaped JSON is forwarded; scalars/arrays/garbage
    /// surface as a structured `parseError` so the caller can log them
    /// instead of silently dropping data.
    @MainActor
    private static func dispatchSSEEvent(
        eventType: String,
        dataLines: [String],
        onEvent: (String, [String: Any]) -> Void
    ) {
        guard !dataLines.isEmpty else { return }
        let payload = dataLines.joined(separator: "\n")
        guard !payload.isEmpty else { return }

        guard let data = payload.data(using: .utf8) else {
            onEvent("parseError", ["raw": payload, "reason": "non-utf8 payload"])
            return
        }
        do {
            let parsed = try JSONSerialization.jsonObject(with: data, options: [.fragmentsAllowed])
            if let obj = parsed as? [String: Any] {
                onEvent(eventType, obj)
            } else {
                onEvent("parseError", ["raw": payload, "reason": "non-object payload"])
            }
        } catch {
            onEvent("parseError", ["raw": payload, "reason": error.localizedDescription])
        }
    }
    
    // MARK: - Private Helpers
    
    private struct EmptyBody: Encodable {}
    private struct EmptyResponse: Decodable {}
    
    private func get<T: Decodable>(_ path: String, authenticated: Bool = true) async throws -> T {
        try await getWithResponse(path, authenticated: authenticated).0
    }

    /// GET that also returns the underlying `HTTPURLResponse` so callers can
    /// inspect response headers (e.g. `X-Max-Sequence` for delta-event resume).
    private func getWithResponse<T: Decodable>(
        _ path: String,
        authenticated: Bool = true
    ) async throws -> (T, HTTPURLResponse) {
        guard let url = URL(string: "\(baseURL)\(path)") else {
            throw APIError.invalidURL
        }

        var request = URLRequest(url: url)
        request.httpMethod = "GET"

        if authenticated, let token = jwtToken {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }

        return try await executeWithResponse(request)
    }
    
    private func post<T: Decodable, B: Encodable>(_ path: String, body: B, authenticated: Bool = true) async throws -> T {
        guard let url = URL(string: "\(baseURL)\(path)") else {
            throw APIError.invalidURL
        }

        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")

        if authenticated, let token = jwtToken {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }

        request.httpBody = try JSONEncoder().encode(body)

        return try await execute(request)
    }

    private func delete<T: Decodable>(_ path: String, authenticated: Bool = true) async throws -> T {
        guard let url = URL(string: "\(baseURL)\(path)") else {
            throw APIError.invalidURL
        }

        var request = URLRequest(url: url)
        request.httpMethod = "DELETE"

        if authenticated, let token = jwtToken {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }

        return try await execute(request)
    }

    private func patch<T: Decodable, B: Encodable>(_ path: String, body: B, authenticated: Bool = true) async throws -> T {
        guard let url = URL(string: "\(baseURL)\(path)") else {
            throw APIError.invalidURL
        }

        var request = URLRequest(url: url)
        request.httpMethod = "PATCH"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")

        if authenticated, let token = jwtToken {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }

        request.httpBody = try JSONEncoder().encode(body)

        return try await execute(request)
    }
    
    private func execute<T: Decodable>(_ request: URLRequest) async throws -> T {
        try await executeWithResponse(request).0
    }

    private func executeWithResponse<T: Decodable>(_ request: URLRequest) async throws -> (T, HTTPURLResponse) {
        let (data, response) = try await Self.jsonSession.data(for: request)

        guard let httpResponse = response as? HTTPURLResponse else {
            throw APIError.invalidResponse
        }

        if httpResponse.statusCode == 401 {
            logout()
            throw APIError.unauthorized
        }

        guard httpResponse.statusCode >= 200 && httpResponse.statusCode < 300 else {
            throw APIError.httpError(httpResponse.statusCode, String(data: data, encoding: .utf8))
        }

        // Handle empty responses
        if data.isEmpty || (T.self == EmptyResponse.self) {
            if let empty = EmptyResponse() as? T {
                return (empty, httpResponse)
            }
        }

        // Decode off the main actor. APIService is `@MainActor`, so without
        // this hop a multi-MB events payload would block the UI thread for
        // hundreds of ms during decode. We use a `nonisolated` global-actor-free
        // helper wrapped in `Task.detached` so the decode runs on a cooperative
        // thread; `DecodedBox` is `@unchecked Sendable` because the decoded
        // value is treated as immutable from the moment we ship it back.
        let box: DecodedBox<T>
        do {
            box = try await Task.detached(priority: .userInitiated) {
                try DecodedBox(value: JSONDecoder().decode(T.self, from: data))
            }.value
        } catch {
            throw APIError.decodingError(error)
        }
        return (box.value, httpResponse)
    }
}

/// Sendable transport for decoded values across the detached-decode boundary.
/// Marked `@unchecked` because `T: Decodable` makes no Sendable promise and we
/// can't constrain every existing Decodable type in the codebase. Safe in
/// practice: the producer (decode task) has the only reference until `.value`
/// crosses back to the consumer actor, after which the producer is gone.
private struct DecodedBox<T>: @unchecked Sendable {
    let value: T
}

/// Result of a `/missions/:id/events` fetch. `maxSequence` is the response's
/// `X-Max-Sequence` header — `nil` if the backend doesn't advertise it (older
/// versions). Callers must treat `nil` as "delta resume unsupported".
struct MissionEventsResult {
    let events: [StoredEvent]
    let maxSequence: Int64?
}

enum APIError: LocalizedError {
    case invalidURL
    case invalidResponse
    case unauthorized
    case httpError(Int, String?)
    case decodingError(Error)
    
    var errorDescription: String? {
        switch self {
        case .invalidURL:
            return "Invalid URL"
        case .invalidResponse:
            return "Invalid response from server"
        case .unauthorized:
            return "Authentication required"
        case .httpError(let code, let message):
            return "HTTP \(code): \(message ?? "Unknown error")"
        case .decodingError(let error):
            return "Failed to decode response: \(error.localizedDescription)"
        }
    }
}
