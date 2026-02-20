//
//  AutomationsView.swift
//  SandboxedDashboard
//
//  Mission automations management (create/edit/stop/delete).
//

import SwiftUI

struct AutomationsView: View {
    let missionId: String?

    @Environment(\.dismiss) private var dismiss

    @State private var automations: [Automation] = []
    @State private var isLoading = false
    @State private var errorMessage: String?

    @State private var showCreateSheet = false
    @State private var editingAutomation: Automation?

    private let api = APIService.shared

    var body: some View {
        NavigationStack {
            Group {
                if let missionId {
                    if isLoading && automations.isEmpty {
                        ProgressView("Loading automations...")
                            .tint(Theme.accent)
                    } else if automations.isEmpty {
                        ContentUnavailableView(
                            "No Automations",
                            systemImage: "bolt.slash",
                            description: Text("Create automations to run tasks automatically for this mission.")
                        )
                    } else {
                        List {
                            Section("Mission \(String(missionId.prefix(8)).uppercased())") {
                                ForEach(automations) { automation in
                                    AutomationRow(
                                        automation: automation,
                                        onToggleActive: { active in
                                            Task { await setAutomationActive(automation, active: active) }
                                        },
                                        onEdit: {
                                            guard automation.commandSource.isInline,
                                                  automation.trigger.isEditableInIOS else { return }
                                            editingAutomation = automation
                                        },
                                        onDelete: {
                                            Task { await deleteAutomation(automation) }
                                        }
                                    )
                                }
                            }
                        }
                        .listStyle(.insetGrouped)
                        .refreshable {
                            await loadAutomations()
                        }
                    }
                } else {
                    ContentUnavailableView(
                        "No Mission Selected",
                        systemImage: "square.stack.3d.up.slash",
                        description: Text("Open or create a mission first, then manage its automations here.")
                    )
                }
            }
            .navigationTitle("Automations")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button("Done") { dismiss() }
                }

                ToolbarItem(placement: .topBarTrailing) {
                    Button {
                        showCreateSheet = true
                    } label: {
                        Label("New", systemImage: "plus")
                    }
                    .disabled(missionId == nil)
                }
            }
            .alert("Automation Error", isPresented: Binding(
                get: { errorMessage != nil },
                set: { if !$0 { errorMessage = nil } }
            )) {
                Button("OK", role: .cancel) { errorMessage = nil }
            } message: {
                Text(errorMessage ?? "Unknown error")
            }
            .task {
                await loadAutomations()
            }
            .sheet(isPresented: $showCreateSheet) {
                AutomationEditorSheet(
                    title: "New Automation",
                    initialCommand: "",
                    initialTrigger: .interval(seconds: 300),
                    onSave: { command, trigger in
                        await createAutomation(command: command, trigger: trigger)
                    }
                )
            }
            .sheet(item: $editingAutomation) { automation in
                AutomationEditorSheet(
                    title: "Edit Automation",
                    initialCommand: automation.commandText,
                    initialTrigger: automation.trigger,
                    onSave: { command, trigger in
                        await updateAutomation(automation, command: command, trigger: trigger)
                    }
                )
            }
        }
    }

    private func loadAutomations() async {
        guard let missionId else { return }
        isLoading = true
        defer { isLoading = false }

        do {
            automations = try await api.listMissionAutomations(missionId: missionId)
                .sorted { $0.createdAt > $1.createdAt }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    private func setAutomationActive(_ automation: Automation, active: Bool) async {
        do {
            _ = try await api.updateAutomation(
                id: automation.id,
                request: UpdateAutomationRequest(
                    commandSource: nil,
                    trigger: nil,
                    variables: nil,
                    active: active
                )
            )
            if let index = automations.firstIndex(where: { $0.id == automation.id }) {
                automations[index].active = active
            }
            HapticService.selectionChanged()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    private func createAutomation(command: String, trigger: AutomationTrigger) async {
        guard let missionId else { return }
        do {
            let created = try await api.createMissionAutomation(
                missionId: missionId,
                request: CreateAutomationRequest(
                    commandSource: .inline(content: command),
                    trigger: trigger,
                    variables: [:],
                    startImmediately: false
                )
            )
            automations.insert(created, at: 0)
            showCreateSheet = false
            HapticService.success()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    private func updateAutomation(_ automation: Automation, command: String, trigger: AutomationTrigger) async {
        do {
            let updated = try await api.updateAutomation(
                id: automation.id,
                request: UpdateAutomationRequest(
                    commandSource: .inline(content: command),
                    trigger: trigger,
                    variables: nil,
                    active: nil
                )
            )
            if let index = automations.firstIndex(where: { $0.id == automation.id }) {
                automations[index] = updated
            }
            editingAutomation = nil
            HapticService.success()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    private func deleteAutomation(_ automation: Automation) async {
        do {
            try await api.deleteAutomation(id: automation.id)
            automations.removeAll { $0.id == automation.id }
            HapticService.success()
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}

private struct AutomationRow: View {
    let automation: Automation
    let onToggleActive: (Bool) -> Void
    let onEdit: () -> Void
    let onDelete: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Image(systemName: automation.active ? "bolt.fill" : "bolt.slash")
                    .font(.subheadline)
                    .foregroundStyle(automation.active ? Theme.success : Theme.textMuted)

                Text(automation.triggerLabel)
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(Theme.textPrimary)

                Spacer()

                Toggle("", isOn: Binding(
                    get: { automation.active },
                    set: { onToggleActive($0) }
                ))
                .labelsHidden()
            }

            Text(automation.commandPreview)
                .font(.caption)
                .foregroundStyle(Theme.textSecondary)
                .lineLimit(2)

            HStack {
                if automation.commandSource.isInline && automation.trigger.isEditableInIOS {
                    Button("Edit") { onEdit() }
                        .font(.caption.weight(.medium))
                } else if automation.commandSource.isInline {
                    Text("Webhook trigger editing coming soon")
                        .font(.caption2)
                        .foregroundStyle(Theme.textMuted)
                } else {
                    Text("Non-inline command")
                        .font(.caption2)
                        .foregroundStyle(Theme.textMuted)
                }

                Spacer()

                Button("Delete", role: .destructive) { onDelete() }
                    .font(.caption.weight(.medium))
            }
            .buttonStyle(.plain)
        }
        .padding(.vertical, 4)
    }
}

private struct AutomationEditorSheet: View {
    let title: String
    let initialCommand: String
    let initialTrigger: AutomationTrigger
    let onSave: (String, AutomationTrigger) async -> Void

    @Environment(\.dismiss) private var dismiss

    @State private var command = ""
    @State private var triggerKind: TriggerKind = .interval
    @State private var intervalSeconds = 300
    @State private var isSaving = false

    enum TriggerKind: String, CaseIterable, Identifiable {
        case interval
        case agentFinished = "agent_finished"

        var id: String { rawValue }

        var label: String {
            switch self {
            case .interval:
                return "Interval"
            case .agentFinished:
                return "After Turn"
            }
        }
    }

    var body: some View {
        NavigationStack {
            Form {
                Section("Command") {
                    TextEditor(text: $command)
                        .frame(minHeight: 120)
                        .font(.system(.body, design: .monospaced))
                }

                Section("Trigger") {
                    Picker("Type", selection: $triggerKind) {
                        ForEach(TriggerKind.allCases) { kind in
                            Text(kind.label).tag(kind)
                        }
                    }
                    .pickerStyle(.segmented)

                    if triggerKind == .interval {
                        Stepper(value: $intervalSeconds, in: 30...86_400, step: 30) {
                            Text("Every \(intervalDescription)")
                        }
                    }
                }
            }
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button("Cancel") { dismiss() }
                        .disabled(isSaving)
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Save") {
                        Task {
                            isSaving = true
                            await onSave(command.trimmingCharacters(in: .whitespacesAndNewlines), selectedTrigger)
                            isSaving = false
                        }
                    }
                    .disabled(command.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || isSaving)
                }
            }
            .onAppear {
                command = initialCommand
                switch initialTrigger {
                case .interval(let seconds):
                    triggerKind = .interval
                    intervalSeconds = max(30, seconds)
                case .agentFinished:
                    triggerKind = .agentFinished
                case .webhook:
                    triggerKind = .interval
                }
            }
        }
    }

    private var selectedTrigger: AutomationTrigger {
        switch triggerKind {
        case .interval:
            return .interval(seconds: intervalSeconds)
        case .agentFinished:
            return .agentFinished
        }
    }

    private var intervalDescription: String {
        if intervalSeconds % 60 == 0 {
            return "\(intervalSeconds / 60)m"
        }
        return "\(intervalSeconds)s"
    }
}

private extension Automation {
    var commandText: String {
        switch commandSource {
        case .inline(let content):
            return content
        case .library(let name):
            return "<library:\(name)>"
        case .localFile(let path):
            return "<file:\(path)>"
        }
    }
}

private extension AutomationCommandSource {
    var isInline: Bool {
        if case .inline = self {
            return true
        }
        return false
    }
}

private extension AutomationTrigger {
    var isEditableInIOS: Bool {
        switch self {
        case .interval, .agentFinished:
            return true
        case .webhook:
            return false
        }
    }
}
