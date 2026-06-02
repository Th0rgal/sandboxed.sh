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

    nonisolated static let defaultBaseURL: String = {
        let raw = Bundle.main.object(forInfoDictionaryKey: "SandboxedDefaultAPIBaseURL") as? String
        return raw?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
    }()

    /// Per-request timeout for ordinary JSON calls. Anything that hasn't started
    /// returning data within this window is treated as failed. Default
    /// `URLSession.shared` ships with 60s, which is far too long for a chat
    /// app's cold-start path — the user sits at "Connecting…" with no feedback.
    /// Combined with retry-on-transient at the call site, a tight 10s surfaces
    /// failures fast without sacrificing reliability.
    nonisolated static let requestTimeout: TimeInterval = 10

    /// Full-transfer cap for ordinary JSON calls. Default is 7 days; a stalled
    /// large download (e.g. a multi-MB event tail over a flaky cellular link)
    /// would block the call until the user kills the app. 25s caps the longest
    /// user-attention window on a stuck spinner; the retry layer handles the
    /// failure cleanly.
    nonisolated static let resourceTimeout: TimeInterval = 25

    /// SSE inactivity threshold. If no bytes arrive for this long the stream is
    /// considered dead and the caller's reconnect logic re-runs. Covers the
    /// silent-half-open-socket case (cell→wifi handoff, NAT idle reset).
    ///
    /// Set to 60s rather than 30s because some agents (Codex high-reasoning,
    /// long-running tool calls) emit no bytes for ≥30s during normal
    /// operation. The proper fix is server-side pings every ~15s for every
    /// open stream; until that lands universally, 60s is the safer tradeoff
    /// — false-positive reconnect storms are worse than slow detection of a
    /// genuinely dead socket, which `NetworkMonitor` (NWPathMonitor +
    /// /api/health probes) catches independently.
    nonisolated static let streamInactivityTimeout: TimeInterval = 60

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

    /// Session for the Ask assistant. Its synchronous backend runs an agentic
    /// loop (up to 6 LLM iterations × ~60s each, plus tool execution) and emits
    /// no bytes until the whole turn finishes, so the 10s/25s JSON timeouts
    /// would abort nearly every Ask request. Generous bounds instead.
    nonisolated private static let askSession: URLSession = {
        let config = URLSessionConfiguration.default
        config.timeoutIntervalForRequest = 200
        config.timeoutIntervalForResource = 420
        config.waitsForConnectivity = false
        config.requestCachePolicy = .reloadIgnoringLocalCacheData
        config.httpAdditionalHeaders = ["Accept": "application/json"]
        return URLSession(configuration: config)
    }()

    // Configuration
    var baseURL: String {
        get {
            let stored = UserDefaults.standard.string(forKey: "api_base_url")?
                .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            return stored.isEmpty ? Self.defaultBaseURL : stored
        }
        set {
            UserDefaults.standard.set(
                newValue.trimmingCharacters(in: .whitespacesAndNewlines),
                forKey: "api_base_url"
            )
        }
    }

    /// Whether the server URL has been configured
    var isConfigured: Bool {
        !baseURL.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty &&
            makeURL("/api/health") != nil
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
    var authSessionExpired: Bool = false
    var onSuccessfulAuthenticatedRequest: (() -> Void)?

    enum AuthMode: String {
        case disabled = "disabled"
        case singleTenant = "single_tenant"
        case multiUser = "multi_user"
    }

    enum ControlStreamTransport: String, Sendable {
        case sse
        case webSocket = "ws"
    }

    enum ControlStreamPhase: String, Sendable {
        case connecting
        case open
        case heartbeat
        case event
        case closed
        case error
        case fallback
    }

    struct ControlStreamDiagnostic: Sendable {
        let transport: ControlStreamTransport
        let phase: ControlStreamPhase
        let host: String?
        let status: Int?
        let bytes: Int?
        let error: String?
        let eventType: String?
        let generation: Int
        let timestamp: Date
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
        authSessionExpired = false
        return true
    }
    
    func logout() {
        jwtToken = nil
        authSessionExpired = false
    }

    func markSessionExpired() {
        jwtToken = nil
        authSessionExpired = true
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

    /// Tell the backend the user has opened this mission. The first call
    /// (when `first_viewed_at` is still null on the row) starts the 1h ack
    /// grace timer for `awaiting_user` missions; later calls are no-ops on
    /// the server. Returns the updated mission so callers can rerender the
    /// "opened" dot immediately.
    func markMissionOpened(id: String) async throws -> Mission {
        try await post("/api/control/missions/\(id)/opened", body: EmptyBody())
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

    func getMissionEvents(id: String, types: [String]? = nil, limit: Int? = nil, beforeSeq: Int64? = nil) async throws -> [StoredEvent] {
        try await getMissionEventsWithMeta(
            id: id,
            types: types,
            limit: limit,
            beforeSeq: beforeSeq
        ).events
    }

    /// Fetch mission events along with the response metadata.
    ///
    /// `maxSequence` is parsed from the `X-Max-Sequence` response header. Backends
    /// that do not advertise this header return `nil` — callers should treat that
    /// as "delta resume not supported" and not seed any high-water mark.
    ///
    /// `sinceSeq` requests only events with `sequence > sinceSeq` (delta path used
    /// on SSE reconnect / scene-phase active). `beforeSeq` requests events with
    /// `sequence < beforeSeq` (backwards pagination).
    func getMissionEventsWithMeta(
        id: String,
        types: [String]? = nil,
        limit: Int? = nil,
        sinceSeq: Int64? = nil,
        beforeSeq: Int64? = nil
    ) async throws -> MissionEventsResult {
        var queryItems: [URLQueryItem] = []
        if let types = types {
            queryItems.append(URLQueryItem(name: "types", value: types.joined(separator: ",")))
        }
        if let limit = limit {
            queryItems.append(URLQueryItem(name: "limit", value: String(limit)))
        }
        if let sinceSeq = sinceSeq {
            queryItems.append(URLQueryItem(name: "since_seq", value: String(sinceSeq)))
        }
        if let beforeSeq = beforeSeq {
            queryItems.append(URLQueryItem(name: "before_seq", value: String(beforeSeq)))
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

    func getMissionSnapshot(id: String) async throws -> MissionSnapshotResult {
        try await get("/api/control/missions/\(id)/snapshot")
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
    
    /// Send a control message. `clientMessageId` doubles as an idempotency key
    /// — the backend (see `control.rs:ControlMessageRequest`) uses it as the
    /// message id, so a POST whose response is lost can be retried without
    /// creating two server-side messages. `missionId` pins the send to the
    /// mission the user is actually looking at; without it the backend routes
    /// to the user's "current" mission, which can drift when parallel
    /// missions are juggled.
    func sendMessage(
        content: String,
        clientMessageId: String,
        missionId: String? = nil
    ) async throws -> (id: String, queued: Bool) {
        struct MessageRequest: Encodable {
            let content: String
            let client_message_id: String
            let mission_id: String?
        }

        struct MessageResponse: Decodable {
            let id: String
            let queued: Bool
        }

        let response: MessageResponse = try await post(
            "/api/control/message",
            body: MessageRequest(
                content: content,
                client_message_id: clientMessageId,
                mission_id: missionId
            )
        )
        return (response.id, response.queued)
    }

    /// Retry policy for transient send failures. Combined with idempotency
    /// (see `sendMessage`), retries are safe — at worst the server sees the
    /// same `client_message_id` twice and returns the cached response.
    ///
    /// Mirrors `postControlMessageWithRetry` in the web client. Jittered
    /// backoff (200ms, 600ms, 1.5s) keeps total round-trip ≤ ~2.3 s in the
    /// worst case so the user gets feedback fast.
    func sendMessageWithRetry(
        content: String,
        clientMessageId: String,
        missionId: String? = nil
    ) async throws -> (id: String, queued: Bool) {
        let delays: [UInt64] = [200_000_000, 600_000_000, 1_500_000_000]
        var attempt = 0
        while true {
            do {
                return try await sendMessage(
                    content: content,
                    clientMessageId: clientMessageId,
                    missionId: missionId
                )
            } catch let urlError as URLError where Self.isRetriableSendError(urlError) {
                guard attempt < delays.count else { throw urlError }
                // Add ±25% jitter so concurrent retries don't synchronise.
                let base = Double(delays[attempt])
                let jitter = Double.random(in: 0.75...1.25)
                try? await Task.sleep(nanoseconds: UInt64(base * jitter))
                attempt += 1
            }
        }
    }

    nonisolated private static func isRetriableSendError(_ error: URLError) -> Bool {
        switch error.code {
        case .timedOut,
             .networkConnectionLost,
             .notConnectedToInternet,
             .cannotConnectToHost,
             .dnsLookupFailed,
             .cannotFindHost,
             .resourceUnavailable:
            return true
        default:
            return false
        }
    }
    
    func cancelControl() async throws {
        let _: EmptyResponse = try await post("/api/control/cancel", body: EmptyBody())
    }

    // MARK: - Ask Assistant (non-interrupting sidecar co-pilot)

    func postAsk(
        missionId: String,
        content: String,
        threadId: String? = nil
    ) async throws -> AskSendResponse {
        struct AskRequest: Encodable {
            let content: String
            let thread_id: String?
        }
        return try await post(
            "/api/control/missions/\(missionId)/ask",
            body: AskRequest(content: content, thread_id: threadId),
            session: Self.askSession
        )
    }

    func listAskThreads(missionId: String) async throws -> [AskThread] {
        try await get("/api/control/missions/\(missionId)/ask/threads")
    }

    func getAskThread(missionId: String, threadId: String) async throws -> AskThreadDetail {
        try await get("/api/control/missions/\(missionId)/ask/threads/\(threadId)")
    }

    func deleteAskThread(missionId: String, threadId: String) async throws {
        let _: EmptyResponse = try await delete(
            "/api/control/missions/\(missionId)/ask/threads/\(threadId)"
        )
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
            markSessionExpired()
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

    // MARK: - Control Streaming

    func streamControl(
        missionId: String?,
        sinceSeq: Int64? = nil,
        preferWebSocket: Bool = true,
        generation: Int = 0,
        onDiagnostic: ((ControlStreamDiagnostic) -> Void)? = nil,
        onEvent: @escaping (String, [String: Any]) -> Void
    ) -> Task<Void, Never> {
        let token = jwtToken

        return Task { [weak self] in
            guard let self else { return }
            if preferWebSocket, token != nil {
                let opened = await self.runControlWebSocket(
                    missionId: missionId,
                    sinceSeq: sinceSeq,
                    token: token,
                    generation: generation,
                    onDiagnostic: onDiagnostic,
                    onEvent: onEvent
                )
                if opened || Task.isCancelled {
                    return
                }
                self.emitDiagnostic(
                    transport: .webSocket,
                    phase: .fallback,
                    host: nil,
                    status: nil,
                    bytes: nil,
                    error: "WebSocket did not open; falling back to SSE",
                    eventType: nil,
                    generation: generation,
                    onDiagnostic: onDiagnostic
                )
            }
            await self.runControlSSE(
                missionId: missionId,
                sinceSeq: sinceSeq,
                token: token,
                generation: generation,
                onDiagnostic: onDiagnostic,
                onEvent: onEvent
            )
        }
    }

    private func runControlSSE(
        missionId: String?,
        sinceSeq: Int64?,
        token: String?,
        generation: Int,
        onDiagnostic: ((ControlStreamDiagnostic) -> Void)?,
        onEvent: @escaping (String, [String: Any]) -> Void
    ) async {
        let maxBuffer = Self.streamMaxBufferBytes
        let inactivity = Self.streamInactivityTimeout
        let session = Self.streamSession
        var queryItems = missionId.map { [URLQueryItem(name: "mission", value: $0)] } ?? []
        if let sinceSeq {
            queryItems.append(URLQueryItem(name: "since_seq", value: String(sinceSeq)))
        }
        guard let url = makeURL("/api/control/stream", queryItems: queryItems) else {
            emitDiagnostic(transport: .sse, phase: .error, host: nil, status: nil, bytes: nil, error: "Invalid server URL", eventType: nil, generation: generation, onDiagnostic: onDiagnostic)
            onEvent("error", ["message": "Invalid server URL", "reason": "invalid_configuration"])
            return
        }

        emitDiagnostic(transport: .sse, phase: .connecting, host: url.host, status: nil, bytes: nil, error: nil, eventType: nil, generation: generation, onDiagnostic: onDiagnostic)

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
                        logout()
                    }
                    emitDiagnostic(transport: .sse, phase: .error, host: url.host, status: http.statusCode, bytes: nil, error: "Stream rejected", eventType: nil, generation: generation, onDiagnostic: onDiagnostic)
                    onEvent("error", [
                        "message": "Stream rejected by server (HTTP \(http.statusCode))",
                        "status": http.statusCode
                    ])
                    return
                }
                emitDiagnostic(transport: .sse, phase: .open, host: url.host, status: http.statusCode, bytes: nil, error: nil, eventType: nil, generation: generation, onDiagnostic: onDiagnostic)
            }
            onEvent("connected", ["type": "connected", "transport": ControlStreamTransport.sse.rawValue])

            // `rawStream.lines` decodes chunks as UTF-8 and yields per line.
            var eventType = "message"
            var dataLines: [String] = []
            var bufferedBytes = 0
            var bytesRead = 0

            for try await line in rawStream.lines {
                if Task.isCancelled { return }
                bytesRead += line.utf8.count + 1

                // Hard cap on per-event payload — a server that never emits a
                // blank line must not balloon this buffer.
                bufferedBytes += line.utf8.count + 1
                if bufferedBytes > maxBuffer {
                    emitDiagnostic(transport: .sse, phase: .error, host: url.host, status: nil, bytes: bytesRead, error: "Stream event exceeded buffer", eventType: nil, generation: generation, onDiagnostic: onDiagnostic)
                    onEvent("error", ["message": "Stream event exceeded \(maxBuffer) bytes — dropping connection"])
                    return
                }

                if line.isEmpty {
                    let dispatched = Self.dispatchSSEEvent(
                        eventType: eventType,
                        dataLines: dataLines,
                        onEvent: onEvent
                    )
                    if dispatched {
                        emitDiagnostic(transport: .sse, phase: .event, host: url.host, status: nil, bytes: bytesRead, error: nil, eventType: eventType, generation: generation, onDiagnostic: onDiagnostic)
                    }
                    eventType = "message"
                    dataLines.removeAll(keepingCapacity: true)
                    bufferedBytes = 0
                    continue
                }

                if line.hasPrefix(":") {
                    emitDiagnostic(transport: .sse, phase: .heartbeat, host: url.host, status: nil, bytes: bytesRead, error: nil, eventType: "heartbeat", generation: generation, onDiagnostic: onDiagnostic)
                    onEvent("heartbeat", ["type": "heartbeat", "transport": ControlStreamTransport.sse.rawValue])
                    continue
                }

                guard let colonIdx = line.firstIndex(of: ":") else { continue }
                let field = line[..<colonIdx]
                var value = line[line.index(after: colonIdx)...]
                if value.first == " " { value = value.dropFirst() }

                switch field {
                case "event":
                    eventType = String(value)
                case "data":
                    dataLines.append(String(value))
                default:
                    break
                }
            }
            emitDiagnostic(transport: .sse, phase: .closed, host: url.host, status: nil, bytes: bytesRead, error: nil, eventType: nil, generation: generation, onDiagnostic: onDiagnostic)
        } catch is CancellationError {
            return
        } catch let urlError as URLError where urlError.code == .timedOut {
            emitDiagnostic(transport: .sse, phase: .error, host: url.host, status: nil, bytes: nil, error: "inactivity", eventType: nil, generation: generation, onDiagnostic: onDiagnostic)
            onEvent("error", [
                "message": "Stream idle (no data for \(Int(inactivity))s) — reconnecting",
                "reason": "inactivity"
            ])
        } catch {
            if !Task.isCancelled {
                emitDiagnostic(transport: .sse, phase: .error, host: url.host, status: nil, bytes: nil, error: error.localizedDescription, eventType: nil, generation: generation, onDiagnostic: onDiagnostic)
                onEvent("error", ["message": "Stream connection failed: \(error.localizedDescription)"])
            }
        }
    }

    /// Returns true after the socket opened. A false return means callers may
    /// try SSE fallback immediately.
    private func runControlWebSocket(
        missionId: String?,
        sinceSeq: Int64?,
        token: String?,
        generation: Int,
        onDiagnostic: ((ControlStreamDiagnostic) -> Void)?,
        onEvent: @escaping (String, [String: Any]) -> Void
    ) async -> Bool {
        guard let url = makeWebSocketURL("/api/control/ws", queryItems: missionId.map { [URLQueryItem(name: "mission", value: $0)] } ?? []) else {
            emitDiagnostic(transport: .webSocket, phase: .error, host: nil, status: nil, bytes: nil, error: "Invalid WebSocket URL", eventType: nil, generation: generation, onDiagnostic: onDiagnostic)
            onEvent("error", ["message": "Invalid server URL", "reason": "invalid_configuration"])
            return false
        }

        emitDiagnostic(transport: .webSocket, phase: .connecting, host: url.host, status: nil, bytes: nil, error: nil, eventType: nil, generation: generation, onDiagnostic: onDiagnostic)

        var request = URLRequest(url: url)
        if let token {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        let task = Self.streamSession.webSocketTask(with: request)
        task.resume()

        var opened = false
        var bytesRead = 0
        do {
            if let sinceSeq {
                let resume = ["type": "resume", "since_seq": sinceSeq] as [String: Any]
                let data = try JSONSerialization.data(withJSONObject: resume)
                if let string = String(data: data, encoding: .utf8) {
                    try await task.send(.string(string))
                }
            }

            while !Task.isCancelled {
                let message = try await task.receive()
                let text: String
                switch message {
                case .string(let value):
                    text = value
                case .data(let data):
                    text = String(data: data, encoding: .utf8) ?? ""
                @unknown default:
                    continue
                }
                guard !text.isEmpty else { continue }
                bytesRead += text.utf8.count
                if !opened {
                    opened = true
                    emitDiagnostic(transport: .webSocket, phase: .open, host: url.host, status: 101, bytes: bytesRead, error: nil, eventType: nil, generation: generation, onDiagnostic: onDiagnostic)
                    onEvent("connected", ["type": "connected", "transport": ControlStreamTransport.webSocket.rawValue])
                }
                guard let data = text.data(using: .utf8),
                      let obj = try JSONSerialization.jsonObject(with: data, options: [.fragmentsAllowed]) as? [String: Any] else {
                    onEvent("parseError", ["raw": text, "reason": "invalid websocket json"])
                    continue
                }

                if let seq = obj["seq"] as? Int64 ?? (obj["seq"] as? Int).map(Int64.init),
                   obj["type"] == nil {
                    emitDiagnostic(transport: .webSocket, phase: .heartbeat, host: url.host, status: nil, bytes: bytesRead, error: nil, eventType: "heartbeat", generation: generation, onDiagnostic: onDiagnostic)
                    onEvent("heartbeat", ["type": "heartbeat", "transport": ControlStreamTransport.webSocket.rawValue, "seq": seq])
                    continue
                }

                let eventType = obj["type"] as? String ?? "message"
                emitDiagnostic(transport: .webSocket, phase: .event, host: url.host, status: nil, bytes: bytesRead, error: nil, eventType: eventType, generation: generation, onDiagnostic: onDiagnostic)
                onEvent(eventType, obj)
            }
        } catch is CancellationError {
            task.cancel(with: .normalClosure, reason: nil)
            return opened
        } catch {
            let nsError = error as NSError
            emitDiagnostic(transport: .webSocket, phase: .error, host: url.host, status: nsError.code, bytes: bytesRead, error: error.localizedDescription, eventType: nil, generation: generation, onDiagnostic: onDiagnostic)
            onEvent("error", [
                "message": opened
                    ? "WebSocket stream failed: \(error.localizedDescription)"
                    : "WebSocket failed to open: \(error.localizedDescription)",
                "reason": opened ? "web_socket_failed" : "web_socket_open_failed",
                "transport": ControlStreamTransport.webSocket.rawValue
            ])
        }
        task.cancel(with: .normalClosure, reason: nil)
        return opened
    }

    /// Flush a complete SSE event. `dataLines` are joined with `\n` per spec —
    /// the previous code joined with empty string, mangling multi-line JSON
    /// payloads. Only object-shaped JSON is forwarded; scalars/arrays/garbage
    /// surface as a structured `parseError` so the caller can log them
    /// instead of silently dropping data.
    @MainActor
    @discardableResult
    private static func dispatchSSEEvent(
        eventType: String,
        dataLines: [String],
        onEvent: (String, [String: Any]) -> Void
    ) -> Bool {
        guard !dataLines.isEmpty else { return false }
        let payload = dataLines.joined(separator: "\n")
        guard !payload.isEmpty else { return false }

        guard let data = payload.data(using: .utf8) else {
            onEvent("parseError", ["raw": payload, "reason": "non-utf8 payload"])
            return true
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
        return true
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
        guard let url = makeURL(path) else {
            throw APIError.invalidURL
        }

        var request = URLRequest(url: url)
        request.httpMethod = "GET"

        if authenticated, let token = jwtToken {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }

        return try await executeWithResponse(request)
    }
    
    private func post<T: Decodable, B: Encodable>(
        _ path: String,
        body: B,
        authenticated: Bool = true,
        session: URLSession? = nil
    ) async throws -> T {
        guard let url = makeURL(path) else {
            throw APIError.invalidURL
        }

        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")

        if authenticated, let token = jwtToken {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }

        request.httpBody = try JSONEncoder().encode(body)

        return try await execute(request, session: session)
    }

    private func delete<T: Decodable>(_ path: String, authenticated: Bool = true) async throws -> T {
        guard let url = makeURL(path) else {
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
        guard let url = makeURL(path) else {
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

    private func makeURL(_ path: String) -> URL? {
        makeURL(path, queryItems: [])
    }

    private func makeURL(_ path: String, queryItems: [URLQueryItem]) -> URL? {
        guard var components = makeURLComponents(path) else { return nil }
        components.queryItems = queryItems.isEmpty ? nil : queryItems
        return components.url
    }

    private func makeWebSocketURL(_ path: String, queryItems: [URLQueryItem]) -> URL? {
        guard var components = makeURLComponents(path) else { return nil }
        switch components.scheme?.lowercased() {
        case "https": components.scheme = "wss"
        case "http": components.scheme = "ws"
        default: return nil
        }
        components.queryItems = queryItems.isEmpty ? nil : queryItems
        return components.url
    }

    private func makeURLComponents(_ path: String) -> URLComponents? {
        let trimmedBase = baseURL.trimmingCharacters(in: .whitespacesAndNewlines)
            .trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        let normalizedPath = path.hasPrefix("/") ? path : "/\(path)"
        guard let components = URLComponents(string: "\(trimmedBase)\(normalizedPath)"),
              let scheme = components.scheme?.lowercased(),
              (scheme == "http" || scheme == "https"),
              components.host?.isEmpty == false
        else {
            return nil
        }
        return components
    }

    private func emitDiagnostic(
        transport: ControlStreamTransport,
        phase: ControlStreamPhase,
        host: String?,
        status: Int?,
        bytes: Int?,
        error: String?,
        eventType: String?,
        generation: Int,
        onDiagnostic: ((ControlStreamDiagnostic) -> Void)?
    ) {
        onDiagnostic?(ControlStreamDiagnostic(
            transport: transport,
            phase: phase,
            host: host,
            status: status,
            bytes: bytes,
            error: error,
            eventType: eventType,
            generation: generation,
            timestamp: Date()
        ))
    }
    
    private func execute<T: Decodable>(_ request: URLRequest, session: URLSession? = nil) async throws -> T {
        try await executeWithResponse(request, session: session).0
    }

    private func executeWithResponse<T: Decodable>(_ request: URLRequest, session: URLSession? = nil) async throws -> (T, HTTPURLResponse) {
        let requestLabel = "\(request.httpMethod ?? "GET") \(request.url?.path ?? "?")"
        let urlSession = session ?? Self.jsonSession
        let (data, response) = try await ControlPerformanceDiagnostics.shared.measureAsync(
            "api.request",
            detail: requestLabel
        ) {
            try await urlSession.data(for: request)
        }

        guard let httpResponse = response as? HTTPURLResponse else {
            throw APIError.invalidResponse
        }

        if httpResponse.statusCode == 401 {
            markSessionExpired()
            throw APIError.unauthorized
        }

        guard httpResponse.statusCode >= 200 && httpResponse.statusCode < 300 else {
            throw APIError.httpError(httpResponse.statusCode, String(data: data, encoding: .utf8))
        }

        if request.value(forHTTPHeaderField: "Authorization") != nil {
            onSuccessfulAuthenticatedRequest?()
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
            box = try await ControlPerformanceDiagnostics.shared.measureAsync(
                "api.decode",
                detail: requestLabel,
                count: data.count
            ) {
                try await Task.detached(priority: .userInitiated) {
                    try DecodedBox(value: JSONDecoder().decode(T.self, from: data))
                }.value
            }
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
/// `X-Max-Sequence` header.
///
/// `Sendable` so it can cross `async let` boundaries from the main actor —
/// `StoredEvent` and `AnyCodable` are Sendable above.
struct MissionEventsResult: Sendable {
    let events: [StoredEvent]
    let maxSequence: Int64?
}

struct MissionSnapshotResult: Codable, Sendable {
    let mission: Mission
    let events: [StoredEvent]
    let eventCounts: [String: Int]
    let visibilityCounts: [String: Int]
    let totalEvents: Int
    let latestSequence: Int64
    let childMissions: [Mission]
    let running: RunningMissionInfo?

    enum CodingKeys: String, CodingKey {
        case mission
        case events
        case eventCounts = "event_counts"
        case visibilityCounts = "visibility_counts"
        case totalEvents = "total_events"
        case latestSequence = "latest_sequence"
        case childMissions = "child_missions"
        case running
    }
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
