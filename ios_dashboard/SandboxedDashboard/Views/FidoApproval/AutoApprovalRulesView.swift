//
//  AutoApprovalRulesView.swift
//  SandboxedDashboard
//
//  Settings sub-view listing active FIDO auto-approval rules
//

import SwiftUI

struct AutoApprovalRulesView: View {
    private var fidoState = FidoApprovalState.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Active Rules")
                .font(.caption.weight(.medium))
                .foregroundStyle(Theme.textSecondary)

            if fidoState.autoApprovalRules.isEmpty {
                HStack {
                    Spacer()
                    VStack(spacing: 8) {
                        Image(systemName: "shield.slash")
                            .font(.title3)
                            .foregroundStyle(Theme.textMuted)
                        Text("No auto-approval rules active")
                            .font(.caption)
                            .foregroundStyle(Theme.textSecondary)
                    }
                    .padding(.vertical, 12)
                    Spacer()
                }
            } else {
                ForEach(fidoState.autoApprovalRules) { rule in
                    RuleRow(rule: rule)
                        .swipeActions(edge: .trailing) {
                            Button(role: .destructive) {
                                withAnimation {
                                    fidoState.removeRule(id: rule.id)
                                }
                            } label: {
                                Label("Delete", systemImage: "trash")
                            }
                        }
                }
            }
        }
    }
}

// MARK: - Rule Row

private struct RuleRow: View {
    let rule: AutoApprovalRule

    var body: some View {
        HStack(spacing: 12) {
            // Rule type icon
            ZStack {
                Circle()
                    .fill(Theme.accent.opacity(0.15))
                    .frame(width: 32, height: 32)

                Image(systemName: rule.ruleType.icon)
                    .font(.system(size: 13, weight: .medium))
                    .foregroundStyle(Theme.accent)
            }

            // Description
            VStack(alignment: .leading, spacing: 2) {
                Text(rule.displayDescription)
                    .font(.subheadline)
                    .foregroundStyle(Theme.textPrimary)
                    .lineLimit(1)

                if rule.requireBiometric {
                    HStack(spacing: 4) {
                        Image(systemName: "faceid")
                            .font(.system(size: 9))
                        Text("Requires Face ID")
                            .font(.caption2)
                    }
                    .foregroundStyle(Theme.textSecondary)
                }
            }

            Spacer()

            // Time remaining badge
            if let timeRemaining = rule.timeRemaining {
                Text(timeRemaining)
                    .font(.caption2.weight(.medium))
                    .foregroundStyle(rule.isExpired ? Theme.error : Theme.textSecondary)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 4)
                    .background(
                        Capsule()
                            .fill(rule.isExpired ? Theme.error.opacity(0.15) : Color.white.opacity(0.05))
                    )
            }
        }
        .padding(.vertical, 4)
    }
}

#Preview {
    ZStack {
        Theme.backgroundPrimary.ignoresSafeArea()
        GlassCard {
            AutoApprovalRulesView()
        }
        .padding()
    }
}
