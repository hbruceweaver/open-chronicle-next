import AppKit
import Carbon
import SwiftUI

@main
struct OpenChronicleApp: App {
    @NSApplicationDelegateAdaptor(AppLifecycleDelegate.self) private var appDelegate
    @StateObject private var navigation = NavigationModel()

    var body: some Scene {
        Window("Open Chronicle", id: "main") {
            ShellView()
                .environmentObject(appDelegate.appModel)
                .environmentObject(navigation)
                .frame(minWidth: 720, minHeight: 480)
        }
        .defaultSize(width: 960, height: 680)

        MenuBarExtra {
            ChronicleMenu(onOpen: appDelegate.prepareForExplicitOpen)
                .environmentObject(appDelegate.appModel)
        } label: {
            ChronicleMenuLabel(appModel: appDelegate.appModel)
        }
    }
}

private struct ChronicleMenuLabel: View {
    @Environment(\.openWindow) private var openWindow
    @ObservedObject var appModel: AppModel

    var body: some View {
        Label("Open Chronicle", systemImage: symbol)
            .onReceive(NotificationCenter.default.publisher(for: .openChronicleMainWindow)) { _ in
                openWindow(id: "main")
                NSApp.activate(ignoringOtherApps: true)
            }
    }

    private var symbol: String {
        switch appModel.health.status {
        case .connecting: "clock"
        case .repairRequired: "exclamationmark.triangle"
        case .ready:
            switch appModel.captureStatus {
            case .recording: "record.circle.fill"
            case .paused, .setupRequired, .stopped: "pause.circle"
            case .protected: "eye.slash"
            case .unavailable: "exclamationmark.circle"
            case .sleeping: "moon.zzz"
            case .studyNotStarted, .studyExpired: "calendar.badge.exclamationmark"
            case .storageBlocked, .repairRequired: "exclamationmark.triangle"
            case .starting: "clock"
            }
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
                Text(captureStatusText)
                    .foregroundStyle(.secondary)
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

    private var captureStatusText: String {
        switch appModel.captureStatus {
        case .setupRequired: "Complete setup before observation starts"
        case .starting: "Observation engine starting…"
        case .recording: "Observation is active"
        case .paused: "Observation is paused"
        case .protected: "The current surface is protected; no pixels or text were retained"
        case let .unavailable(reason): "Observation is unavailable: \(reason.displayName)"
        case .sleeping: "Observation is suspended while this Mac sleeps"
        case .studyNotStarted: "The configured study has not started"
        case .studyExpired: "The configured study has ended"
        case .storageBlocked: "Observation is paused until storage recovers"
        case .stopped: "Observation engine stopped"
        case let .repairRequired(message): "Observation repair required: \(message)"
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
    let onOpen: @MainActor () -> Void

    var body: some View {
        Text(statusLabel)
        Divider()
        if appModel.captureStatus == .recording {
            Button("Pause Observation") {
                Task { await appModel.setRecordingEnabled(false) }
            }
        } else if appModel.captureStatus == .paused {
            Button("Resume Observation") {
                Task { await appModel.setRecordingEnabled(true) }
            }
        }
        Button("Open Chronicle") {
            onOpen()
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
        case .repairRequired: "Core: Repair Required"
        case .ready:
            switch appModel.captureStatus {
            case .setupRequired: "Setup required"
            case .starting: "Observation starting"
            case .recording: "Observation active"
            case .paused: "Observation paused"
            case .protected: "Current surface protected"
            case .unavailable: "Observation unavailable"
            case .sleeping: "Sleeping"
            case .studyNotStarted: "Study not started"
            case .studyExpired: "Study ended"
            case .storageBlocked: "Storage blocked"
            case .stopped: "Observation stopped"
            case .repairRequired: "Observation repair required"
            }
        }
    }
}

private extension CaptureDenial {
    var displayName: String {
        switch self {
        case .permissionDenied: "Screen Recording permission is unavailable"
        case .asleep: "this Mac is asleep"
        case .noExactWindow: "no exact foreground window was found"
        case .ambiguousWindow: "the foreground window could not be identified safely"
        case .foregroundChanged: "the foreground window changed during observation"
        case .userPaused: "observation is paused"
        case .studyExpired: "the study has ended"
        case .locked: "the session is locked"
        case .secureInput: "secure input is active"
        case .applicationExcluded: "the current application is excluded"
        case .titleExcluded: "the current window is excluded"
        case .chronicleSelf: "Open Chronicle excludes itself"
        }
    }
}

@MainActor
final class AppLifecycleDelegate: NSObject, NSApplicationDelegate {
    static let activationRequest = Notification.Name(
        "com.screenata.openchronicle.activation-request"
    )

    lazy var appModel = AppModel(duplicateInstanceHandler: { [weak self] in
        self?.requestAuthoritativeInstanceActivation()
    })
    private var terminationInFlight = false
    private var suppressInitialWindows = false
    private var windowVisibilityObserver: NSObjectProtocol?

    func applicationWillFinishLaunching(_ notification: Notification) {
        suppressInitialWindows = NSAppleEventManager.shared()
            .currentAppleEvent?
            .paramDescriptor(forKeyword: AEKeyword(keyAELaunchedAsLogInItem)) != nil
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        DistributedNotificationCenter.default().addObserver(
            self,
            selector: #selector(handleActivationRequest(_:)),
            name: Self.activationRequest,
            object: nil,
            suspensionBehavior: .deliverImmediately
        )
        if suppressInitialWindows {
            windowVisibilityObserver = NotificationCenter.default.addObserver(
                forName: NSWindow.didBecomeKeyNotification,
                object: nil,
                queue: .main
            ) { [weak self] notification in
                Task { @MainActor [weak self] in
                    self?.suppressWindowIfNeeded(notification)
                }
            }
            hideMainWindows()
        }
        Task { await appModel.connect() }
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard !terminationInFlight else { return .terminateLater }
        terminationInFlight = true
        Task {
            await appModel.shutdown()
            sender.reply(toApplicationShouldTerminate: true)
        }
        return .terminateLater
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }

    func applicationShouldHandleReopen(
        _ sender: NSApplication,
        hasVisibleWindows flag: Bool
    ) -> Bool {
        presentMainWindow()
        return true
    }

    func prepareForExplicitOpen() {
        suppressInitialWindows = false
        if let observer = windowVisibilityObserver {
            NotificationCenter.default.removeObserver(observer)
            windowVisibilityObserver = nil
        }
    }

    @objc private func handleActivationRequest(_ notification: Notification) {
        let ownProcess = String(ProcessInfo.processInfo.processIdentifier)
        guard notification.object as? String != ownProcess else { return }
        presentMainWindow()
    }

    private func presentMainWindow() {
        prepareForExplicitOpen()
        if let window = NSApp.windows.first(where: \.canBecomeMain) {
            window.makeKeyAndOrderFront(nil)
        } else {
            NotificationCenter.default.post(name: .openChronicleMainWindow, object: nil)
        }
        NSApp.activate(ignoringOtherApps: true)
    }

    private func requestAuthoritativeInstanceActivation() {
        DistributedNotificationCenter.default().postNotificationName(
            Self.activationRequest,
            object: String(ProcessInfo.processInfo.processIdentifier),
            userInfo: nil,
            deliverImmediately: true
        )
        Task { @MainActor in
            await Task.yield()
            NSApp.terminate(nil)
        }
    }

    private func hideMainWindows() {
        for window in NSApp.windows where window.canBecomeMain {
            window.orderOut(nil)
        }
    }

    private func suppressWindowIfNeeded(_ notification: Notification) {
        guard suppressInitialWindows,
              let window = notification.object as? NSWindow,
              window.canBecomeMain
        else { return }
        window.orderOut(nil)
    }
}

private extension Notification.Name {
    static let openChronicleMainWindow = Notification.Name(
        "com.screenata.openchronicle.open-main-window"
    )
}
