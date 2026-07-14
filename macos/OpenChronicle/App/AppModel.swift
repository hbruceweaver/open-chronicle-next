import Foundation

@MainActor
final class AppModel: ObservableObject {
    typealias CoreFactory = @Sendable (URL) async throws -> any CoreService
    typealias RuntimeFactory = @MainActor (
        any CoreService,
        @escaping AppCaptureRuntime.StatusSink
    ) async throws -> AppCaptureRuntime
    typealias DuplicateInstanceHandler = @MainActor @Sendable () async -> Void

    @Published private(set) var health = ChronicleHealthState(status: .connecting)
    @Published private(set) var captureStatus: CapturePresentationState = .starting
    private var core: (any CoreService)?
    private var runtime: AppCaptureRuntime?
    private var lifecycleMonitor: LifecycleMonitor?
    private var connectionTask: Task<Void, Never>?
    private var isShuttingDown = false
    private let coreFactory: CoreFactory
    private let runtimeFactory: RuntimeFactory
    private let shouldStartCapture: () -> Bool
    private let duplicateInstanceHandler: DuplicateInstanceHandler

    init(
        coreFactory: CoreFactory? = nil,
        runtimeFactory: RuntimeFactory? = nil,
        shouldStartCapture: (() -> Bool)? = nil,
        duplicateInstanceHandler: @escaping DuplicateInstanceHandler = {}
    ) {
        self.coreFactory = coreFactory ?? { supportURL in
            try await Task.detached(priority: .userInitiated) {
                try InProcessCore(applicationSupportURL: supportURL)
            }.value
        }
        self.runtimeFactory = runtimeFactory ?? { core, statusSink in
            try await AppRuntimeFactory.make(core: core, statusSink: statusSink)
        }
        self.shouldStartCapture = shouldStartCapture ?? {
            AppRuntimeFactory.hasCompletedOnboarding()
        }
        self.duplicateInstanceHandler = duplicateInstanceHandler
    }

    func connect() async {
        guard !isShuttingDown, core == nil else { return }

        if let connectionTask {
            await connectionTask.value
            return
        }

        let task = Task { await performConnection() }
        connectionTask = task
        await task.value
        connectionTask = nil
    }

    func setRecordingEnabled(_ enabled: Bool) async {
        await runtime?.setRecordingEnabled(enabled)
    }

    func retryStorageRecovery() async {
        await runtime?.retryStorageRecovery()
    }

    func shutdown() async {
        guard !isShuttingDown else {
            await connectionTask?.value
            return
        }
        isShuttingDown = true
        connectionTask?.cancel()
        await connectionTask?.value
        connectionTask = nil
        lifecycleMonitor?.stop()
        lifecycleMonitor = nil
        if let runtime {
            try? await runtime.shutdown()
        }
        self.runtime = nil
        if let core {
            try? await core.close()
        }
        self.core = nil
        captureStatus = .stopped
    }

    private func performConnection() async {
        var openedCore: (any CoreService)?
        do {
            let supportURL = try Self.applicationSupportURL()
            let opened = try await coreFactory(supportURL)
            openedCore = opened
            guard !isShuttingDown else {
                try? await opened.close()
                return
            }
            let identity = try await opened.schemaIdentity()
            guard !isShuttingDown else {
                try? await opened.close()
                return
            }
            guard identity.abiSchemaVersion.hasPrefix("1.") else {
                try? await opened.close()
                openedCore = nil
                health = ChronicleHealthState(
                    status: .repairRequired("Core schema \(identity.abiSchemaVersion) is incompatible.")
                )
                return
            }
            if shouldStartCapture() {
                let runtime = try await runtimeFactory(opened) { [weak self] status in
                    await self?.updateCaptureStatus(status)
                }
                guard !isShuttingDown else {
                    try? await opened.close()
                    return
                }
                try await runtime.start()
                guard !isShuttingDown else {
                    try? await runtime.shutdown()
                    try? await opened.close()
                    return
                }
                let monitor = LifecycleMonitor(runtime: runtime)
                monitor.start()
                self.runtime = runtime
                lifecycleMonitor = monitor
            } else {
                captureStatus = .setupRequired
            }
            core = opened
            openedCore = nil
            health = ChronicleHealthState(status: .ready)
        } catch {
            if let openedCore {
                try? await openedCore.close()
            }
            if Self.isDuplicateInstance(error) {
                captureStatus = .stopped
                await duplicateInstanceHandler()
                return
            }
            health = ChronicleHealthState(status: .repairRequired(error.localizedDescription))
        }
    }

    private static func applicationSupportURL() throws -> URL {
        let base = try FileManager.default.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: true
        )
        let identifier = Bundle.main.bundleIdentifier ?? "com.screenata.openchronicle"
        return base.appendingPathComponent(identifier, isDirectory: true)
    }

    private func updateCaptureStatus(_ status: CapturePresentationState) {
        captureStatus = status
    }

    private static func isDuplicateInstance(_ error: Error) -> Bool {
        guard case let ChronicleBridgeError.bridgeStatus(_, payload) = error else {
            return false
        }
        return payload?.code == "capture-owner-active"
    }
}
