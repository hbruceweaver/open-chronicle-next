import AppKit
import SwiftUI

@main
struct OpenChronicleApp: App {
    @StateObject private var appModel = AppModel()
    @StateObject private var navigation = NavigationModel()

    var body: some Scene {
        WindowGroup("Open Chronicle", id: "main") {
            ShellView()
                .environmentObject(appModel)
                .environmentObject(navigation)
                .frame(minWidth: 720, minHeight: 480)
                .task { await appModel.connect() }
        }
        .defaultSize(width: 960, height: 680)

        MenuBarExtra("Open Chronicle", systemImage: menuBarSymbol) {
            ChronicleMenu()
                .environmentObject(appModel)
        }
    }

    private var menuBarSymbol: String {
        switch appModel.health.status {
        case .connecting: "clock"
        case .ready: "circle.fill"
        case .repairRequired: "exclamationmark.triangle"
        }
    }
}

private struct ShellView: View {
    @EnvironmentObject private var appModel: AppModel
    @EnvironmentObject private var navigation: NavigationModel

    var body: some View {
        NavigationStack(path: $navigation.path) {
            VStack(alignment: .leading, spacing: 16) {
                Text("Open Chronicle")
                    .font(.largeTitle)
                Text(statusText)
                    .foregroundStyle(statusColor)
                Spacer()
            }
            .padding(32)
            .navigationDestination(for: AppRoute.self) { route in
                Text(title(for: route))
                    .navigationTitle(title(for: route))
            }
        }
    }

    private var statusText: String {
        switch appModel.health.status {
        case .connecting: "Connecting to the local Chronicle core…"
        case .ready: "Local Chronicle core ready"
        case let .repairRequired(message): "Core repair required: \(message)"
        }
    }

    private var statusColor: Color {
        switch appModel.health.status {
        case .connecting: .secondary
        case .ready: .green
        case .repairRequired: .orange
        }
    }

    private func title(for route: AppRoute) -> String {
        switch route {
        case .home: "Home"
        case .timeline: "Timeline"
        case let .chunk(id): "Chunk \(id)"
        case let .event(id): "Event \(id)"
        case let .analysis(id): "Analysis \(id)"
        case .settings: "Settings"
        }
    }
}

private struct ChronicleMenu: View {
    @Environment(\.openWindow) private var openWindow
    @EnvironmentObject private var appModel: AppModel

    var body: some View {
        Text(statusLabel)
        Divider()
        Button("Open Chronicle") {
            openWindow(id: "main")
            NSApp.activate(ignoringOtherApps: true)
        }
        Button("Quit Open Chronicle") {
            NSApp.terminate(nil)
        }
    }

    private var statusLabel: String {
        switch appModel.health.status {
        case .connecting: "Core: Connecting"
        case .ready: "Core: Ready"
        case .repairRequired: "Core: Repair Required"
        }
    }
}
