//
//  FidoSignRequest.swift
//  SandboxedDashboard
//
//  Models for FIDO signing approval requests and auto-approval rules
//

import Foundation

struct FidoSignRequest: Identifiable, Codable, Equatable {
    let id: String
    let keyType: String
    let keyFingerprint: String
    let origin: String
    let hostname: String?
    let workspace: String?
    let expiresAt: Date

    enum CodingKeys: String, CodingKey {
        case id = "request_id"
        case keyType = "key_type"
        case keyFingerprint = "key_fingerprint"
        case origin, hostname, workspace
        case expiresAt = "expires_at"
    }
}

enum AutoApprovalRuleType: String, Codable, CaseIterable {
    case allSSH = "all_ssh"
    case hostname = "hostname"
    case keyFingerprint = "key_fingerprint"

    var displayName: String {
        switch self {
        case .allSSH: return "All SSH"
        case .hostname: return "Hostname"
        case .keyFingerprint: return "Key"
        }
    }

    var icon: String {
        switch self {
        case .allSSH: return "terminal"
        case .hostname: return "network"
        case .keyFingerprint: return "key"
        }
    }
}

struct AutoApprovalRule: Identifiable, Codable, Equatable {
    let id: UUID
    let ruleType: AutoApprovalRuleType
    let value: String?
    let expiresAt: Date?
    let requireBiometric: Bool

    var isExpired: Bool {
        if let expiresAt { return expiresAt < Date() }
        return false
    }

    var displayDescription: String {
        switch ruleType {
        case .allSSH:
            return "All SSH requests"
        case .hostname:
            return value ?? "Unknown host"
        case .keyFingerprint:
            let short = (value ?? "").prefix(16)
            return "Key \(short)..."
        }
    }

    var timeRemaining: String? {
        guard let expiresAt else { return "Permanent" }
        let remaining = expiresAt.timeIntervalSinceNow
        if remaining <= 0 { return "Expired" }
        let minutes = Int(remaining / 60)
        if minutes < 1 { return "\(Int(remaining))s left" }
        return "\(minutes)m left"
    }
}
