import SwiftUI

/// The projects surface: every sandboxed.sh project as a card showing its
/// controller-reported mode, its live missions, and — through the detail
/// view — the controller that drives it and the control conversation bound
/// to it.
///
/// Unlike the mission-switcher's project rows, this does NOT hide projects
/// without a bound conversation: a paused or freshly-seeded project is still
/// a project worth seeing.
struct ProjectsView: View {
    private let api = APIService.shared
    private let unreadStore = ProjectUnreadStore.shared

    @State private var projects: [ProjectSummary] = []
    /// Unread deliveries per slug, recomputed on load and whenever the list
    /// reappears (coming back from a detail view clears that project's badge).
    @State private var unread: [String: Int] = [:]
    @State private var isLoading = true
    @State private var loadError: String?

    var body: some View {
        Group {
            if isLoading && projects.isEmpty {
                LoadingView()
            } else if let loadError, projects.isEmpty {
                ContentUnavailableView("Projects unavailable", systemImage: "square.stack.3d.up.slash", description: Text(loadError))
            } else if projects.isEmpty {
                ContentUnavailableView("No projects", systemImage: "square.stack.3d.up", description: Text("No sandboxed.sh projects on this backend."))
            } else {
                content
            }
        }
        .navigationTitle("Projects")
        .navigationBarTitleDisplayMode(.inline)
        .task { await load() }
        .refreshable { await load() }
    }

    private var summaryLine: String {
        let live = projects.reduce(0) { $0 + $1.liveMissions.count }
        let blocked = projects.filter { $0.mode?.base == .blocked }.count
        let attention = projects.filter { $0.needsAttention }.count
        var parts: [String] = []
        if live > 0 { parts.append("\(live) live") }
        if attention > 0 { parts.append("\(attention) need attention") }
        if blocked > 0 { parts.append("\(blocked) blocked") }
        return parts.joined(separator: " · ")
    }

    private var sortedProjects: [ProjectSummary] {
        // Attention first, then blocked, then the rest — the stuck one is what
        // you open the app to find.
        projects.sorted { a, b in
            func rank(_ p: ProjectSummary) -> Int {
                if p.needsAttention { return 0 }
                if p.mode?.base == .blocked { return 1 }
                if p.bucket == "paused" || p.mode?.base == .paused { return 3 }
                return 2
            }
            let ra = rank(a), rb = rank(b)
            return ra == rb ? a.slug < b.slug : ra < rb
        }
    }

    private var content: some View {
        ScrollView {
            LazyVStack(spacing: 12) {
                if !summaryLine.isEmpty {
                    Text(summaryLine)
                        .font(.system(size: 12))
                        .foregroundStyle(Theme.textTertiary)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, 4)
                }
                ForEach(sortedProjects) { project in
                    NavigationLink {
                        ProjectDetailView(slug: project.slug, summary: project)
                    } label: {
                        ProjectCard(project: project, unread: unread[project.slug] ?? 0)
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(16)
        }
        .background(Theme.backgroundPrimary)
        .onAppear { refreshUnread() }
    }

    private func refreshUnread() {
        unread = Dictionary(
            projects.map { ($0.slug, unreadStore.unreadCount(for: $0)) },
            uniquingKeysWith: { first, _ in first }
        )
    }

    private func load() async {
        if projects.isEmpty { isLoading = true }
        defer { isLoading = false }
        do {
            projects = try await api.listProjects()
            loadError = nil
            refreshUnread()
        } catch {
            loadError = error.localizedDescription
        }
    }
}

/// One project as a card: title/slug, mode chip, live-mission count, unread
/// deliveries, and the single most useful line (attention reason, blocker,
/// or headline).
struct ProjectCard: View {
    let project: ProjectSummary
    /// New deliveries since the project was last opened; 0 hides the badge.
    var unread: Int = 0

    var body: some View {
        GlassCard {
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 8) {
                    Text(project.displayName)
                        .font(.system(size: 15, weight: .semibold))
                        .foregroundStyle(Theme.textPrimary)
                        .lineLimit(1)
                    if let mode = project.mode {
                        ModeChipView(mode: mode)
                    }
                    Spacer(minLength: 4)
                    if unread > 0 {
                        Text(unread > 9 ? "9+" : "\(unread)")
                            .font(.system(size: 10, weight: .semibold))
                            .foregroundStyle(Theme.accent)
                            .padding(.horizontal, 6)
                            .padding(.vertical, 2)
                            .background(Theme.accent.opacity(0.15), in: Capsule())
                            .accessibilityLabel("\(unread > 9 ? "9 or more" : "\(unread)") new updates")
                    }
                    if !project.liveMissions.isEmpty {
                        Label("\(project.liveMissions.count)", systemImage: "circle.fill")
                            .labelStyle(.titleAndIcon)
                            .font(.system(size: 11, weight: .medium))
                            .foregroundStyle(Theme.accent)
                    }
                }
                let attention = project.missionsNeedingAttention.count
                if attention > 0 {
                    Label("\(attention) need you", systemImage: "exclamationmark.circle.fill")
                        .font(.system(size: 12, weight: .medium))
                        .foregroundStyle(Theme.warning)
                }
                if let sub = subtitle {
                    Text(sub)
                        .font(.system(size: 12))
                        .foregroundStyle(Theme.textSecondary)
                        .lineLimit(2)
                        .multilineTextAlignment(.leading)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private var subtitle: String? {
        if let blocker = project.latestUpdate?.blocker, !blocker.isEmpty {
            return "Blocked: \(blocker)"
        }
        if let reason = project.attentionReasons.first { return reason }
        return project.latestUpdate?.headline
    }
}

/// The controller mode as a small colored chip. Amber = look at this; indigo =
/// working; dim = deliberately paused.
struct ModeChipView: View {
    let mode: ControllerMode

    private var color: Color {
        switch mode.base {
        case .blocked: return Theme.warning
        case .active: return Theme.accent
        case .paused: return Theme.textMuted
        }
    }

    var body: some View {
        HStack(spacing: 3) {
            Circle().fill(color).frame(width: 5, height: 5)
            Text(mode.label.uppercased())
                .font(.system(size: 9, weight: .semibold))
                .foregroundStyle(color)
        }
        .padding(.horizontal, 6)
        .padding(.vertical, 2)
        .background(color.opacity(0.12), in: Capsule())
    }
}
