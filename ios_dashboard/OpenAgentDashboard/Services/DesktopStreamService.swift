//
//  DesktopStreamService.swift
//  OpenAgentDashboard
//
//  WebSocket client for MJPEG desktop streaming
//

import Foundation
import Observation
import UIKit

@MainActor
@Observable
final class DesktopStreamService {
    static let shared = DesktopStreamService()
    nonisolated init() {}

    // Stream state
    var isConnected = false
    var isPaused = false
    var currentFrame: UIImage?
    var errorMessage: String?
    var frameCount: UInt64 = 0
    var fps: Int = 10
    var quality: Int = 70

    private var webSocket: URLSessionWebSocketTask?
    private var displayId: String?
    // Connection ID to prevent stale callbacks from corrupting state
    private var connectionId: UInt64 = 0

    // MARK: - Connection

    func connect(displayId: String) {
        disconnect()
        self.displayId = displayId
        self.errorMessage = nil
        // Increment connection ID to invalidate any pending callbacks from old connections
        self.connectionId += 1

        guard let url = buildWebSocketURL(displayId: displayId) else {
            errorMessage = "Invalid URL"
            return
        }

        let session = URLSession(configuration: .default)
        var request = URLRequest(url: url)

        // Add JWT token via subprotocol (same pattern as console)
        if let token = UserDefaults.standard.string(forKey: "jwt_token") {
            request.setValue("openagent, jwt.\(token)", forHTTPHeaderField: "Sec-WebSocket-Protocol")
        } else {
            request.setValue("openagent", forHTTPHeaderField: "Sec-WebSocket-Protocol")
        }

        webSocket = session.webSocketTask(with: request)
        webSocket?.resume()
        // Note: isConnected will be set to true on first successful message receive

        // Start receiving frames with current connection ID
        receiveMessage(forConnection: connectionId)
    }

    func disconnect() {
        webSocket?.cancel(with: .normalClosure, reason: nil)
        webSocket = nil
        isConnected = false
        isPaused = false  // Reset paused state for fresh connection
        currentFrame = nil
        frameCount = 0
    }

    // MARK: - Controls

    func pause() {
        guard isConnected else { return }
        isPaused = true
        sendCommand(["t": "pause"])
    }

    func resume() {
        guard isConnected else { return }
        isPaused = false
        sendCommand(["t": "resume"])
    }

    func setFps(_ newFps: Int) {
        fps = newFps
        guard isConnected else { return }
        sendCommand(["t": "fps", "fps": newFps])
    }

    func setQuality(_ newQuality: Int) {
        quality = newQuality
        guard isConnected else { return }
        sendCommand(["t": "quality", "quality": newQuality])
    }

    // MARK: - Private

    private func buildWebSocketURL(displayId: String) -> URL? {
        let baseURL = UserDefaults.standard.string(forKey: "api_base_url") ?? "https://agent-backend.thomas.md"

        // Convert https to wss, http to ws
        var wsURL = baseURL
            .replacingOccurrences(of: "https://", with: "wss://")
            .replacingOccurrences(of: "http://", with: "ws://")

        // Build query string
        let encodedDisplay = displayId.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? displayId
        wsURL += "/api/desktop/stream?display=\(encodedDisplay)&fps=\(fps)&quality=\(quality)"

        return URL(string: wsURL)
    }

    private func sendCommand(_ command: [String: Any]) {
        guard let webSocket = webSocket,
              let data = try? JSONSerialization.data(withJSONObject: command),
              let string = String(data: data, encoding: .utf8) else {
            return
        }

        webSocket.send(.string(string)) { [weak self] error in
            if let error = error {
                Task { @MainActor in
                    self?.errorMessage = "Send failed: \(error.localizedDescription)"
                }
            }
        }
    }

    private func receiveMessage(forConnection connId: UInt64) {
        webSocket?.receive { [weak self] result in
            Task { @MainActor in
                guard let self = self else { return }

                // Ignore callbacks from stale connections
                // This prevents old WebSocket failures from corrupting new connection state
                guard self.connectionId == connId else { return }

                switch result {
                case .success(let message):
                    // Mark as connected on first successful message
                    if !self.isConnected {
                        self.isConnected = true
                    }
                    self.handleMessage(message)
                    // Continue receiving with same connection ID
                    self.receiveMessage(forConnection: connId)

                case .failure(let error):
                    self.errorMessage = "Connection lost: \(error.localizedDescription)"
                    self.isConnected = false
                }
            }
        }
    }

    private func handleMessage(_ message: URLSessionWebSocketTask.Message) {
        switch message {
        case .data(let data):
            // Binary data = JPEG frame
            if let image = UIImage(data: data) {
                currentFrame = image
                frameCount += 1
                errorMessage = nil
            }

        case .string(let text):
            // Text message = JSON (error or control response)
            if let data = text.data(using: .utf8),
               let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
                if let error = json["error"] as? String {
                    errorMessage = json["message"] as? String ?? error
                }
            }

        @unknown default:
            break
        }
    }
}
