//
//  FidoApprovalState.swift
//  SandboxedDashboard
//
//  Observable state for FIDO signing approval with auto-approval rules
//

import Foundation
import LocalAuthentication
import Observation

@MainActor
@Observable
final class FidoApprovalState {
    static let shared = FidoApprovalState()

    var pendingRequests: [FidoSignRequest] = []

    var autoApprovalRules: [AutoApprovalRule] = [] {
        didSet { persistRules() }
    }

    var requireBiometricForAll: Bool {
        get { UserDefaults.standard.bool(forKey: "fido_require_biometric_all") }
        set { UserDefaults.standard.set(newValue, forKey: "fido_require_biometric_all") }
    }

    private let api = APIService.shared

    private init() {
        loadRules()
    }

    // MARK: - SSE Event Handling

    func handleSignRequest(_ data: [String: Any]) {
        guard let requestId = data["request_id"] as? String,
              let keyType = data["key_type"] as? String,
              let keyFingerprint = data["key_fingerprint"] as? String,
              let origin = data["origin"] as? String,
              let expiresAtString = data["expires_at"] as? String else {
            return
        }

        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        let expiresAt = formatter.date(from: expiresAtString) ?? Date().addingTimeInterval(30)

        let request = FidoSignRequest(
            id: requestId,
            keyType: keyType,
            keyFingerprint: keyFingerprint,
            origin: origin,
            hostname: data["hostname"] as? String,
            workspace: data["workspace"] as? String,
            expiresAt: expiresAt
        )

        clearExpiredRules()

        if let rule = matchingRule(for: request) {
            if rule.requireBiometric || requireBiometricForAll {
                Task {
                    let authenticated = await authenticateWithBiometric()
                    if authenticated {
                        await approve(request.id)
                    } else {
                        pendingRequests.append(request)
                        HapticService.error()
                    }
                }
            } else {
                Task { await approve(request.id) }
            }
        } else {
            pendingRequests.append(request)
            HapticService.error()
        }
    }

    // MARK: - Approve / Deny

    func approve(_ requestId: String) async {
        do {
            try await api.fidoRespond(requestId: requestId, approved: true)
            HapticService.success()
        } catch {
            // Silently handle - request may have expired
        }
        pendingRequests.removeAll { $0.id == requestId }
    }

    func deny(_ requestId: String) async {
        do {
            try await api.fidoRespond(requestId: requestId, approved: false)
        } catch {
            // Silently handle
        }
        pendingRequests.removeAll { $0.id == requestId }
    }

    // MARK: - Auto-Approval Rules

    func addAutoApprovalRule(
        type: AutoApprovalRuleType,
        value: String?,
        duration: Int?,
        requireBiometric: Bool
    ) {
        let expiresAt: Date? = duration.map { Date().addingTimeInterval(Double($0) * 60) }
        let rule = AutoApprovalRule(
            id: UUID(),
            ruleType: type,
            value: value,
            expiresAt: expiresAt,
            requireBiometric: requireBiometric
        )
        autoApprovalRules.append(rule)
    }

    func removeRule(id: UUID) {
        autoApprovalRules.removeAll { $0.id == id }
    }

    func clearExpiredRules() {
        autoApprovalRules.removeAll { $0.isExpired }
    }

    func matchingRule(for request: FidoSignRequest) -> AutoApprovalRule? {
        autoApprovalRules.first { rule in
            guard !rule.isExpired else { return false }
            switch rule.ruleType {
            case .allSSH:
                return request.keyType.lowercased().contains("ssh")
            case .hostname:
                return rule.value == request.hostname
            case .keyFingerprint:
                return rule.value == request.keyFingerprint
            }
        }
    }

    // MARK: - Biometric Authentication

    func authenticateWithBiometric() async -> Bool {
        let context = LAContext()
        var error: NSError?

        guard context.canEvaluatePolicy(.deviceOwnerAuthenticationWithBiometrics, error: &error) else {
            return false
        }

        do {
            return try await context.evaluatePolicy(
                .deviceOwnerAuthenticationWithBiometrics,
                localizedReason: "Authenticate to approve signing request"
            )
        } catch {
            return false
        }
    }

    // MARK: - Persistence

    private func persistRules() {
        if let data = try? JSONEncoder().encode(autoApprovalRules) {
            UserDefaults.standard.set(data, forKey: "fido_auto_approval_rules")
        }
    }

    private func loadRules() {
        guard let data = UserDefaults.standard.data(forKey: "fido_auto_approval_rules"),
              let rules = try? JSONDecoder().decode([AutoApprovalRule].self, from: data) else {
            return
        }
        autoApprovalRules = rules
    }
}
