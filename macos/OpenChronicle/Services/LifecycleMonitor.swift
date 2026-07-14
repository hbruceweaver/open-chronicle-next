import AppKit
import Foundation

@MainActor
final class LifecycleMonitor {
    private let runtime: AppCaptureRuntime
    private var observations: [(NotificationCenter, NSObjectProtocol)] = []
    private var eventTask: Task<Void, Never>?

    init(runtime: AppCaptureRuntime) {
        self.runtime = runtime
    }

    func start(
        workspaceCenter: NotificationCenter = NSWorkspace.shared.notificationCenter,
        systemCenter: NotificationCenter = .default
    ) {
        guard observations.isEmpty else { return }
        observe(
            NSWorkspace.willSleepNotification,
            in: workspaceCenter,
            event: .willSleep
        )
        observe(
            NSWorkspace.didWakeNotification,
            in: workspaceCenter,
            event: .didWake
        )
        observe(
            NSWorkspace.sessionDidResignActiveNotification,
            in: workspaceCenter,
            event: .sessionResigned
        )
        observe(
            NSWorkspace.sessionDidBecomeActiveNotification,
            in: workspaceCenter,
            event: .sessionBecameActive
        )
        observe(
            Notification.Name.NSSystemClockDidChange,
            in: systemCenter,
            event: .wallClockChanged
        )
        observe(
            Notification.Name.NSSystemTimeZoneDidChange,
            in: systemCenter,
            event: .wallClockChanged
        )
    }

    func stop() {
        for (center, observation) in observations {
            center.removeObserver(observation)
        }
        observations.removeAll()
        eventTask?.cancel()
        eventTask = nil
    }

    func flush() async {
        await eventTask?.value
    }

    private func observe(
        _ name: Notification.Name,
        in center: NotificationCenter,
        event: AppLifecycleEvent
    ) {
        let token = center.addObserver(
            forName: name,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                self?.enqueue(event)
            }
        }
        observations.append((center, token))
    }

    private func enqueue(_ event: AppLifecycleEvent) {
        let previous = eventTask
        let runtime = runtime
        eventTask = Task {
            await previous?.value
            guard !Task.isCancelled else { return }
            await runtime.handle(event)
        }
    }

    deinit {
        for (center, observation) in observations {
            center.removeObserver(observation)
        }
        eventTask?.cancel()
    }
}
