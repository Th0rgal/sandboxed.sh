import SwiftUI

/// One project in full: the controller-reported state, the cron controller that
/// drives it, its bound Hermes session (tap to open it in Control), the
/// autonomy grant, tracks, live mission-agents, and any open decisions.
///
/// This is where "which controller drives this project, and which session is it
/// tied to" is answered — the link the switcher never surfaced.
struct ProjectDetailView: View {
    let slug: String
    /// The overview row we came from, used as an instant fallback while the
    /// detail loads (and for the live mission chips the detail endpoint omits).
    let summary: ProjectSummary?

    private let api = APIService.shared
    private let nav = NavigationState.shared

    @State private var detail: ProjectDetail?
    @State private var isLoading = true
    @State private var loadError: String?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                header
                controllerSection
                if let next = record?.nextAction, !next.isEmpty {
                    infoCard(title: "Next action", body: next, icon: "arrow.right.circle")
                }
                if let blocker = record?.blocker, !blocker.isEmpty {
                    infoCard(title: "Blocked on", body: blocker, icon: "exclamationmark.triangle", tint: Theme.warning)
                }
                grantSection
                missionsSection
                tracksSection
                decisionsSection
            }
            .padding(16)
        }
        .background(Theme.backgroundPrimary)
        .navigationTitle(record?.displayTitle ?? slug)
        .navigationBarTitleDisplayMode(.inline)
        .task { await load() }
        .refreshable { await load() }
    }

    private var record: ProjectRecord? { detail?.project }

    // MARK: header

    private var header: some View {
        GlassCard {
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 8) {
                    Text(record?.displayTitle ?? slug)
                        .font(.system(size: 18, weight: .bold))
                        .foregroundStyle(Theme.textPrimary)
                    if let mode = record?.controllerMode ?? summary?.mode {
                        ModeChipView(mode: mode)
                        if mode.base == .blocked, let wait = record?.waitTicks, wait > 0 {
                            Text("· \(wait) tick\(wait == 1 ? "" : "s")")
                                .font(.system(size: 11))
                                .foregroundStyle(Theme.textTertiary)
                        }
                    }
                }
                if let objective = record?.objective, !objective.isEmpty {
                    Text(objective)
                        .font(.system(size: 13))
                        .foregroundStyle(Theme.textSecondary)
                }
                if let repo = record?.repository, !repo.isEmpty {
                    Label(repo, systemImage: "chevron.left.forwardslash.chevron.right")
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.textTertiary)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    // MARK: controller + bound session

    private var controllerSection: some View {
        GlassCard {
            VStack(alignment: .leading, spacing: 10) {
                sectionTitle("Controller", icon: "gearshape.2")
                if let cron = record?.controllerCronId, !cron.isEmpty {
                    labeledRow("Cron", value: cron, mono: true)
                } else {
                    Text("No controller drives this project.")
                        .font(.system(size: 12))
                        .foregroundStyle(Theme.textTertiary)
                }
                if let session = detail?.boundSessionId ?? summary?.boundSessionId {
                    Button {
                        nav.openHermesSession(session)
                    } label: {
                        HStack {
                            Label("Open control conversation", systemImage: "bubble.left.and.text.bubble.right")
                                .font(.system(size: 13, weight: .medium))
                            Spacer()
                            Image(systemName: "chevron.right").font(.system(size: 11))
                        }
                        .foregroundStyle(Theme.accent)
                    }
                } else {
                    Text("No control conversation bound.")
                        .font(.system(size: 12))
                        .foregroundStyle(Theme.textTertiary)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    // MARK: grant

    @ViewBuilder private var grantSection: some View {
        if let grant = detail?.grant, hasGrant(grant) {
            GlassCard {
                VStack(alignment: .leading, spacing: 8) {
                    sectionTitle("Autonomy grant", icon: "checkmark.seal")
                    if let m = grant.mergeAuthority { labeledRow("Merge", value: m) }
                    if let b = grant.budgetPerTick { labeledRow("Budget", value: b) }
                    if let bar = grant.materialBar { labeledRow("Material", value: bar) }
                    if let reason = grant.pauseReason {
                        labeledRow("Paused", value: reason, tint: Theme.warning)
                    }
                    if let resume = grant.resumeCondition {
                        labeledRow("Resume when", value: resume)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    private func hasGrant(_ g: ProjectGrant) -> Bool {
        g.mergeAuthority != nil || g.budgetPerTick != nil || g.materialBar != nil
            || g.pauseReason != nil || g.resumeCondition != nil
    }

    // MARK: missions (the agents)

    @ViewBuilder private var missionsSection: some View {
        let missions = summary?.missions ?? []
        if !missions.isEmpty {
            GlassCard {
                VStack(alignment: .leading, spacing: 8) {
                    sectionTitle("Agents", icon: "cpu")
                    ForEach(missions) { m in
                        HStack(spacing: 8) {
                            Circle()
                                .fill(missionColor(m.status))
                                .frame(width: 6, height: 6)
                            Text(m.displayTitle)
                                .font(.system(size: 12))
                                .foregroundStyle(Theme.textSecondary)
                                .lineLimit(1)
                            Spacer(minLength: 4)
                            if m.needsAttention {
                                Text("NEEDS YOU")
                                    .font(.system(size: 9, weight: .semibold))
                                    .foregroundStyle(Theme.warning)
                            }
                            if let pr = m.githubPr {
                                Text(pr).font(.system(size: 10)).foregroundStyle(Theme.textTertiary)
                            }
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    private func missionColor(_ status: String) -> Color {
        switch status {
        case "active": return Theme.accent
        case "awaiting_user": return Theme.warning
        case "failed", "interrupted": return Theme.error
        default: return Theme.textMuted
        }
    }

    // MARK: tracks

    @ViewBuilder private var tracksSection: some View {
        if let tracks = detail?.tracks, !tracks.isEmpty {
            GlassCard {
                VStack(alignment: .leading, spacing: 8) {
                    sectionTitle("Tracks", icon: "point.3.connected.trianglepath.dotted")
                    ForEach(tracks) { t in
                        HStack {
                            Text(t.track).font(.system(size: 12)).foregroundStyle(Theme.textSecondary)
                            Spacer()
                            if let status = t.status {
                                Text(status).font(.system(size: 11)).foregroundStyle(Theme.textTertiary)
                            }
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    // MARK: decisions

    @ViewBuilder private var decisionsSection: some View {
        if let decisions = detail?.openDecisions, !decisions.isEmpty {
            GlassCard {
                VStack(alignment: .leading, spacing: 8) {
                    sectionTitle("Open questions", icon: "questionmark.bubble")
                    ForEach(decisions) { d in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(d.question).font(.system(size: 12)).foregroundStyle(Theme.textPrimary)
                            if let r = d.rationale {
                                Text(r).font(.system(size: 11)).foregroundStyle(Theme.textTertiary)
                            }
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    // MARK: helpers

    private func sectionTitle(_ text: String, icon: String) -> some View {
        Label(text, systemImage: icon)
            .font(.system(size: 12, weight: .semibold))
            .foregroundStyle(Theme.textTertiary)
    }

    private func labeledRow(_ label: String, value: String, mono: Bool = false, tint: Color? = nil) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Text(label)
                .font(.system(size: 12, weight: .medium))
                .foregroundStyle(Theme.textTertiary)
                .frame(width: 76, alignment: .leading)
            Text(value)
                .font(.system(size: 12, design: mono ? .monospaced : .default))
                .foregroundStyle(tint ?? Theme.textSecondary)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func infoCard(title: String, body: String, icon: String, tint: Color? = nil) -> some View {
        GlassCard {
            VStack(alignment: .leading, spacing: 6) {
                sectionTitle(title, icon: icon)
                Text(body)
                    .font(.system(size: 13))
                    .foregroundStyle(tint ?? Theme.textPrimary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func load() async {
        isLoading = true
        defer { isLoading = false }
        do {
            detail = try await api.getProject(slug: slug)
            loadError = nil
        } catch {
            loadError = error.localizedDescription
        }
    }
}
