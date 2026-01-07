//
//  APIService.swift
//  OpenAgentDashboard
//
//  HTTP API client for the Open Agent backend
//

import Foundation
import Observation

@MainActor
@Observable
final class APIService {
    static let shared = APIService()
    nonisolated init() {}
    
    // Configuration
    var baseURL: String {
        get { UserDefaults.standard.string(forKey: "api_base_url") ?? "https://agent-backend.thomas.md" }
        set { UserDefaults.standard.set(newValue, forKey: "api_base_url") }
    }
    
    private var jwtToken: String? {
        get { UserDefaults.standard.string(forKey: "jwt_token") }
        set { UserDefaults.standard.set(newValue, forKey: "jwt_token") }
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
    
    func createMission(workspaceId: String? = nil, title: String? = nil, modelOverride: String? = nil) async throws -> Mission {
        struct CreateMissionRequest: Encodable {
            let workspaceId: String?
            let title: String?
            let modelOverride: String?

            enum CodingKeys: String, CodingKey {
                case workspaceId = "workspace_id"
                case title
                case modelOverride = "model_override"
            }
        }
        return try await post("/api/control/missions", body: CreateMissionRequest(workspaceId: workspaceId, title: title, modelOverride: modelOverride))
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
        
        let (responseData, response) = try await URLSession.shared.data(for: request)
        
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
    
    // MARK: - Workspaces

    // MARK: - Agents

    func listAgents(completion: @escaping (Result<[AgentConfig], Error>) -> Void) {
        Task {
            do {
                let agents: [AgentConfig] = try await get("/api/agents")
                completion(.success(agents))
            } catch {
                completion(.failure(error))
            }
        }
    }

    func createAgent(name: String, modelId: String, completion: @escaping (Result<AgentConfig, Error>) -> Void) {
        Task {
            do {
                struct CreateAgentRequest: Encodable {
                    let name: String
                    let model_id: String
                }
                let agent: AgentConfig = try await post("/api/agents", body: CreateAgentRequest(name: name, model_id: modelId))
                completion(.success(agent))
            } catch {
                completion(.failure(error))
            }
        }
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

    // MARK: - SSE Streaming

    func streamControl(onEvent: @escaping (String, [String: Any]) -> Void) -> Task<Void, Never> {
        Task {
            guard let url = URL(string: "\(baseURL)/api/control/stream") else { return }
            
            var request = URLRequest(url: url)
            request.setValue("text/event-stream", forHTTPHeaderField: "Accept")
            
            if let token = jwtToken {
                request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
            }
            
            do {
                let (stream, _) = try await URLSession.shared.bytes(for: request)
                
                var buffer = ""
                for try await byte in stream {
                    guard !Task.isCancelled else { break }
                    
                    if let char = String(bytes: [byte], encoding: .utf8) {
                        buffer.append(char)
                        
                        // Look for double newline (end of SSE event)
                        while let range = buffer.range(of: "\n\n") {
                            let eventString = String(buffer[..<range.lowerBound])
                            buffer = String(buffer[range.upperBound...])
                            
                            parseSSEEvent(eventString, onEvent: onEvent)
                        }
                    }
                }
            } catch {
                if !Task.isCancelled {
                    onEvent("error", ["message": "Stream connection failed: \(error.localizedDescription)"])
                }
            }
        }
    }
    
    private func parseSSEEvent(_ eventString: String, onEvent: @escaping (String, [String: Any]) -> Void) {
        var eventType = "message"
        var dataString = ""
        
        for line in eventString.split(separator: "\n", omittingEmptySubsequences: false) {
            let lineStr = String(line)
            if lineStr.hasPrefix("event:") {
                eventType = String(lineStr.dropFirst(6)).trimmingCharacters(in: .whitespaces)
            } else if lineStr.hasPrefix("data:") {
                dataString += String(lineStr.dropFirst(5)).trimmingCharacters(in: .whitespaces)
            }
        }
        
        guard !dataString.isEmpty else { return }
        
        do {
            if let data = dataString.data(using: .utf8),
               let json = try JSONSerialization.jsonObject(with: data) as? [String: Any] {
                onEvent(eventType, json)
            }
        } catch {
            // Ignore parse errors
        }
    }
    
    // MARK: - Private Helpers
    
    private struct EmptyBody: Encodable {}
    private struct EmptyResponse: Decodable {}
    
    private func get<T: Decodable>(_ path: String, authenticated: Bool = true) async throws -> T {
        guard let url = URL(string: "\(baseURL)\(path)") else {
            throw APIError.invalidURL
        }
        
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        
        if authenticated, let token = jwtToken {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        
        return try await execute(request)
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
    
    private func execute<T: Decodable>(_ request: URLRequest) async throws -> T {
        let (data, response) = try await URLSession.shared.data(for: request)
        
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
                return empty
            }
        }
        
        let decoder = JSONDecoder()
        return try decoder.decode(T.self, from: data)
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
