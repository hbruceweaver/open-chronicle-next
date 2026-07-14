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
    @Published private(set) var operationalStorageState: OperationalStorageState = .healthy
    let healthViewModel = HealthViewModel()
    let homeViewModel = HomeViewModel()
    let timelineViewModel = TimelineViewModel()
    let analysisViewModel = AnalysisViewModel()
    private var core: (any CoreService)?
    private var runtime: AppCaptureRuntime?
    private var lifecycleMonitor: LifecycleMonitor?
    private var storageMonitor: StorageMonitor?
    private let notificationService: NotificationService
    private var latestDiagnosticHealth: DiagnosticHealthSnapshot?
    private var connectionTask: Task<Void, Never>?
    private var runtimeStartTask: Task<Void, Error>?
    private var onboardingCompletionTask: Task<Void, Error>?
    private var onboardingConfigurationInFlight: OnboardingRuntimeConfiguration?
    private var completedOnboardingConfiguration: OnboardingRuntimeConfiguration?
    private var isShuttingDown = false
    private let coreFactory: CoreFactory
    private let runtimeFactory: RuntimeFactory
    private let shouldStartCapture: () -> Bool
    private let duplicateInstanceHandler: DuplicateInstanceHandler

    init(
        coreFactory: CoreFactory? = nil,
        runtimeFactory: RuntimeFactory? = nil,
        shouldStartCapture: (() -> Bool)? = nil,
        notificationService: NotificationService? = nil,
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
        self.notificationService = notificationService ?? NotificationService()
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

    func connectAgent(_ installation: AgentInstallation) async -> AgentRegistrationOutcome {
        guard !isShuttingDown, let core else {
            return .failed(.clientUnavailable)
        }
        do {
            let managedRoot = try Self.applicationSupportURL()
            let helper = Bundle.main.bundleURL
                .appendingPathComponent("Contents/Helpers/chronicle-mcp", isDirectory: false)
            let connection = AgentConnectionService(
                grants: CoreDisclosureGrantService(core: core),
                registration: AgentRegistrationService(),
                applicationBundleURL: Bundle.main.bundleURL,
                helperURL: helper,
                managedRootURL: managedRoot
            )
            return await connection.connect(installation)
        } catch {
            return .failed(.clientUnavailable)
        }
    }

    func completeOnboarding(_ configuration: OnboardingRuntimeConfiguration) async throws {
        if let completedOnboardingConfiguration {
            guard completedOnboardingConfiguration == configuration else {
                throw AppModelOnboardingError.conflictingConfiguration
            }
            return
        }
        if let onboardingCompletionTask {
            guard onboardingConfigurationInFlight == configuration else {
                throw AppModelOnboardingError.conflictingConfiguration
            }
            try await onboardingCompletionTask.value
            return
        }
        guard !isShuttingDown, let core else {
            throw AppModelOnboardingError.coreUnavailable
        }
        let task = Task { @MainActor [weak self] in
            guard let self, !self.isShuttingDown else { throw CancellationError() }
            try await CoreOnboardingConfigurationService(core: core).apply(configuration)
            try await self.startCaptureRuntime(using: core)
        }
        onboardingConfigurationInFlight = configuration
        onboardingCompletionTask = task
        do {
            try await task.value
            completedOnboardingConfiguration = configuration
            onboardingCompletionTask = nil
            onboardingConfigurationInFlight = nil
        } catch {
            onboardingCompletionTask = nil
            onboardingConfigurationInFlight = nil
            throw error
        }
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
        onboardingCompletionTask?.cancel()
        if let onboardingCompletionTask {
            _ = try? await onboardingCompletionTask.value
        }
        self.onboardingCompletionTask = nil
        onboardingConfigurationInFlight = nil
        runtimeStartTask?.cancel()
        if let runtimeStartTask {
            _ = try? await runtimeStartTask.value
        }
        self.runtimeStartTask = nil
        lifecycleMonitor?.stop()
        lifecycleMonitor = nil
        await storageMonitor?.stop()
        storageMonitor = nil
        homeViewModel.detach()
        timelineViewModel.detach()
        analysisViewModel.detach()
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
                try await startCaptureRuntime(using: opened)
            } else {
                captureStatus = .setupRequired
            }
            homeViewModel.attach(client: CoreFactualReportClient(core: opened))
            timelineViewModel.attach(client: CoreTimelineEvidenceClient(core: opened))
            analysisViewModel.attach(client: CoreAnalysisEvidenceClient(core: opened))
            let diagnosticClient = CoreDiagnosticHealthClient(core: opened)
            healthViewModel.attach(fetcher: diagnosticClient)
            let monitor = StorageMonitor(fetcher: diagnosticClient) { [weak self] update in
                await self?.applyStorageMonitorUpdate(update)
            }
            storageMonitor = monitor
            await monitor.start()
            guard !isShuttingDown else {
                await monitor.stop()
                storageMonitor = nil
                try? await opened.close()
                return
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

    private func updateCaptureStatus(_ status: CapturePresentationState) async {
        captureStatus = status
        await notificationService.evaluate(
            captureStatus: status,
            health: latestDiagnosticHealth
        )
    }

    private func startCaptureRuntime(using core: any CoreService) async throws {
        if runtime != nil { return }
        if let runtimeStartTask {
            try await runtimeStartTask.value
            return
        }

        let task = Task { @MainActor [weak self] in
            guard let self, !self.isShuttingDown else { throw CancellationError() }
            let created = try await self.runtimeFactory(core) { [weak self] status in
                await self?.updateCaptureStatus(status)
            }
            guard !self.isShuttingDown else { throw CancellationError() }
            try await created.start()
            guard !self.isShuttingDown else {
                try? await created.shutdown()
                throw CancellationError()
            }
            let monitor = LifecycleMonitor(runtime: created)
            monitor.start()
            self.runtime = created
            self.lifecycleMonitor = monitor
        }
        runtimeStartTask = task
        do {
            try await task.value
            runtimeStartTask = nil
        } catch {
            runtimeStartTask = nil
            throw error
        }
    }

    /// Internal so the authoritative health-to-presentation fan-out can be
    /// integration tested without waiting for the monitor timer.
    func applyStorageMonitorUpdate(_ update: StorageMonitorUpdate) async {
        guard !isShuttingDown else { return }
        switch update {
        case let .snapshot(snapshot):
            latestDiagnosticHealth = snapshot
            operationalStorageState = OperationalStoragePolicy.state(for: snapshot)
            healthViewModel.apply(snapshot)
            let latestProjectionAt = snapshot.latest.lastProjectionAt.flatMap(ChronicleTimestamp.date)
            homeViewModel.observe(latestProjectionAt: latestProjectionAt)
            timelineViewModel.observe(latestProjectionAt: latestProjectionAt)
            await notificationService.evaluate(
                captureStatus: captureStatus,
                health: snapshot
            )
        case let .failed(message):
            healthViewModel.fail(message)
        }
    }

    private static func isDuplicateInstance(_ error: Error) -> Bool {
        guard case let ChronicleBridgeError.bridgeStatus(_, payload) = error else {
            return false
        }
        return payload?.code == "capture-owner-active"
    }
}

enum AppModelOnboardingError: LocalizedError {
    case coreUnavailable
    case conflictingConfiguration

    var errorDescription: String? {
        switch self {
        case .coreUnavailable:
            "The local Chronicle core is still connecting. Wait a moment and retry."
        case .conflictingConfiguration:
            "A different onboarding configuration is already being applied."
        }
    }
}
