import AppKit
import Foundation
import SwiftUI

struct AgentEvent: Decodable {
    let provider: String
    let status: String
    let cwd: String?
    let detail: String?
    let event: String?
    let timestamp: TimeInterval
}

struct AgentSnapshot: Identifiable, Equatable {
    let id: String
    let name: String
    var status: AgentState
    var cwd: String?
    var detail: String?
    var updatedAt: Date?

    static func idle(id: String, name: String) -> AgentSnapshot {
        AgentSnapshot(id: id, name: name, status: .idle, cwd: nil, detail: nil, updatedAt: nil)
    }
}

enum AgentState: String {
    case idle
    case running
    case waitingApproval = "waiting_approval"
    case completed
    case error

    var label: String {
        switch self {
        case .idle:
            return "Idle"
        case .running:
            return "Running"
        case .waitingApproval:
            return "Waiting"
        case .completed:
            return "Done"
        case .error:
            return "Error"
        }
    }

    var color: Color {
        switch self {
        case .idle:
            return Color(red: 0.52, green: 0.55, blue: 0.58)
        case .running:
            return Color(red: 0.16, green: 0.68, blue: 0.36)
        case .waitingApproval:
            return Color(red: 0.95, green: 0.68, blue: 0.20)
        case .completed:
            return Color(red: 0.22, green: 0.48, blue: 0.88)
        case .error:
            return Color(red: 0.88, green: 0.20, blue: 0.24)
        }
    }

    var symbol: String {
        switch self {
        case .idle:
            return "minus.circle.fill"
        case .running:
            return "play.circle.fill"
        case .waitingApproval:
            return "exclamationmark.circle.fill"
        case .completed:
            return "checkmark.circle.fill"
        case .error:
            return "xmark.circle.fill"
        }
    }
}

@MainActor
final class AgentStatusStore: ObservableObject {
    @Published private(set) var snapshots: [AgentSnapshot] = [
        .idle(id: "claude", name: "Claude"),
        .idle(id: "codex", name: "Codex")
    ]

    private let eventFile: URL
    private let codexLogFile: URL
    private var timer: Timer?
    private var lastFileSize: UInt64 = 0
    private let logDateFormatter: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter
    }()

    init() {
        let home = FileManager.default.homeDirectoryForCurrentUser
        eventFile = home.appendingPathComponent(".agent-pin/events.jsonl")
        codexLogFile = home.appendingPathComponent(".codex/log/codex-tui.log")
    }

    func start() {
        refresh()
        timer = Timer.scheduledTimer(withTimeInterval: 0.8, repeats: true) { [weak self] _ in
            Task { @MainActor in
                self?.refresh()
            }
        }
    }

    private func refresh() {
        let fileSize = (try? eventFile.resourceValues(forKeys: [.fileSizeKey]).fileSize).map(UInt64.init) ?? 0
        lastFileSize = fileSize
        let contents = (try? String(contentsOf: eventFile, encoding: .utf8)) ?? ""

        var latest: [String: AgentSnapshot] = [
            "claude": .idle(id: "claude", name: "Claude"),
            "codex": .idle(id: "codex", name: "Codex")
        ]

        let decoder = JSONDecoder()
        for line in contents.split(separator: "\n") {
            guard let data = String(line).data(using: .utf8),
                  let event = try? decoder.decode(AgentEvent.self, from: data) else {
                continue
            }

            let provider = event.provider.lowercased()
            guard provider == "claude" || provider == "codex" else {
                continue
            }

            let state = AgentState(rawValue: event.status) ?? .idle
            latest[provider] = AgentSnapshot(
                id: provider,
                name: provider == "claude" ? "Claude" : "Codex",
                status: state,
                cwd: event.cwd,
                detail: event.detail ?? event.event,
                updatedAt: Date(timeIntervalSince1970: event.timestamp)
            )
        }

        applyCodexLogFallback(to: &latest)
        snapshots = ["claude", "codex"].compactMap { latest[$0] }.filter(shouldDisplay)
        markStaleStates()
    }

    private func applyCodexLogFallback(to latest: inout [String: AgentSnapshot]) {
        guard let inferred = inferCodexSnapshotFromLog() else {
            return
        }

        let current = latest["codex"]
        let currentDate = current?.updatedAt ?? .distantPast
        guard inferred.updatedAt ?? .distantPast > currentDate else {
            return
        }

        latest["codex"] = inferred
    }

    private func inferCodexSnapshotFromLog() -> AgentSnapshot? {
        guard let log = readTail(of: codexLogFile, maxBytes: 260_000) else {
            return nil
        }

        var snapshot: AgentSnapshot?
        for line in log.split(separator: "\n") {
            let text = String(line)
            guard let timestamp = parseLogTimestamp(from: text) else {
                continue
            }

            let state: AgentState?
            let detail: String
            if text.contains("codex_core::tasks: new") ||
                text.contains("model_client.stream_responses_websocket") && text.contains(": codex_core::client: new") {
                state = .running
                detail = "log activity"
            } else if text.contains("ToolCall:") && text.contains("\"sandbox_permissions\":\"require_escalated\"") {
                state = .waitingApproval
                detail = "approval requested"
            } else if text.contains("codex.op=\"exec_approval\"") {
                state = .running
                detail = "approval handled"
            } else if text.contains("codex_core::tasks: close") {
                state = .completed
                detail = "turn complete"
            } else {
                state = nil
                detail = ""
            }

            guard let state else {
                continue
            }

            snapshot = AgentSnapshot(
                id: "codex",
                name: "Codex",
                status: state,
                cwd: nil,
                detail: detail,
                updatedAt: timestamp
            )
        }

        return snapshot
    }

    private func readTail(of url: URL, maxBytes: UInt64) -> String? {
        guard let handle = try? FileHandle(forReadingFrom: url) else {
            return nil
        }
        defer { try? handle.close() }

        let size = (try? handle.seekToEnd()) ?? 0
        let offset = size > maxBytes ? size - maxBytes : 0
        try? handle.seek(toOffset: offset)
        let data = handle.readDataToEndOfFile()
        return String(data: data, encoding: .utf8)
    }

    private func parseLogTimestamp(from line: String) -> Date? {
        guard line.count >= 27 else {
            return nil
        }

        let endIndex = line.index(line.startIndex, offsetBy: 27)
        return logDateFormatter.date(from: String(line[..<endIndex]))
    }

    private func markStaleStates() {
        let now = Date()
        snapshots = snapshots.map { snapshot in
            guard let updatedAt = snapshot.updatedAt else {
                return snapshot
            }

            let age = now.timeIntervalSince(updatedAt)
            let isStaleTerminal = snapshot.status == .completed && age > 600
            let isStaleActive = (snapshot.status == .running || snapshot.status == .waitingApproval) && age > 900

            if isStaleTerminal || isStaleActive {
                var stale = snapshot
                stale.status = .idle
                stale.detail = "stale"
                return stale
            }

            return snapshot
        }.filter(shouldDisplay)
    }

    private func shouldDisplay(_ snapshot: AgentSnapshot) -> Bool {
        guard let updatedAt = snapshot.updatedAt else {
            return false
        }

        if snapshot.status != .idle {
            return true
        }

        return Date().timeIntervalSince(updatedAt) < 20
    }
}

struct PinView: View {
    @ObservedObject var store: AgentStatusStore
    @State private var expanded = true

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            HStack(spacing: 8) {
                Image(systemName: "pin.fill")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(.white.opacity(0.82))
                if expanded {
                    Text("Agent Pin")
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(.white.opacity(0.9))
                }
            }

            if store.snapshots.isEmpty {
                HStack(spacing: 7) {
                    Image(systemName: "moon.circle.fill")
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(Color(red: 0.52, green: 0.55, blue: 0.58))
                        .frame(width: 16, height: 16)

                    if expanded {
                        Text("Idle")
                            .font(.system(size: 11, weight: .semibold))
                            .foregroundStyle(.white.opacity(0.82))
                    }
                }
                .frame(height: expanded ? 24 : 18)
            } else {
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(store.snapshots) { snapshot in
                        AgentRow(snapshot: snapshot, expanded: expanded)
                    }
                }
            }
        }
        .padding(.horizontal, expanded ? 12 : 9)
        .padding(.vertical, 10)
        .frame(width: expanded ? 164 : 54, alignment: .leading)
        .background(.black.opacity(0.78), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .stroke(.white.opacity(0.16), lineWidth: 1)
        )
        .shadow(color: .black.opacity(0.22), radius: 10, x: 0, y: 4)
        .onTapGesture(count: 2) {
            expanded.toggle()
        }
    }
}

struct AgentRow: View {
    let snapshot: AgentSnapshot
    let expanded: Bool

    var body: some View {
        HStack(spacing: 7) {
            Image(systemName: snapshot.status.symbol)
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(snapshot.status.color)
                .frame(width: 16, height: 16)

            Text(snapshot.name.prefix(1))
                .font(.system(size: 11, weight: .bold))
                .foregroundStyle(.white.opacity(0.86))
                .frame(width: 10)

            if expanded {
                VStack(alignment: .leading, spacing: 1) {
                    HStack(spacing: 5) {
                        Text(snapshot.name)
                            .font(.system(size: 11, weight: .semibold))
                            .foregroundStyle(.white.opacity(0.95))
                        Spacer(minLength: 4)
                        Text(snapshot.status.label)
                            .font(.system(size: 11, weight: .medium))
                            .foregroundStyle(snapshot.status.color)
                    }

                    Text(secondaryText)
                        .font(.system(size: 9, weight: .regular))
                        .foregroundStyle(.white.opacity(0.56))
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            }
        }
        .frame(height: expanded ? 30 : 18)
    }

    private var secondaryText: String {
        if let cwd = snapshot.cwd, !cwd.isEmpty {
            return URL(fileURLWithPath: cwd).lastPathComponent
        }

        if let updatedAt = snapshot.updatedAt {
            let seconds = Int(Date().timeIntervalSince(updatedAt))
            return seconds < 60 ? "just now" : "\(seconds / 60)m ago"
        }

        return "no session"
    }
}

final class PinPanel: NSPanel {
    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private let store = AgentStatusStore()
    private var panel: NSPanel?

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
        store.start()

        let hostingView = NSHostingView(rootView: PinView(store: store))
        hostingView.frame = NSRect(x: 0, y: 0, width: 170, height: 112)

        let screenFrame = NSScreen.main?.visibleFrame ?? NSRect(x: 0, y: 0, width: 1440, height: 900)
        let panelFrame = NSRect(
            x: screenFrame.maxX - 190,
            y: screenFrame.maxY - 134,
            width: 170,
            height: 112
        )

        let panel = PinPanel(
            contentRect: panelFrame,
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )
        panel.contentView = hostingView
        panel.backgroundColor = .clear
        panel.isOpaque = false
        panel.hasShadow = false
        panel.isFloatingPanel = true
        panel.hidesOnDeactivate = false
        panel.isMovableByWindowBackground = true
        panel.level = .statusBar
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary]
        panel.orderFrontRegardless()

        self.panel = panel
    }
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.run()
