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
                    initialFreshSession: .keep,
                    initialNextSessionId: "",
                    onSave: { command, trigger, freshSession, nextSessionId in
                        await createAutomation(
                            command: command,
                            trigger: trigger,
                            freshSession: freshSession,
                            nextSessionId: nextSessionId
                        )
                    }
                )
            }
            .sheet(item: $editingAutomation) { automation in
                AutomationEditorSheet(
                    title: "Edit Automation",
                    initialCommand: automation.commandText,
                    initialTrigger: automation.trigger,
                    initialFreshSession: automation.freshSession ?? .keep,
                    initialNextSessionId: automation.variables["nextSessionId"] ?? "",
                    onSave: { command, trigger, freshSession, nextSessionId in
                        await updateAutomation(
                            automation,
                            command: command,
                            trigger: trigger,
                            freshSession: freshSession,
                            nextSessionId: nextSessionId
                        )
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
        // Flip the toggle instantly so the switch animates in the same
        // frame the user tapped. On a slow connection the previous
        // version left the switch in its old position until the network
        // call returned, making the tap feel ignored.
        let originalActive = automation.active
        if let index = automations.firstIndex(where: { $0.id == automation.id }) {
            automations[index].active = active
        }
        HapticService.selectionChanged()
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
        } catch {
            errorMessage = error.localizedDescription
            // Roll back so the toggle reflects the server's true state.
            if let index = automations.firstIndex(where: { $0.id == automation.id }) {
                automations[index].active = originalActive
            }
            HapticService.error()
        }
    }

    private func createAutomation(
        command: String,
        trigger: AutomationTrigger,
        freshSession: AutomationFreshSession,
        nextSessionId: String
    ) async {
        guard let missionId else { return }
        let trimmedNextSessionId = nextSessionId.trimmingCharacters(in: .whitespacesAndNewlines)
        var variables: [String: String] = [:]
        if freshSession == .switchSession {
            variables["nextSessionId"] = trimmedNextSessionId
        }

        do {
            let created = try await api.createMissionAutomation(
                missionId: missionId,
                request: CreateAutomationRequest(
                    commandSource: .inline(content: command),
                    trigger: trigger,
                    variables: variables,
                    startImmediately: false,
                    freshSession: freshSession
                )
            )
            automations.insert(created, at: 0)
            showCreateSheet = false
            HapticService.success()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    private func updateAutomation(
        _ automation: Automation,
        command: String,
        trigger: AutomationTrigger,
        freshSession: AutomationFreshSession,
        nextSessionId: String
    ) async {
        let trimmedNextSessionId = nextSessionId.trimmingCharacters(in: .whitespacesAndNewlines)
        var updatedVariables = automation.variables
        if freshSession == .switchSession {
            updatedVariables["nextSessionId"] = trimmedNextSessionId
        } else {
            updatedVariables.removeValue(forKey: "nextSessionId")
        }

        do {
            let updated = try await api.updateAutomation(
                id: automation.id,
                request: UpdateAutomationRequest(
                    commandSource: .inline(content: command),
                    trigger: trigger,
                    variables: updatedVariables,
                    active: nil,
                    freshSession: freshSession
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
        // Remove the row instantly so the list shrinks in the same frame
        // as the Delete tap. Re-insert on failure.
        let originalIndex = automations.firstIndex(where: { $0.id == automation.id })
        withAnimation {
            automations.removeAll { $0.id == automation.id }
        }
        do {
            try await api.deleteAutomation(id: automation.id)
            HapticService.success()
        } catch {
            errorMessage = error.localizedDescription
            if let idx = originalIndex {
                withAnimation {
                    automations.insert(automation, at: min(idx, automations.count))
                }
            }
            HapticService.error()
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

            if let sessionModeLabel = automation.sessionModeLabel {
                Text(sessionModeLabel)
                    .font(.caption2)
                    .foregroundStyle(Theme.textMuted)
            }

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
    let initialFreshSession: AutomationFreshSession
    let initialNextSessionId: String
    let onSave: (String, AutomationTrigger, AutomationFreshSession, String) async -> Void

    @Environment(\.dismiss) private var dismiss

    @State private var command = ""
    @State private var triggerKind: TriggerKind = .interval
    @State private var intervalSeconds = 300
    @State private var freshSessionKind: FreshSessionKind = .keep
    @State private var nextSessionId = ""
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

    enum FreshSessionKind: String, CaseIterable, Identifiable {
        case keep
        case always
        case switchSession = "switch"

        var id: String { rawValue }

        var label: String {
            switch self {
            case .keep:
                return "Keep"
            case .always:
                return "Always New"
            case .switchSession:
                return "Switch"
            }
        }

        var value: AutomationFreshSession {
            switch self {
            case .always:
                return .always
            case .keep:
                return .keep
            case .switchSession:
                return .switchSession
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

                Section("Session") {
                    Picker("Mode", selection: $freshSessionKind) {
                        ForEach(FreshSessionKind.allCases) { kind in
                            Text(kind.label).tag(kind)
                        }
                    }

                    if freshSessionKind == .switchSession {
                        TextField("Next session ID", text: $nextSessionId)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
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
                            await onSave(
                                command.trimmingCharacters(in: .whitespacesAndNewlines),
                                selectedTrigger,
                                freshSessionKind.value,
                                nextSessionId.trimmingCharacters(in: .whitespacesAndNewlines)
                            )
                            isSaving = false
                        }
                    }
                    .disabled(
                        command.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ||
                        isSaving ||
                        (freshSessionKind == .switchSession &&
                         nextSessionId.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    )
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
                freshSessionKind = FreshSessionKind(rawValue: initialFreshSession.rawValue) ?? .keep
                nextSessionId = initialNextSessionId
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

    var sessionModeLabel: String? {
        guard let freshSession else { return nil }
        switch freshSession {
        case .keep:
            return "Session: Keep current"
        case .always:
            return "Session: Always new"
        case .switchSession:
            let nextSessionId = variables["nextSessionId"] ?? "(missing nextSessionId)"
            return "Session: Switch to \(nextSessionId)"
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
