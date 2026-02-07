//
//  DesktopStreamService.swift
//  SandboxedDashboard
//
//  WebSocket client for MJPEG desktop streaming with Picture-in-Picture support
//

import Foundation
import Observation
import UIKit
import AVKit
import CoreMedia
import VideoToolbox

@MainActor
@Observable
final class DesktopStreamService: NSObject {
    static let shared = DesktopStreamService()
    override nonisolated init() { super.init() }

    // Stream state
    var isConnected = false
    var isPaused = false
    var currentFrame: UIImage?
    var errorMessage: String?
    var frameCount: UInt64 = 0
    var fps: Int = 10
    var quality: Int = 70

    // Picture-in-Picture state
    var isPipSupported: Bool { AVPictureInPictureController.isPictureInPictureSupported() }
    var isPipActive = false
    /// Whether PiP has been set up and is ready to use
    var isPipReady = false
    /// When true, disconnect and cleanup when PiP stops (set when view is dismissed while PiP is active)
    var shouldDisconnectAfterPip = false
    private(set) var pipController: AVPictureInPictureController?
    private(set) var sampleBufferDisplayLayer: AVSampleBufferDisplayLayer?

    // For PiP content source
    private var pipContentSource: AVPictureInPictureController.ContentSource?
    private var lastFrameTime: CMTime = .zero
    private var frameTimeScale: CMTimeScale = 600

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
            request.setValue("sandboxed, jwt.\(token)", forHTTPHeaderField: "Sec-WebSocket-Protocol")
        } else {
            request.setValue("sandboxed", forHTTPHeaderField: "Sec-WebSocket-Protocol")
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
        let baseURL = APIService.shared.baseURL
        guard !baseURL.isEmpty else { return nil }

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

                // Feed frame to PiP layer if active
                if isPipActive || pipController != nil {
                    feedFrameToPipLayer(image)
                }
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

    // MARK: - Picture-in-Picture

    /// Set up the PiP layer and controller
    func setupPip(in view: UIView) {
        guard isPipSupported else { return }

        // Create the sample buffer display layer
        let layer = AVSampleBufferDisplayLayer()
        layer.videoGravity = .resizeAspect
        layer.frame = view.bounds
        view.layer.addSublayer(layer)
        sampleBufferDisplayLayer = layer

        // Create PiP content source using the sample buffer layer
        let contentSource = AVPictureInPictureController.ContentSource(
            sampleBufferDisplayLayer: layer,
            playbackDelegate: self
        )
        pipContentSource = contentSource

        // Create PiP controller
        let controller = AVPictureInPictureController(contentSource: contentSource)
        controller.delegate = self
        pipController = controller
        isPipReady = true
    }

    /// Clean up PiP resources
    func cleanupPip() {
        stopPip()
        sampleBufferDisplayLayer?.removeFromSuperlayer()
        sampleBufferDisplayLayer = nil
        pipController = nil
        pipContentSource = nil
        isPipReady = false
    }

    /// Start Picture-in-Picture
    func startPip() {
        guard isPipSupported,
              let controller = pipController,
              controller.isPictureInPicturePossible else { return }

        controller.startPictureInPicture()
    }

    /// Stop Picture-in-Picture
    func stopPip() {
        pipController?.stopPictureInPicture()
    }

    /// Toggle Picture-in-Picture
    func togglePip() {
        if isPipActive {
            stopPip()
        } else {
            startPip()
        }
    }

    /// Feed a UIImage frame to the sample buffer layer for PiP display
    private func feedFrameToPipLayer(_ image: UIImage) {
        guard let cgImage = image.cgImage,
              let layer = sampleBufferDisplayLayer else { return }

        // Create pixel buffer from CGImage
        let width = cgImage.width
        let height = cgImage.height

        var pixelBuffer: CVPixelBuffer?
        let attrs: [CFString: Any] = [
            kCVPixelBufferCGImageCompatibilityKey: true,
            kCVPixelBufferCGBitmapContextCompatibilityKey: true,
            kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary
        ]

        let status = CVPixelBufferCreate(
            kCFAllocatorDefault,
            width, height,
            kCVPixelFormatType_32BGRA,
            attrs as CFDictionary,
            &pixelBuffer
        )

        guard status == kCVReturnSuccess, let buffer = pixelBuffer else { return }

        CVPixelBufferLockBaseAddress(buffer, [])
        defer { CVPixelBufferUnlockBaseAddress(buffer, []) }

        guard let context = CGContext(
            data: CVPixelBufferGetBaseAddress(buffer),
            width: width,
            height: height,
            bitsPerComponent: 8,
            bytesPerRow: CVPixelBufferGetBytesPerRow(buffer),
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue
        ) else { return }

        context.draw(cgImage, in: CGRect(x: 0, y: 0, width: width, height: height))

        // Create format description
        var formatDescription: CMFormatDescription?
        CMVideoFormatDescriptionCreateForImageBuffer(
            allocator: kCFAllocatorDefault,
            imageBuffer: buffer,
            formatDescriptionOut: &formatDescription
        )

        guard let format = formatDescription else { return }

        // Calculate timing
        let frameDuration = CMTime(value: 1, timescale: CMTimeScale(fps))
        let presentationTime = CMTimeAdd(lastFrameTime, frameDuration)
        lastFrameTime = presentationTime

        var timingInfo = CMSampleTimingInfo(
            duration: frameDuration,
            presentationTimeStamp: presentationTime,
            decodeTimeStamp: .invalid
        )

        // Create sample buffer
        var sampleBuffer: CMSampleBuffer?
        CMSampleBufferCreateReadyWithImageBuffer(
            allocator: kCFAllocatorDefault,
            imageBuffer: buffer,
            formatDescription: format,
            sampleTiming: &timingInfo,
            sampleBufferOut: &sampleBuffer
        )

        guard let sample = sampleBuffer else { return }

        // Enqueue to layer
        if #available(iOS 18.0, *) {
            if layer.sampleBufferRenderer.status == .failed {
                layer.sampleBufferRenderer.flush()
            }
            layer.sampleBufferRenderer.enqueue(sample)
        } else {
            if layer.status == .failed {
                layer.flush()
            }
            layer.enqueue(sample)
        }
    }
}

// MARK: - AVPictureInPictureControllerDelegate

extension DesktopStreamService: AVPictureInPictureControllerDelegate {
    nonisolated func pictureInPictureControllerWillStartPictureInPicture(_ pictureInPictureController: AVPictureInPictureController) {
        Task { @MainActor in
            isPipActive = true
        }
    }

    nonisolated func pictureInPictureControllerDidStopPictureInPicture(_ pictureInPictureController: AVPictureInPictureController) {
        Task { @MainActor in
            isPipActive = false
            // If the view was dismissed while PiP was active, clean up now
            if shouldDisconnectAfterPip {
                shouldDisconnectAfterPip = false
                cleanupPip()
                disconnect()
            }
        }
    }

    nonisolated func pictureInPictureController(_ pictureInPictureController: AVPictureInPictureController, failedToStartPictureInPictureWithError error: Error) {
        Task { @MainActor in
            errorMessage = "PiP failed: \(error.localizedDescription)"
            isPipActive = false
        }
    }
}

// MARK: - AVPictureInPictureSampleBufferPlaybackDelegate

extension DesktopStreamService: AVPictureInPictureSampleBufferPlaybackDelegate {
    nonisolated func pictureInPictureController(_ pictureInPictureController: AVPictureInPictureController, setPlaying playing: Bool) {
        Task { @MainActor in
            if playing {
                resume()
            } else {
                pause()
            }
        }
    }

    nonisolated func pictureInPictureControllerTimeRangeForPlayback(_ pictureInPictureController: AVPictureInPictureController) -> CMTimeRange {
        // Live stream - return a large range
        return CMTimeRange(start: .zero, duration: CMTime(value: 3600, timescale: 1))
    }

    nonisolated func pictureInPictureControllerIsPlaybackPaused(_ pictureInPictureController: AVPictureInPictureController) -> Bool {
        // This is called on the main thread, so we can safely access MainActor-isolated state
        return MainActor.assumeIsolated { isPaused }
    }

    nonisolated func pictureInPictureController(_ pictureInPictureController: AVPictureInPictureController, didTransitionToRenderSize newRenderSize: CMVideoDimensions) {
        // Handle render size change if needed
    }

    nonisolated func pictureInPictureController(_ pictureInPictureController: AVPictureInPictureController, skipByInterval skipInterval: CMTime) async {
        // Not applicable for live stream
    }
}
