import SwiftUI
import UIKit

enum OrbStyle {
    static let background = Color(white: 0.073)
    static let surface = Color(white: 25 / 255)
    static let card = Color(white: 0.125)
    static let elevated = Color(white: 0.155)
    static let icon = Color(red: 138 / 255, green: 138 / 255, blue: 138 / 255)
    static let textSecondary = Color(red: 155 / 255, green: 155 / 255, blue: 155 / 255)
    static let textMuted = Color(red: 108 / 255, green: 108 / 255, blue: 108 / 255)
    static let border = Color.white.opacity(0.08)
    static let borderStrong = Color(white: 51 / 255)
    static let controlRadius: CGFloat = 6
    static let panelRadius: CGFloat = 12
    static let success = Color(red: 115 / 255, green: 201 / 255, blue: 145 / 255)
    static let warning = Color(red: 214 / 255, green: 161 / 255, blue: 106 / 255)
    static let error = Color(red: 235 / 255, green: 111 / 255, blue: 111 / 255)

    static func serviceName(_ value: String) -> String {
        [
            "claudecode": "Claude Code",
            "codex": "Codex",
            "opencode": "OpenCode",
            "gemini": "Gemini",
            "antigravity": "Antigravity",
            "amp": "Amp",
            "cloud_chatgpt": "ChatGPT",
            "cloud_cursor": "Cursor Cloud",
            "cloud_cursor_cloud": "Cursor Cloud",
            "cloud_grok_bot": "Grok Bot",
            "cloud_hermes": "Hermes",
        ][value] ?? value
    }

    static func statusLabel(_ state: String) -> String {
        switch state {
        case "active", "running", "starting": return "Working"
        case "pending", "queued": return "Queued"
        case "completed", "succeeded": return "Completed"
        case "awaiting_user", "waiting_user": return "Waiting for reply"
        case "blocked": return "Blocked"
        case "failed": return "Failed"
        case "interrupted", "cancelled": return "Stopped"
        case "acknowledged": return "Archived"
        default: return state.replacingOccurrences(of: "_", with: " ")
        }
    }

    nonisolated(unsafe) private static let isoFractionalFormatter: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f
    }()
    nonisolated(unsafe) private static let isoFallbackFormatter = ISO8601DateFormatter()

    static func relativeTime(_ raw: String, now: Date = Date()) -> String {
        guard !raw.isEmpty else { return "" }
        guard let date = isoFractionalFormatter.date(from: raw) ?? isoFallbackFormatter.date(from: raw) else { return "" }
        let delta = max(0, Int(now.timeIntervalSince(date)))
        if delta < 45 { return "now" }
        if delta < 3600 { return "\(delta / 60)m" }
        if delta < 86_400 { return "\(delta / 3600)h" }
        if delta < 604_800 { return "\(delta / 86_400)d" }
        return "\(delta / 604_800)w"
    }

    static func goalObjective(_ text: String) -> String? {
        let rest = text.drop(while: { $0.isWhitespace })
        guard rest.hasPrefix("/goal") else { return nil }
        let after = rest.dropFirst(5)
        if !after.isEmpty && !(after.first?.isWhitespace ?? false) { return nil }
        let objective = after.trimmingCharacters(in: .whitespacesAndNewlines)
        return objective.isEmpty ? nil : objective
    }

    static func planObjective(_ text: String) -> String? {
        let rest = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard rest.hasPrefix("/plan") else { return nil }
        let after = rest.dropFirst(5)
        if !after.isEmpty && !(after.first?.isWhitespace ?? false) { return nil }
        return after.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    static func displayTitle(_ title: String) -> String {
        guard !title.isEmpty else { return title }
        if let g = goalObjective(title), !g.isEmpty { return g }
        if let p = planObjective(title), !p.isEmpty { return p }
        return title
    }

    static func missionTitle(_ text: String) -> String {
        let base = goalObjective(text) ?? planObjective(text).flatMap({ $0.isEmpty ? nil : $0 }) ?? text
        let line = base.trimmingCharacters(in: .whitespacesAndNewlines).components(separatedBy: .newlines).first?.trimmingCharacters(in: .whitespaces) ?? ""
        if line.count > 42 {
            return String(line.prefix(41)).trimmingCharacters(in: .whitespaces) + "…"
        }
        return line.isEmpty ? String(text.prefix(42)) : line
    }
}

@MainActor
enum OrbHaptics {
    static func selection() { UISelectionFeedbackGenerator().selectionChanged() }
    static func light() { UIImpactFeedbackGenerator(style: .light).impactOccurred() }
    static func success() { UINotificationFeedbackGenerator().notificationOccurred(.success) }
}

/// Tactile press scale effect for interactive rows and buttons.
struct OrbPressButtonStyle: ButtonStyle {
    var scale: CGFloat = 0.985
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .scaleEffect(configuration.isPressed ? scale : 1.0)
            .opacity(configuration.isPressed ? 0.88 : 1.0)
            .animation(.spring(response: 0.22, dampingFraction: 0.78), value: configuration.isPressed)
    }
}

/// Cursor-style 3×3 animated dot matrix for running agents and tool folds.
struct OrbRunningDots: View {
    var size: CGFloat = 12
    private static let delays: [Double] = [0.0, 0.15, 0.30, 0.20, 0.35, 0.50, 0.40, 0.55, 0.70]
    var body: some View {
        TimelineView(.animation(minimumInterval: 0.12)) { context in
            let t = context.date.timeIntervalSinceReferenceDate
            let dotSize = max(1.8, size / 5.2)
            let step = (size - dotSize) / 2
            Canvas { ctx, _ in
                for index in 0..<9 {
                    let row = CGFloat(index / 3)
                    let col = CGFloat(index % 3)
                    let phase = (t / 1.2 + Self.delays[index]).truncatingRemainder(dividingBy: 1.0)
                    let wave = 0.5 - 0.5 * cos(phase * 2 * .pi)
                    let alpha = 0.18 + 0.72 * wave
                    let rect = CGRect(x: col * step, y: row * step, width: dotSize, height: dotSize)
                    ctx.fill(Path(ellipseIn: rect), with: .color(.white.opacity(alpha)))
                }
            }
            .frame(width: size, height: size)
        }
        .accessibilityHidden(true)
    }
}

/// Subtle metallic sweep matching Cursor's `.ui-collapsible-shimmer` and Orb desktop `.work-shimmer`.
struct OrbShimmerModifier: ViewModifier {
    let active: Bool
    func body(content: Content) -> some View {
        if active {
            TimelineView(.animation(minimumInterval: 0.05)) { context in
                let phase = CGFloat((context.date.timeIntervalSinceReferenceDate / 1.9).truncatingRemainder(dividingBy: 1.0))
                content.overlay {
                    GeometryReader { geo in
                        let width = max(1, geo.size.width)
                        LinearGradient(
                            stops: [
                                .init(color: .clear, location: 0),
                                .init(color: .white.opacity(0.26), location: 0.5),
                                .init(color: .clear, location: 1),
                            ],
                            startPoint: .leading,
                            endPoint: .trailing
                        )
                        .frame(width: width * 0.65)
                        .offset(x: (phase * 1.65 - 0.5) * width)
                    }
                    .mask(content)
                    .allowsHitTesting(false)
                }
            }
        } else {
            content
        }
    }
}

extension View {
    func orbShimmer(active: Bool) -> some View {
        modifier(OrbShimmerModifier(active: active))
    }
}

/// Structured error card with copy affordance and collapsible `log tail:` (parity with desktop `ErrorNotice`).
struct OrbNotice: View {
    let message: String
    @State private var copied = false
    @State private var showTail = false
    private var parts: (head: String, tail: String?) {
        let components = message.components(separatedBy: "\n\nlog tail:\n")
        if components.count > 1 {
            return (components[0], components.dropFirst().joined(separator: "\n\nlog tail:\n"))
        }
        return (message, nil)
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .top, spacing: 10) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(OrbStyle.warning)
                    .padding(.top, 2)
                Text(parts.head)
                    .font(.footnote)
                    .foregroundStyle(OrbStyle.warning)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Button {
                    UIPasteboard.general.string = message
                    OrbHaptics.light()
                    copied = true
                    Task {
                        try? await Task.sleep(for: .seconds(1.5))
                        copied = false
                    }
                } label: {
                    Image(systemName: copied ? "checkmark" : "doc.on.doc")
                        .font(.system(size: 11, weight: .medium))
                        .foregroundStyle(copied ? OrbStyle.success : OrbStyle.textSecondary)
                        .frame(width: 26, height: 26)
                        .background(Color.white.opacity(0.05), in: RoundedRectangle(cornerRadius: 6, style: .continuous))
                }
                .buttonStyle(.plain)
                .accessibilityLabel(copied ? "Copied error" : "Copy error")
            }
            if let tail = parts.tail, !tail.isEmpty {
                Button {
                    withAnimation(.snappy(duration: 0.2)) { showTail.toggle() }
                    OrbHaptics.selection()
                } label: {
                    HStack(spacing: 5) {
                        Image(systemName: "chevron.right")
                            .font(.system(size: 9, weight: .semibold))
                            .rotationEffect(.degrees(showTail ? 90 : 0))
                        Text("Log tail")
                            .font(.caption2.weight(.medium))
                    }
                    .foregroundStyle(OrbStyle.textSecondary)
                }
                .buttonStyle(.plain)
                if showTail {
                    ScrollView(.horizontal, showsIndicators: false) {
                        Text(tail)
                            .font(.system(size: 11, design: .monospaced))
                            .foregroundStyle(OrbStyle.textSecondary)
                            .textSelection(.enabled)
                    }
                    .frame(maxHeight: 160)
                    .padding(8)
                    .background(Color.black.opacity(0.28), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
                }
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .background(OrbStyle.warning.opacity(0.08), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).stroke(OrbStyle.warning.opacity(0.24), lineWidth: 1))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("orb.error")
    }
}

struct OrbCircle: View {
    let symbol: String
    var body: some View { Image(systemName: symbol).font(.system(size: 17, weight: .regular)).frame(width: 26, height: 26) }
}

/// Match desktop's neutral leading glyph; only a project's own color tints its folders.
struct OrbListIcon: View {
    let symbol: String
    var color: Color?
    var body: some View {
        Image(systemName: symbol)
            .font(.system(size: 16, weight: .regular))
            .foregroundStyle(color ?? OrbStyle.icon)
            .frame(width: 20, height: 22)
            .accessibilityHidden(true)
    }
}

struct OrbHome: View {
    @State private var projects: [OrbRow] = []
    @State private var loading = true
    @State private var error = ""
    @State private var search = ""
    @State private var settings = false
    @State private var creating = false
    @State private var name = ""
    @State private var linkedMission: String?
    @State private var linkedProject = ""
    @State private var linkedFolder = ""
    @State private var renaming: OrbRow?
    @State private var renamedTitle = ""
    @State private var homeTab = ProcessInfo.processInfo.arguments.contains("-orb_open_inbox") ? "inbox" : "projects"
    @State private var inboxCount = 0
    @State private var workingByProject: [String: Int] = [:]
    @State private var linkedProjectObj: OrbRow?
    private let api = OrbCore.shared
    private let appearance = OrbProjectAppearance.shared
    private static func argValue(_ flag: String) -> String? {
        let args = ProcessInfo.processInfo.arguments
        guard let idx = args.firstIndex(of: flag), idx + 1 < args.count else { return nil }
        return args[idx + 1]
    }
    var body: some View {
        NavigationStack {
            Group {
                if homeTab == "inbox" {
                    OrbInboxView(
                        projects: projects,
                        actionableCount: $inboxCount,
                        onOpenMission: { row in
                            linkedProject = row.raw["project"].text
                            linkedFolder = row.folder
                            linkedMission = row.id
                        }
                    )
                } else {
                    projectsScrollView
                        .searchable(text: $search, prompt: "Search projects")
                }
            }
            .background(OrbStyle.background)
            .navigationTitle(homeTab == "inbox" ? "Inbox" : "Projects")
            .navigationBarTitleDisplayMode(.inline)
            .toolbarBackground(OrbStyle.background.opacity(0.92), for: .navigationBar)
            .toolbarBackground(.visible, for: .navigationBar)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) { Button { settings = true } label: { OrbCircle(symbol: "person.crop.circle") }.accessibilityLabel("Settings") }
                ToolbarItem(placement: .principal) { homeModePicker }
                ToolbarItem(placement: .topBarTrailing) { Button { creating = true } label: { OrbCircle(symbol: "folder.badge.plus") }.accessibilityLabel("New project") }
            }
            .task {
                if let m = Self.argValue("-orb_open_mission") {
                    linkedProject = Self.argValue("-orb_open_project") ?? "verity-core"
                    linkedFolder = ""
                    linkedMission = m
                } else if let p = Self.argValue("-orb_open_project") {
                    linkedProjectObj = projects.first(where: { $0.id == p }) ?? OrbRow(.object(["slug": .string(p), "title": .string(p == "verity-core" ? "Verity" : p.capitalized)]), project: true)
                }
                await load()
            }
            .sheet(isPresented: $settings) { OrbSettingsHome(onBackendChanged: { settings = false; Task { await load() } }) }
            .alert("New project", isPresented: $creating) {
                TextField("Project name", text: $name)
                Button("Create") { Task { await create() } }; Button("Cancel", role: .cancel) {}
            }
            .alert("Rename project", isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })) {
                TextField("Name", text: $renamedTitle)
                Button("Save") { if let project = renaming { Task { do { _ = try await api.call("/api/projects", method: "PUT", body: .object(["slug": .string(project.id), "title": .string(renamedTitle)])); await load() } catch { self.error = error.localizedDescription } } } }
                Button("Cancel", role: .cancel) {}
            }
            .navigationDestination(item: $linkedMission) { OrbConversation(missionID: $0, project: linkedProject, folder: linkedFolder) }
            .navigationDestination(item: $linkedProjectObj) { OrbProjectPage(project: $0) }
            .onOpenURL { url in
                if ["orb", "sandboxed"].contains(url.scheme ?? ""), url.host == "mission" {
                    linkedProject = ""
                    linkedFolder = ""
                    linkedMission = url.lastPathComponent
                } else if ["orb", "sandboxed"].contains(url.scheme ?? ""), url.host == "inbox" {
                    homeTab = "inbox"
                }
            }
        }.tint(.primary).preferredColorScheme(.dark)
    }

    private var homeModePicker: some View {
        HStack(spacing: 2) {
            Button {
                withAnimation(.snappy(duration: 0.2)) { homeTab = "projects" }
                OrbHaptics.selection()
            } label: {
                Text("Projects")
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(homeTab == "projects" ? .primary : OrbStyle.textSecondary)
                    .fixedSize()
                    .padding(.horizontal, 11)
                    .padding(.vertical, 5)
                    .background(
                        homeTab == "projects" ? OrbStyle.elevated : Color.clear,
                        in: RoundedRectangle(cornerRadius: OrbStyle.controlRadius)
                    )
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("home.tab.projects")
            .accessibilityAddTraits(homeTab == "projects" ? .isSelected : [])

            Button {
                withAnimation(.snappy(duration: 0.2)) { homeTab = "inbox" }
                OrbHaptics.selection()
            } label: {
                HStack(spacing: 5) {
                    Text("Inbox")
                        .font(.footnote.weight(.semibold))
                        .foregroundStyle(homeTab == "inbox" ? .primary : OrbStyle.textSecondary)
                        .fixedSize()
                    if inboxCount > 0 {
                        Text("\(inboxCount)")
                            .font(.system(size: 10.5, weight: .bold))
                            .foregroundStyle(homeTab == "inbox" ? OrbStyle.background : .primary)
                            .monospacedDigit()
                            .fixedSize()
                            .padding(.horizontal, 5.5)
                            .padding(.vertical, 1.5)
                            .background(
                                homeTab == "inbox" ? Color.white : OrbStyle.card,
                                in: RoundedRectangle(cornerRadius: OrbStyle.controlRadius)
                            )
                    }
                }
                .padding(.horizontal, 11)
                .padding(.vertical, 5)
                .background(
                    homeTab == "inbox" ? OrbStyle.elevated : Color.clear,
                    in: RoundedRectangle(cornerRadius: OrbStyle.controlRadius)
                )
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("home.tab.inbox")
            .accessibilityAddTraits(homeTab == "inbox" ? .isSelected : [])
        }
        .padding(3)
        .fixedSize(horizontal: true, vertical: false)
    }

    private var projectsScrollView: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 0) {
                if !error.isEmpty { OrbNotice(message: error).padding(.vertical, 6) }
                ForEach(projects.filter { search.isEmpty || $0.name.localizedCaseInsensitiveContains(search) }) { project in
                    NavigationLink { OrbProjectPage(project: project) } label: {
                        HStack(spacing: 14) {
                            OrbListIcon(symbol: "folder", color: appearance.color(project.id))
                            Text(project.name)
                                .font(.body.weight(.medium))
                                .foregroundStyle(.primary)
                                .lineLimit(1)
                                .truncationMode(.tail)
                            Spacer()
                            if let working = workingByProject[project.id], working > 0 {
                                HStack(spacing: 5) {
                                    OrbRunningDots(size: 10)
                                    Text("\(working) working")
                                        .font(.caption)
                                        .foregroundStyle(OrbStyle.textSecondary)
                                        .monospacedDigit()
                                        .lineLimit(1)
                                        .fixedSize(horizontal: true, vertical: false)
                                }
                                .accessibilityIdentifier("project.working.\(project.id)")
                            }
                            if !project.updatedAt.isEmpty {
                                Text(OrbStyle.relativeTime(project.updatedAt))
                                    .font(.caption)
                                    .foregroundStyle(OrbStyle.textMuted)
                                    .monospacedDigit()
                                    .lineLimit(1)
                                    .fixedSize(horizontal: true, vertical: false)
                            }
                            Image(systemName: "chevron.right")
                                .font(.system(size: 11, weight: .semibold))
                                .foregroundStyle(.tertiary)
                        }
                        .padding(.vertical, 12)
                        .contentShape(Rectangle())
                        .overlay(alignment: .bottom) {
                            Rectangle().fill(OrbStyle.border).frame(height: 0.5).padding(.leading, 34)
                        }
                    }
                    .buttonStyle(OrbPressButtonStyle())
                    .accessibilityIdentifier("project.\(project.id)")
                    .contextMenu {
                        Button("Rename") { renamedTitle = project.name; renaming = project }
                        OrbProjectColorMenu(project: project.id)
                        Button("Archive") {
                            Task {
                                do {
                                    _ = try await api.call("/api/projects/\(OrbCore.escape(project.id))/action", method: "POST", body: .object(["action": .string("archive")]))
                                    await load()
                                } catch { self.error = error.localizedDescription }
                            }
                        }
                    }
                }
                if loading && projects.isEmpty { projectSkeletons }
                if !loading && projects.isEmpty && error.isEmpty { ContentUnavailableView("Your projects", systemImage: "folder", description: Text("Create a project to start a conversation.")) }
            }.padding(.horizontal, 18)
        }
        .refreshable { await load() }
    }
    private var projectSkeletons: some View {
        VStack(spacing: 0) {
            ForEach(0..<5, id: \.self) { index in
                HStack(spacing: 14) {
                    OrbListIcon(symbol: "folder")
                    Text(index.isMultiple(of: 2) ? "Project name placeholder" : "Project workspace")
                        .font(.body.weight(.medium))
                        .redacted(reason: .placeholder)
                    Spacer()
                    Text("2h")
                        .font(.caption)
                        .redacted(reason: .placeholder)
                }
                .padding(.vertical, 12)
                .overlay(alignment: .bottom) {
                    Rectangle().fill(OrbStyle.border).frame(height: 0.5).padding(.leading, 34)
                }
            }
        }
        .orbShimmer(active: true)
        .allowsHitTesting(false)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Loading projects")
    }
    private static func countWorkingByProject(_ rows: [OrbRow]) -> [String: Int] {
        let activeStates: Set<String> = ["active", "running", "starting", "pending", "queued", "resuming", "waiting_background"]
        var counts: [String: Int] = [:]
        var seen: Set<String> = []
        for row in rows where row.mobile && !seen.contains(row.id) {
            seen.insert(row.id)
            guard activeStates.contains(row.state) else { continue }
            let tags = row.raw["tags"].items.map(\.text)
            if tags.contains("superseded") || tags.contains(where: { $0.hasPrefix("superseded-by:") }) { continue }
            let rawSlug = row.raw["project"].text.trimmingCharacters(in: .whitespacesAndNewlines)
            let slug = rawSlug.isEmpty ? "default" : rawSlug
            counts[slug, default: 0] += 1
        }
        return counts
    }
    private func load() async {
        defer { loading = false }
        if projects.isEmpty, let cached = OrbDisk.read("projects", as: OrbJSON.self) {
            projects = cached["projects"].items.filter { !["archived", "deleted"].contains($0["status"].text) }.map { OrbRow($0, project: true) }
        }
        if let cachedMissions = OrbDisk.read("inbox:missions", as: OrbJSON.self) {
            OrbReadCache.seedFromGlobalMissions(cachedMissions.items)
            let rows = cachedMissions.items.map { OrbRow($0) }.filter(\.mobile)
            workingByProject = Self.countWorkingByProject(rows)
            inboxCount = OrbInboxModel.unreadCount(missions: rows, projects: projects)
        }
        do {
            let requested = Date(), endpoint = api.endpoint
            let value = try await api.call("/api/projects")
            // Colors ride on the roster; a color chosen before the server stored any goes up here, once.
            Task { await appearance.apply(roster: value["projects"].items, fetchedAt: requested, endpoint: endpoint) }
            projects = value["projects"].items.filter { !["archived", "deleted"].contains($0["status"].text) }.map { OrbRow($0, project: true) }
            OrbDisk.saveAsync(value, key: "projects"); error = ""
            let roster = projects
            Task {
                if let rawMissions = try? await api.call("/api/control/missions?limit=100") {
                    OrbDisk.saveAsync(rawMissions, key: "inbox:missions")
                    OrbReadCache.seedFromGlobalMissions(rawMissions.items)
                    let rows = rawMissions.items.map { OrbRow($0) }.filter(\.mobile)
                    workingByProject = Self.countWorkingByProject(rows)
                    inboxCount = OrbInboxModel.unreadCount(missions: rows, projects: roster)
                    await OrbReadCache.prefetch(rows)
                }
            }
            // Warm the top projects and the agent picker catalog so opening a project or composer feels instant.
            let topProjects = Array(projects.prefix(3))
            Task {
                _ = try? await OrbReadCache.agentCatalog()
                for proj in topProjects {
                    _ = try? await OrbReadCache.project(proj.id)
                }
            }
        } catch { self.error = error.localizedDescription }
    }
    private func create() async {
        let slug = name.folding(options: .diacriticInsensitive, locale: .current).lowercased().split(whereSeparator: { !$0.isLetter && !($0.isNumber) }).joined(separator: "-")
        guard !slug.isEmpty else { return }
        do { _ = try await api.call("/api/projects", method: "PUT", body: .object(["slug": .string(slug), "title": .string(name)])); name = ""; await load() }
        catch { self.error = error.localizedDescription }
    }
}

struct OrbProjectPage: View {
    let project: OrbRow
    @State private var missions: [OrbRow] = []
    @State private var loading = true
    @State private var folders: [String] = []
    @State private var collapsed: Set<String> = []
    @State private var expandedMissions: Set<String> = []
    @State private var search = ""
    @State private var filter = "All"
    @State private var error = ""
    @State private var newFolder = false
    @State private var folderName = ""
    @State private var newFolderParent = ""
    @State private var renamingFolder: String?
    @State private var renameFolderDraft = ""
    @State private var movingFolder: String?
    @State private var moveFolderDraft = ""
    @State private var deletingFolder: String?
    @State private var loadedArchived = false
    private let api = OrbCore.shared
    private let appearance = OrbProjectAppearance.shared
    private func matchesFilter(_ row: OrbRow) -> Bool {
        (search.isEmpty || row.name.localizedCaseInsensitiveContains(search) || OrbStyle.displayTitle(row.name).localizedCaseInsensitiveContains(search)) &&
        (filter == "Archived" ? row.state == "acknowledged" : row.state != "acknowledged") &&
        (filter != "Working" || row.active) &&
        (filter != "Needs attention" || ["blocked", "failed", "interrupted"].contains(row.state))
    }
    private var visible: [OrbRow] {
        if !search.isEmpty {
            return missions.filter(matchesFilter)
        }
        return OrbMissionTree.treeRows(missions, visible: matchesFilter)
    }
    private var nestedRoots: [OrbNestedMission] {
        if !search.isEmpty {
            return visible.map { OrbNestedMission(mission: $0) }
        }
        return OrbMissionTree.nest(visible)
    }
    private struct FlattenedMissionRow: Identifiable {
        let node: OrbNestedMission
        let depth: Int
        let launched: Int
        let launchedLive: Int
        let isExpanded: Bool
        var id: String { node.id }
    }
    private func flatten(_ nodes: [OrbNestedMission], depth: Int = 0) -> [FlattenedMissionRow] {
        var result: [FlattenedMissionRow] = []
        for node in nodes {
            let isExpanded = expandedMissions.contains(node.id)
            let launched = OrbMissionTree.countNested(node)
            let launchedLive = OrbMissionTree.countNested(node, matches: \.active)
            result.append(FlattenedMissionRow(node: node, depth: depth, launched: launched, launchedLive: launchedLive, isExpanded: isExpanded))
            if isExpanded && !node.children.isEmpty {
                result.append(contentsOf: flatten(node.children, depth: depth + 1))
            }
        }
        return result
    }
    private var filtering: Bool { !search.isEmpty || filter != "All" }
    private var paths: [String] {
        let sources = (filtering ? [] : folders) + nestedRoots.map(\.mission.folder)
        var all = Set(sources)
        for path in sources {
            let parts = path.split(separator: "/")
            for depth in 1...max(1, parts.count) where depth <= parts.count { all.insert(parts.prefix(depth).joined(separator: "/")) }
        }
        return all.filter { !$0.isEmpty }.sorted()
    }
    private func shown(_ folder: String) -> Bool { !search.isEmpty || !collapsed.contains(where: { folder.hasPrefix($0 + "/") }) }
    private static func folderParent(_ path: String) -> String {
        guard let idx = path.lastIndex(of: "/") else { return "" }
        return String(path[..<idx])
    }
    private static func folderBaseName(_ path: String) -> String {
        path.split(separator: "/").last.map(String.init) ?? path
    }
    private var scrollContent: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 0) {
                if filter != "All" {
                    HStack {
                        Text(filter)
                            .font(.footnote.weight(.medium))
                            .foregroundStyle(OrbStyle.textSecondary)
                            .lineLimit(1)
                            .fixedSize(horizontal: true, vertical: false)
                        Spacer()
                        Button("Clear filter") { withAnimation(.snappy(duration: 0.18)) { filter = "All" } }
                            .font(.footnote)
                            .lineLimit(1)
                            .fixedSize(horizontal: true, vertical: false)
                    }.frame(minHeight: 38)
                }
                if !error.isEmpty { OrbNotice(message: error).padding(.vertical, 6) }
                if loading && missions.isEmpty { conversationSkeletons }
                ForEach(flatten(nestedRoots.filter { $0.mission.folder.isEmpty })) { item in
                    missionRowView(item)
                }
                folderRows
                if !loading && nestedRoots.isEmpty && (folders.isEmpty || filtering) && error.isEmpty {
                    ContentUnavailableView(
                        filtering ? "No matching conversations" : "No conversations yet",
                        systemImage: "bubble.left.and.bubble.right",
                        description: Text(filtering ? "Try another search or filter." : "Start an agent with the + button.")
                    )
                }
            }.padding(.horizontal, 18)
        }
    }
    private var projectToolbarMenu: some View {
        Menu {
            Picker("Show", selection: $filter) {
                ForEach(["All", "Working", "Needs attention", "Archived"], id: \.self) { option in
                    Text(option)
                }
            }
            Button("New folder") { newFolderParent = ""; folderName = ""; newFolder = true }
            OrbProjectColorMenu(project: project.id)
            NavigationLink("Project context") { OrbDocuments(project: project.id, path: "") }
        } label: {
            OrbCircle(symbol: "ellipsis")
        }
        .accessibilityLabel("Project actions")
    }
    var body: some View {
        scrollContent
            .background(OrbStyle.background)
            .navigationTitle(project.name)
            .navigationBarTitleDisplayMode(.inline)
            .toolbarBackground(OrbStyle.background.opacity(0.92), for: .navigationBar)
            .toolbarBackground(.visible, for: .navigationBar)
            .searchable(text: $search, prompt: "Search conversations")
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) { projectToolbarMenu }
                ToolbarItem(placement: .topBarTrailing) {
                    NavigationLink { OrbConversation(missionID: nil, project: project.id, folder: "") } label: { Image(systemName: "plus") }
                        .accessibilityLabel("New agent").accessibilityIdentifier("new-agent")
                }
            }
            .alert(newFolderParent.isEmpty ? "New folder" : "New subfolder in \(Self.folderBaseName(newFolderParent))", isPresented: $newFolder) {
                TextField("Folder name", text: $folderName)
                Button("Create") { Task { await mkdir() } }
                Button("Cancel", role: .cancel) { newFolderParent = ""; folderName = "" }
            }
            .alert("Rename folder", isPresented: Binding(get: { renamingFolder != nil }, set: { if !$0 { renamingFolder = nil } })) {
                TextField("Folder name", text: $renameFolderDraft)
                Button("Rename") {
                    if let folder = renamingFolder {
                        Task { await renameFolder(folder, to: renameFolderDraft) }
                    }
                }
                Button("Cancel", role: .cancel) { renamingFolder = nil }
            }
            .alert("Move folder", isPresented: Binding(get: { movingFolder != nil }, set: { if !$0 { movingFolder = nil } })) {
                TextField("Destination folder (empty for root)", text: $moveFolderDraft)
                Button("Move") {
                    if let folder = movingFolder {
                        Task { await moveFolder(folder, into: moveFolderDraft) }
                    }
                }
                Button("Cancel", role: .cancel) { movingFolder = nil }
            } message: {
                Text("Enter destination parent folder path within \(project.name), or leave empty to move to the project root.")
            }
            .alert("Delete folder?", isPresented: Binding(get: { deletingFolder != nil }, set: { if !$0 { deletingFolder = nil } })) {
                Button("Delete", role: .destructive) {
                    if let folder = deletingFolder {
                        Task { await deleteFolder(folder) }
                    }
                }
                Button("Cancel", role: .cancel) { deletingFolder = nil }
            } message: {
                if let folder = deletingFolder {
                    Text("Delete \"\(folder)\" and all context files inside? This cannot be undone.")
                }
            }
            .task { await load() }.refreshable { await load(force: true) }
            .onChange(of: filter) { _, newValue in
                if newValue == "Archived" && !loadedArchived {
                    Task { await load(force: true) }
                }
            }
    }
    private var conversationSkeletons: some View {
        VStack(spacing: 0) {
            ForEach(0..<5, id: \.self) { index in
                HStack(alignment: .top, spacing: 12) {
                    OrbListIcon(symbol: "cpu")
                        .opacity(0.45)
                        .padding(.top, 1)
                    VStack(alignment: .leading, spacing: 4) {
                        Text(index.isMultiple(of: 2) ? "Conversation title placeholder" : "Conversation title")
                            .font(.subheadline)
                        Text("Agent · Conversation status").font(.caption).foregroundStyle(.secondary)
                    }.redacted(reason: .placeholder)
                    Spacer(minLength: 0)
                }.padding(.vertical, 10)
                    .overlay(alignment: .bottom) { Rectangle().fill(OrbStyle.border).frame(height: 0.5).padding(.leading, 32) }
            }
        }
        .orbShimmer(active: true)
        .allowsHitTesting(false)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Loading conversations")
        .accessibilityIdentifier("conversations-loading")
    }
    private var folderRows: some View {
        ForEach(paths.filter(shown), id: \.self) { folder in
            HStack {
                Button {
                    withAnimation(.snappy(duration: 0.18)) {
                        if !collapsed.insert(folder).inserted { collapsed.remove(folder) }
                    }
                    OrbHaptics.selection()
                } label: {
                    HStack(spacing: 10) {
                        OrbListIcon(symbol: "folder", color: appearance.color(project.id))
                        Text(Self.folderBaseName(folder))
                            .font(.subheadline.weight(.semibold))
                            .lineLimit(1)
                            .truncationMode(.tail)
                        Spacer()
                        Image(systemName: "chevron.right")
                            .font(.system(size: 10, weight: .semibold))
                            .foregroundStyle(OrbStyle.icon)
                            .rotationEffect(.degrees(collapsed.contains(folder) ? 0 : 90))
                    }
                    .padding(.vertical, 8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
                }.accessibilityIdentifier("folder.\(folder)")
                NavigationLink { OrbConversation(missionID: nil, project: project.id, folder: folder) } label: { Image(systemName: "plus").font(.system(size: 14)).frame(width: 44, height: 44) }.accessibilityLabel("New agent in \(folder)")
            }
            .contentShape(Rectangle())
            .contextMenu {
                NavigationLink {
                    OrbConversation(missionID: nil, project: project.id, folder: folder)
                } label: {
                    Label("New agent in folder", systemImage: "plus.bubble")
                }
                Button {
                    newFolderParent = folder
                    folderName = ""
                    newFolder = true
                } label: {
                    Label("New subfolder", systemImage: "folder.badge.plus")
                }
                NavigationLink {
                    OrbDocuments(project: project.id, path: folder)
                } label: {
                    Label("Folder context files", systemImage: "doc.text")
                }
                Divider()
                Button {
                    renameFolderDraft = Self.folderBaseName(folder)
                    renamingFolder = folder
                } label: {
                    Label("Rename", systemImage: "pencil")
                }
                Button {
                    moveFolderDraft = Self.folderParent(folder)
                    movingFolder = folder
                } label: {
                    Label("Move…", systemImage: "folder")
                }
                Divider()
                Button(role: .destructive) {
                    deletingFolder = folder
                } label: {
                    Label("Delete…", systemImage: "trash")
                }
            }
            .padding(.leading, CGFloat(min(36, max(0, folder.split(separator: "/").count - 1) * 12))).padding(.top, 2)
            if !collapsed.contains(folder) || !search.isEmpty {
                ForEach(flatten(nestedRoots.filter { $0.mission.folder == folder })) { item in
                    missionRowView(item)
                        .padding(.leading, 16)
                }
            }
        }
    }
    private func missionRowView(_ item: FlattenedMissionRow) -> some View {
        missionLink(item.node.mission, launched: item.launched, launchedLive: item.launchedLive, isExpanded: item.isExpanded) {
            withAnimation(.snappy(duration: 0.18)) {
                if !expandedMissions.insert(item.node.id).inserted {
                    expandedMissions.remove(item.node.id)
                }
            }
            OrbHaptics.selection()
        }
        .padding(.leading, CGFloat(min(48, item.depth * 18)))
    }
    @ViewBuilder
    private func statusGlyph(for row: OrbRow) -> some View {
        let _ = OrbMissionUnreadStore.shared.version
        if row.active {
            OrbRunningDots(size: 13)
                .frame(width: 20, height: 22)
        } else {
            OrbListIcon(symbol: row.backend.hasPrefix("cloud_") ? "cloud" : "cpu")
                .overlay(alignment: .bottomTrailing) {
                    if ["failed", "blocked", "not_feasible"].contains(row.state) {
                        Circle().fill(OrbStyle.warning).frame(width: 6, height: 6)
                    } else if OrbMissionUnreadStore.shared.isUnread(row: row) {
                        Circle().fill(Color.blue).frame(width: 6, height: 6)
                    }
                }
        }
    }
    private func missionLink(_ row: OrbRow, launched: Int = 0, launchedLive: Int = 0, isExpanded: Bool = false, onToggleLaunched: (() -> Void)? = nil) -> some View {
        let isGoal = OrbStyle.goalObjective(row.name) != nil || row.raw["goal_mode"].flag
        let cleanTitle = OrbStyle.displayTitle(row.name)
        let rel = OrbStyle.relativeTime(row.updatedAt)
        return HStack(alignment: .top, spacing: 6) {
            NavigationLink { OrbConversation(missionID: row.id, project: project.id, folder: row.folder) } label: {
                HStack(alignment: .top, spacing: 12) {
                    statusGlyph(for: row)
                        .padding(.top, 1)
                    VStack(alignment: .leading, spacing: 3) {
                        HStack(alignment: .center, spacing: 6) {
                            if isGoal {
                                HStack(spacing: 3) {
                                    Image(systemName: "target")
                                        .font(.system(size: 9, weight: .semibold))
                                    Text("Goal")
                                        .font(.system(size: 10, weight: .semibold))
                                        .lineLimit(1)
                                        .fixedSize(horizontal: true, vertical: false)
                                }
                                .foregroundStyle(OrbStyle.textSecondary)
                                .padding(.horizontal, 6)
                                .padding(.vertical, 2)
                                .background(Color.white.opacity(0.07), in: Capsule())
                            }
                            Text(cleanTitle)
                                .font(.subheadline.weight(.medium))
                                .lineLimit(1)
                                .truncationMode(.tail)
                                .foregroundStyle(.primary)
                            Spacer(minLength: 4)
                            if launched == 0, !rel.isEmpty {
                                Text(rel)
                                    .font(.caption2)
                                    .foregroundStyle(OrbStyle.textMuted)
                                    .monospacedDigit()
                                    .lineLimit(1)
                                    .fixedSize(horizontal: true, vertical: false)
                            }
                        }
                        Text("\(OrbStyle.serviceName(row.backend)) · \(OrbStyle.statusLabel(row.state))")
                            .font(.caption)
                            .foregroundStyle(row.active ? OrbStyle.textSecondary : OrbStyle.textMuted)
                            .lineLimit(1)
                    }
                }
                .padding(.vertical, 10)
                .contentShape(Rectangle())
            }
            .buttonStyle(OrbPressButtonStyle())
            .accessibilityIdentifier("mission.\(row.id)")

            if launched > 0, let onToggleLaunched {
                let badgeColor = launchedLive > 0 ? OrbStyle.success : OrbStyle.textSecondary
                Button(action: onToggleLaunched) {
                    HStack(alignment: .center, spacing: 6) {
                        HStack(spacing: 3) {
                            Text(launchedLive > 0 ? "\(launchedLive)/\(launched)" : "\(launched)")
                                .font(.caption2.weight(.medium).monospacedDigit())
                                .lineLimit(1)
                                .fixedSize(horizontal: true, vertical: false)
                            Image(systemName: "chevron.right")
                                .font(.system(size: 8.5, weight: .bold))
                                .rotationEffect(.degrees(isExpanded ? 90 : 0))
                        }
                        .foregroundStyle(badgeColor)
                        .padding(.leading, 6)
                        .padding(.trailing, 5)
                        .padding(.vertical, 2)
                        .background(
                            Color.white.opacity(isExpanded ? 0.10 : 0.05),
                            in: Capsule()
                        )
                        .overlay(
                            Capsule().stroke(
                                launchedLive > 0
                                    ? OrbStyle.success.opacity(0.35)
                                    : (isExpanded ? OrbStyle.borderStrong : OrbStyle.border),
                                lineWidth: 0.75
                            )
                        )

                        if !rel.isEmpty {
                            Text(rel)
                                .font(.caption2)
                                .foregroundStyle(OrbStyle.textMuted)
                                .monospacedDigit()
                                .lineLimit(1)
                                .fixedSize(horizontal: true, vertical: false)
                        }
                    }
                    .padding(.top, 9)
                    .padding(.bottom, 6)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("\(isExpanded ? "Hide" : "Show") \(launched) subagents for \(cleanTitle)")
                .accessibilityIdentifier("mission.subagents.\(row.id)")
            }
        }
        .overlay(alignment: .bottom) {
            Rectangle().fill(OrbStyle.border).frame(height: 0.5).padding(.leading, 32)
        }
        .contextMenu {
            if !row.active && row.state != "acknowledged" {
                Button("Archive") {
                    Task {
                        do {
                            _ = try await api.call("/api/control/missions/\(OrbCore.escape(row.id))/status", method: "POST", body: .object(["status": .string("acknowledged")]))
                            OrbReadCache.invalidate("project:\(project.id)")
                            OrbReadCache.invalidate("project:\(project.id):all")
                            await load(force: true)
                        } catch { self.error = error.localizedDescription }
                    }
                }
            }
        }
    }
    private func apply(_ value: OrbJSON) {
        missions = value["missions"].items.map { OrbRow($0) }.filter(\.mobile)
        if case .object(let entries) = value["manifest"]["entries"] { folders = entries.filter { $0.value["directory"].flag }.map(\.key) }
    }
    private func load(force: Bool = false) async {
        let includeArchived = filter == "Archived"
        let cacheKey = includeArchived ? "project:\(project.id):all" : "project:\(project.id)"
        if let cached = OrbReadCache.read(cacheKey) ?? OrbReadCache.read("project:\(project.id)") {
            apply(cached)
            loading = false
        }
        defer { loading = false }
        do {
            let value = try await OrbReadCache.project(project.id, includeArchived: includeArchived, force: force)
            guard !Task.isCancelled else { return }
            if includeArchived { loadedArchived = true }
            apply(value); loading = false; error = ""
            await OrbReadCache.prefetch(nestedRoots.map(\.mission))
        } catch is CancellationError {} catch { self.error = error.localizedDescription }
    }
    private func mkdir() async {
        let trimmed = folderName.trimmingCharacters(in: .whitespacesAndNewlines).trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        guard !trimmed.isEmpty else { return }
        let fullPath = newFolderParent.isEmpty ? trimmed : "\(newFolderParent)/\(trimmed)"
        do {
            _ = try await api.call("/api/projects/\(OrbCore.escape(project.id))/file/mkdir", method: "POST", body: .object(["path": .string(fullPath)]))
            folderName = ""
            newFolderParent = ""
            OrbReadCache.invalidate("project:\(project.id)")
            OrbReadCache.invalidate("project:\(project.id):all")
            await load(force: true)
        } catch { self.error = error.localizedDescription }
    }
    private func migrateFolderMissions(from oldPath: String, to destination: String) async {
        let affected = missions.filter { $0.folder == oldPath || $0.folder.hasPrefix(oldPath + "/") }
        for m in affected {
            let suffix = String(m.folder.dropFirst(oldPath.count))
            let targetFolder = destination + suffix
            var tags = m.raw["tags"].items.map(\.text).filter { !$0.isEmpty && !$0.hasPrefix("orb-folder:") }
            if !targetFolder.isEmpty {
                tags.append("orb-folder:\(targetFolder)")
            }
            _ = try? await api.call(
                "/api/control/missions/\(OrbCore.escape(m.id))/project",
                method: "POST",
                body: .object([
                    "project": .string(project.id),
                    "tags": .array(tags.map(OrbJSON.string))
                ])
            )
        }
    }
    private func renameFolder(_ folder: String, to rawNewName: String) async {
        let input = rawNewName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !input.isEmpty && !input.contains("/") && !input.contains("\\") else {
            self.error = "Enter a folder name without slashes."
            return
        }
        let parent = Self.folderParent(folder)
        let destination = parent.isEmpty ? input : "\(parent)/\(input)"
        guard destination != folder else { return }
        do {
            if folders.contains(where: { $0 == folder || $0.hasPrefix(folder + "/") }) {
                _ = try await api.call(
                    "/api/projects/\(OrbCore.escape(project.id))/file/transfer",
                    method: "POST",
                    body: .object(["path": .string(folder), "destination": .string(destination), "copy": .bool(false)])
                )
            } else {
                _ = try await api.call(
                    "/api/projects/\(OrbCore.escape(project.id))/file/mkdir",
                    method: "POST",
                    body: .object(["path": .string(destination)])
                )
            }
            await migrateFolderMissions(from: folder, to: destination)
            OrbReadCache.invalidate("project:\(project.id)")
            OrbReadCache.invalidate("project:\(project.id):all")
            await load(force: true)
        } catch { self.error = error.localizedDescription }
    }
    private func moveFolder(_ folder: String, into rawParent: String) async {
        let parent = rawParent.trimmingCharacters(in: .whitespacesAndNewlines).trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        let base = Self.folderBaseName(folder)
        let destination = parent.isEmpty ? base : "\(parent)/\(base)"
        guard destination != folder else { return }
        if destination.hasPrefix(folder + "/") {
            self.error = "Cannot move a folder inside itself."
            return
        }
        do {
            if folders.contains(where: { $0 == folder || $0.hasPrefix(folder + "/") }) {
                _ = try await api.call(
                    "/api/projects/\(OrbCore.escape(project.id))/file/transfer",
                    method: "POST",
                    body: .object(["path": .string(folder), "destination": .string(destination), "copy": .bool(false)])
                )
            } else {
                _ = try await api.call(
                    "/api/projects/\(OrbCore.escape(project.id))/file/mkdir",
                    method: "POST",
                    body: .object(["path": .string(destination)])
                )
            }
            await migrateFolderMissions(from: folder, to: destination)
            OrbReadCache.invalidate("project:\(project.id)")
            OrbReadCache.invalidate("project:\(project.id):all")
            await load(force: true)
        } catch { self.error = error.localizedDescription }
    }
    private func deleteFolder(_ folder: String) async {
        let hasMissions = missions.contains { $0.state != "acknowledged" && ($0.folder == folder || $0.folder.hasPrefix(folder + "/")) }
        if hasMissions {
            self.error = "Move or archive the agents in \"\(folder)\" before deleting it."
            return
        }
        do {
            _ = try await api.call("/api/projects/\(OrbCore.escape(project.id))/file?path=\(OrbCore.escape(folder))", method: "DELETE")
            folders.removeAll { $0 == folder || $0.hasPrefix(folder + "/") }
            OrbReadCache.invalidate("project:\(project.id)")
            OrbReadCache.invalidate("project:\(project.id):all")
            await load(force: true)
        } catch { self.error = error.localizedDescription }
    }
}
