import AppKit
import Foundation
import XCTest
@testable import OpenChronicle

final class AppRuntimeTests: XCTestCase {
    func testCoreRuntimeControllerUsesVersionedRuntimeAndLifecycleControls() async throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let now = Date(timeIntervalSince1970: 1_784_016_000)
        let core = try InProcessCore(applicationSupportURL: directory, now: now)
        let control = CoreAppRuntimeController(
            core: core,
            deviceID: "device-runtime-control",
            displayTimezone: "Europe/Zurich"
        )

        let configuration = try await control.runtimeConfiguration(at: now)
        XCTAssertFalse(configuration.recordingEnabled)
        XCTAssertEqual(configuration.cadenceSeconds, 60)
        try await control.startupReconcile(sessionID: "session-runtime-control", at: now)
        try await control.prepareTermination(
            sessionID: "session-runtime-control",
            at: now.addingTimeInterval(60)
        )
        try await core.close()
    }

    @MainActor
    func testLifecycleMonitorSerializesWorkspaceAndClockNotifications() async throws {
        let coordinator = CoordinatorProbe()
        let runtime = AppCaptureRuntime(
            sessionID: "session-monitor",
            recordingEnabled: true,
            control: RuntimeControlProbe(),
            coordinator: coordinator,
            environment: SystemCaptureEnvironmentSource(),
            statusSink: { _ in }
        )
        try await runtime.start()
        let workspace = NotificationCenter()
        let system = NotificationCenter()
        let monitor = LifecycleMonitor(runtime: runtime)
        monitor.start(workspaceCenter: workspace, systemCenter: system)

        workspace.post(name: NSWorkspace.willSleepNotification, object: nil)
        workspace.post(name: NSWorkspace.didWakeNotification, object: nil)
        workspace.post(name: NSWorkspace.sessionDidResignActiveNotification, object: nil)
        workspace.post(name: NSWorkspace.sessionDidBecomeActiveNotification, object: nil)
        system.post(name: Notification.Name.NSSystemClockDidChange, object: nil)
        system.post(name: Notification.Name.NSSystemTimeZoneDidChange, object: nil)
        await Task.yield()
        await Task.yield()
        await monitor.flush()
        monitor.stop()

        let calls = await coordinator.calls
        XCTAssertEqual(calls, [
            .start,
            .suspend,
            .resume,
            .privacyBoundary,
            .privacyBoundary,
            .wallClockChanged,
            .wallClockChanged,
        ])
    }

    @MainActor
    func testLifecycleMonitorPreservesOrderAcrossSynchronousNotificationBursts() async throws {
        let coordinator = CoordinatorProbe()
        let runtime = AppCaptureRuntime(
            sessionID: "session-monitor-burst",
            recordingEnabled: true,
            control: RuntimeControlProbe(),
            coordinator: coordinator,
            environment: SystemCaptureEnvironmentSource(),
            statusSink: { _ in }
        )
        try await runtime.start()
        let workspace = NotificationCenter()
        let system = NotificationCenter()
        let monitor = LifecycleMonitor(runtime: runtime)
        monitor.start(workspaceCenter: workspace, systemCenter: system)

        for _ in 0..<20 {
            workspace.post(name: NSWorkspace.willSleepNotification, object: nil)
            workspace.post(name: NSWorkspace.didWakeNotification, object: nil)
            workspace.post(name: NSWorkspace.sessionDidResignActiveNotification, object: nil)
            workspace.post(name: NSWorkspace.sessionDidBecomeActiveNotification, object: nil)
            system.post(name: Notification.Name.NSSystemClockDidChange, object: nil)
        }
        await monitor.flush()
        monitor.stop()

        let calls = await coordinator.calls
        let expectedBurst: [CoordinatorCall] = [
            .suspend,
            .resume,
            .privacyBoundary,
            .privacyBoundary,
            .wallClockChanged,
        ]
        XCTAssertEqual(calls, [.start] + Array(repeating: expectedBurst, count: 20).flatMap { $0 })
    }

    func testLifecycleEventsUpdatePrivacyStateAndCoordinatorInOrder() async throws {
        let control = RuntimeControlProbe()
        let coordinator = CoordinatorProbe()
        let environment = SystemCaptureEnvironmentSource()
        let statuses = StatusProbe()
        let runtime = AppCaptureRuntime(
            sessionID: "session-lifecycle",
            recordingEnabled: true,
            control: control,
            coordinator: coordinator,
            environment: environment,
            statusSink: { state in await statuses.append(state) }
        )
        let startedAt = Date(timeIntervalSince1970: 1_784_016_000)

        try await runtime.start(at: startedAt)
        await runtime.handle(.sessionResigned)
        var privacy = await environment.currentEnvironment()
        XCTAssertTrue(privacy.isLocked)
        await runtime.handle(.sessionBecameActive)
        privacy = await environment.currentEnvironment()
        XCTAssertFalse(privacy.isLocked)
        await runtime.handle(.willSleep)
        privacy = await environment.currentEnvironment()
        XCTAssertTrue(privacy.isAsleep)
        await runtime.handle(.didWake)
        privacy = await environment.currentEnvironment()
        XCTAssertFalse(privacy.isAsleep)
        await runtime.handle(.wallClockChanged)
        await runtime.setRecordingEnabled(false)
        privacy = await environment.currentEnvironment()
        XCTAssertTrue(privacy.isPaused)
        try await runtime.shutdown(at: startedAt.addingTimeInterval(60))

        let controlCalls = await control.calls
        XCTAssertEqual(controlCalls, [
            .startup("session-lifecycle", startedAt),
            .termination("session-lifecycle", startedAt.addingTimeInterval(60)),
        ])
        let coordinatorCalls = await coordinator.calls
        XCTAssertEqual(coordinatorCalls, [
            .start,
            .privacyBoundary,
            .privacyBoundary,
            .suspend,
            .resume,
            .wallClockChanged,
            .recordingPreference(false),
            .stop,
        ])
        let published = await statuses.values
        XCTAssertEqual(published, [
            .starting,
            .recording,
            .protected,
            .recording,
            .sleeping,
            .recording,
            .paused,
            .stopped,
            .stopped,
        ])
    }

    func testStorageRetryAndRepairStateArePublishedHonestly() async throws {
        let control = RuntimeControlProbe()
        let coordinator = CoordinatorProbe()
        let statuses = StatusProbe()
        let runtime = AppCaptureRuntime(
            sessionID: "session-storage",
            recordingEnabled: true,
            control: control,
            coordinator: coordinator,
            environment: SystemCaptureEnvironmentSource(),
            statusSink: { state in await statuses.append(state) }
        )
        try await runtime.start()

        await coordinator.setState(.storageFailure)
        await runtime.handle(.sessionResigned)
        var lastStatus = await statuses.last
        XCTAssertEqual(lastStatus, .storageBlocked)
        await runtime.handle(.sessionBecameActive)
        await runtime.retryStorageRecovery()
        lastStatus = await statuses.last
        XCTAssertEqual(lastStatus, .recording)

        await coordinator.setState(.repairRequired(CapturePersistenceFailure(
            category: .contractRepair,
            code: "fixture-repair",
            retryable: false
        )))
        await runtime.handle(.sessionResigned)
        lastStatus = await statuses.last
        XCTAssertEqual(lastStatus, .repairRequired("fixture-repair"))
        try await runtime.shutdown()
    }

    func testSchedulerDrivenDenialsAndTerminalStatesUpdatePresentation() async throws {
        let coordinator = CoordinatorProbe()
        let statuses = StatusProbe()
        let runtime = AppCaptureRuntime(
            sessionID: "session-async-status",
            recordingEnabled: true,
            control: RuntimeControlProbe(),
            coordinator: coordinator,
            environment: SystemCaptureEnvironmentSource(),
            statusSink: { state in await statuses.append(state) }
        )
        try await runtime.start()

        await coordinator.emitDenial(.permissionDenied)
        var lastStatus = await statuses.last
        XCTAssertEqual(lastStatus, .unavailable(.permissionDenied))
        await coordinator.emitDenial(.applicationExcluded)
        lastStatus = await statuses.last
        XCTAssertEqual(lastStatus, .protected)
        await coordinator.setState(.studyExpired)
        lastStatus = await statuses.last
        XCTAssertEqual(lastStatus, .studyExpired)
        await coordinator.setState(.storageFailure)
        lastStatus = await statuses.last
        XCTAssertEqual(lastStatus, .storageBlocked)
        try await runtime.shutdown()
    }

    @MainActor
    func testAppModelDoesNotConstructCaptureRuntimeBeforeConsent() async {
        let core = AppCoreProbe()
        let runtimeFactory = RuntimeFactoryProbe()
        let model = AppModel(
            coreFactory: { _ in core },
            runtimeFactory: { _, _ in try await runtimeFactory.make() },
            shouldStartCapture: { false }
        )

        await model.connect()

        XCTAssertEqual(model.health.status, .ready)
        XCTAssertEqual(model.captureStatus, .setupRequired)
        let runtimeFactoryCalls = await runtimeFactory.calls
        XCTAssertEqual(runtimeFactoryCalls, 0)
        await model.shutdown()
        let closeCount = await core.closeCount
        XCTAssertEqual(closeCount, 1)
    }

    @MainActor
    func testCompletionCommitsCoreChoicesAndStartsRuntimeUsingExistingCore() async throws {
        let core = OnboardingCoreProbe()
        let runtimeFactory = SuccessfulRuntimeFactoryProbe()
        let model = AppModel(
            coreFactory: { _ in core },
            runtimeFactory: { _, statusSink in
                await runtimeFactory.make(statusSink: statusSink)
            },
            shouldStartCapture: { false }
        )
        await model.connect()
        XCTAssertEqual(model.captureStatus, .setupRequired)

        try await model.completeOnboarding(testOnboardingConfiguration())

        let controls = await core.controlTypesValue()
        XCTAssertEqual(controls, [
            "set-cadence",
            "set-screenshot-retention",
            "use-personal-mode",
            "set-recording-preference",
        ])
        let runtimeCalls = await runtimeFactory.callsValue()
        XCTAssertEqual(runtimeCalls, 1)
        XCTAssertEqual(model.captureStatus, .recording)
        await model.shutdown()
    }

    @MainActor
    func testConcurrentCompletionConstructsExactlyOneRuntime() async throws {
        let core = OnboardingCoreProbe()
        let runtimeFactory = SuccessfulRuntimeFactoryProbe()
        let model = AppModel(
            coreFactory: { _ in core },
            runtimeFactory: { _, statusSink in
                await runtimeFactory.make(statusSink: statusSink)
            },
            shouldStartCapture: { false }
        )
        await model.connect()
        let configuration = testOnboardingConfiguration()

        async let first: Void = model.completeOnboarding(configuration)
        async let second: Void = model.completeOnboarding(configuration)
        _ = try await (first, second)

        let runtimeCalls = await runtimeFactory.callsValue()
        XCTAssertEqual(runtimeCalls, 1)
        let controls = await core.controlTypesValue()
        XCTAssertEqual(controls.count, 4)
        await model.shutdown()
    }

    @MainActor
    func testShutdownWaitsForSuspendedOpenAndClosesLateCore() async {
        let core = AppCoreProbe()
        let factory = BlockingCoreFactory(core: core)
        let model = AppModel(
            coreFactory: { _ in try await factory.open() },
            shouldStartCapture: { false }
        )

        let connection = Task { await model.connect() }
        await factory.waitUntilStarted()
        let shutdown = Task { await model.shutdown() }
        await Task.yield()
        var closeCount = await core.closeCountValue()
        XCTAssertEqual(closeCount, 0)
        await factory.release()
        await shutdown.value
        await connection.value

        closeCount = await core.closeCountValue()
        XCTAssertEqual(closeCount, 1)
        XCTAssertEqual(model.captureStatus, .stopped)
        await model.connect()
        let openCount = await factory.openCountValue()
        XCTAssertEqual(openCount, 1)
    }

    @MainActor
    func testDuplicateCaptureOwnerRequestsActivationInsteadOfRepair() async {
        let activation = ActivationProbe()
        let model = AppModel(
            coreFactory: { _ in
                throw ChronicleBridgeError.bridgeStatus(
                    1,
                    ChronicleErrorPayload(
                        code: "capture-owner-active",
                        message: "another owner is active",
                        retryable: false
                    )
                )
            },
            shouldStartCapture: { false },
            duplicateInstanceHandler: { await activation.record() }
        )

        await model.connect()

        let activationCount = await activation.countValue()
        XCTAssertEqual(activationCount, 1)
        XCTAssertEqual(model.captureStatus, .stopped)
        XCTAssertEqual(model.health.status, .connecting)
    }

    private func testOnboardingConfiguration() -> OnboardingRuntimeConfiguration {
        OnboardingRuntimeConfiguration(
            recordingMode: .personal,
            cadenceSeconds: 30,
            screenshotRetentionSeconds: 7 * 24 * 60 * 60,
            studyStart: Date(timeIntervalSince1970: 1_784_016_000),
            studyEnd: Date(timeIntervalSince1970: 1_786_608_000)
        )
    }
}

private enum RuntimeControlCall: Equatable, Sendable {
    case startup(String, Date)
    case termination(String, Date)
}

private actor RuntimeControlProbe: AppRuntimeControlling {
    private(set) var calls: [RuntimeControlCall] = []

    func runtimeConfiguration(at _: Date) -> AppRuntimeConfiguration {
        AppRuntimeConfiguration(
            recordingEnabled: true,
            cadenceSeconds: 30,
            screenshotRetentionSeconds: 24 * 60 * 60
        )
    }

    func startupReconcile(sessionID: String, at date: Date) {
        calls.append(.startup(sessionID, date))
    }

    func prepareTermination(sessionID: String, at date: Date) {
        calls.append(.termination(sessionID, date))
    }
}

private enum CoordinatorCall: Equatable, Sendable {
    case start
    case stop
    case suspend
    case resume
    case storageRecovery
    case wallClockChanged
    case recordingPreference(Bool)
    case privacyBoundary
}

private actor CoordinatorProbe: CaptureCoordinating {
    private(set) var calls: [CoordinatorCall] = []
    private var state: CaptureCoordinatorState = .stopped
    private var generation: UInt64 = 0
    private var lastDenial: CaptureDenial?
    private var updateSink: CaptureCoordinatorUpdateSink?

    func setUpdateSink(_ sink: @escaping CaptureCoordinatorUpdateSink) async {
        updateSink = sink
    }

    func start() async {
        calls.append(.start)
        generation += 1
        state = .running
        await publish()
    }

    func stop() async {
        calls.append(.stop)
        state = .stopped
        await publish()
    }

    func suspend() async {
        calls.append(.suspend)
        state = .suspended
        await publish()
    }

    func resume() async {
        calls.append(.resume)
        generation += 1
        state = .running
        await publish()
    }

    func resumeAfterStorageRecovery() async {
        calls.append(.storageRecovery)
        generation += 1
        state = .running
        await publish()
    }

    func wallClockChanged() {
        calls.append(.wallClockChanged)
    }

    func recordingPreferenceChanged(enabled: Bool) {
        calls.append(.recordingPreference(enabled))
    }

    func privacyBoundaryChanged() async {
        calls.append(.privacyBoundary)
        lastDenial = nil
        await publish()
    }

    func snapshot() -> CaptureCoordinatorSnapshot {
        CaptureCoordinatorSnapshot(
            state: state,
            nextTick: nil,
            attemptInFlight: false,
            executionGeneration: generation,
            lastDenial: lastDenial
        )
    }

    func setState(_ state: CaptureCoordinatorState) async {
        self.state = state
        lastDenial = nil
        await publish()
    }

    func emitDenial(_ denial: CaptureDenial) async {
        state = .running
        lastDenial = denial
        await publish()
    }

    private func publish() async {
        guard let updateSink else { return }
        await updateSink(snapshot())
    }
}

private actor StatusProbe {
    private(set) var values: [CapturePresentationState] = []
    var last: CapturePresentationState? { values.last }

    func append(_ value: CapturePresentationState) {
        values.append(value)
    }
}

private actor RuntimeFactoryProbe {
    private(set) var calls = 0

    func make() throws -> AppCaptureRuntime {
        calls += 1
        throw AppRuntimeTestError.unexpectedFactoryCall
    }
}

private actor SuccessfulRuntimeFactoryProbe {
    private var calls = 0

    func make(statusSink: @escaping AppCaptureRuntime.StatusSink) -> AppCaptureRuntime {
        calls += 1
        return AppCaptureRuntime(
            sessionID: "session-onboarding-runtime",
            recordingEnabled: true,
            control: RuntimeControlProbe(),
            coordinator: CoordinatorProbe(),
            environment: SystemCaptureEnvironmentSource(),
            statusSink: statusSink
        )
    }

    func callsValue() -> Int { calls }
}

private actor AppCoreProbe: CoreService {
    private(set) var closeCount = 0

    func openedStoreGeneration() -> UInt64 { 1 }

    func schemaIdentity() -> ChronicleSchemaIdentity {
        ChronicleSchemaIdentity(abiSchemaVersion: "1.0", contractSchemaVersion: "1.0")
    }

    func call(_: Data) -> Data { Data() }
    func ingest(_: Data, image _: Data?) -> Data { Data() }
    func imageRead(artifactID _: String, generation _: UInt64, maxBytes _: UInt64) -> Data {
        Data()
    }

    func close() {
        closeCount += 1
    }

    func closeCountValue() -> Int { closeCount }
}

private actor OnboardingCoreProbe: CoreService {
    private var controlTypes: [String] = []
    private var closeCount = 0

    func openedStoreGeneration() -> UInt64 { 1 }

    func schemaIdentity() -> ChronicleSchemaIdentity {
        ChronicleSchemaIdentity(abiSchemaVersion: "1.0", contractSchemaVersion: "1.0")
    }

    func call(_ request: Data) -> Data {
        guard let object = try? JSONSerialization.jsonObject(with: request) as? [String: Any],
              let control = object["control"] as? [String: Any],
              let type = control["type"] as? String
        else {
            return Self.response(result: ["state": "healthy"])
        }
        controlTypes.append(type)
        let result: [String: Any]
        switch type {
        case "set-cadence":
            result = ["cadence": control["cadence"] ?? "sixty-seconds"]
        case "set-screenshot-retention":
            result = [
                "screenshot_retention": control["retention"] ?? "twenty-four-hours",
            ]
        case "use-personal-mode":
            result = ["mode": "personal"]
        case "configure-study":
            result = [
                "start": control["start"] ?? "",
                "end": control["end"] ?? "",
            ]
        case "set-recording-preference":
            result = ["recording_preference": control["enabled"] ?? false]
        default:
            result = [:]
        }
        return Self.response(result: result)
    }

    func ingest(_: Data, image _: Data?) -> Data { Data() }

    func imageRead(artifactID _: String, generation _: UInt64, maxBytes _: UInt64) -> Data {
        Data()
    }

    func close() { closeCount += 1 }
    func controlTypesValue() -> [String] { controlTypes }

    private static func response(result: [String: Any]) -> Data {
        (try? JSONSerialization.data(withJSONObject: [
            "schema_version": "1.0",
            "ok": true,
            "result": result,
            "error": NSNull(),
        ])) ?? Data()
    }
}

private actor BlockingCoreFactory {
    private let core: AppCoreProbe
    private var started = false
    private var released = false
    private var startWaiters: [CheckedContinuation<Void, Never>] = []
    private var releaseWaiters: [CheckedContinuation<Void, Never>] = []
    private var openCount = 0

    init(core: AppCoreProbe) {
        self.core = core
    }

    func open() async throws -> any CoreService {
        openCount += 1
        started = true
        for waiter in startWaiters { waiter.resume() }
        startWaiters.removeAll()
        if !released {
            await withCheckedContinuation { releaseWaiters.append($0) }
        }
        return core
    }

    func waitUntilStarted() async {
        if started { return }
        await withCheckedContinuation { startWaiters.append($0) }
    }

    func release() {
        released = true
        for waiter in releaseWaiters { waiter.resume() }
        releaseWaiters.removeAll()
    }

    func openCountValue() -> Int { openCount }
}

private actor ActivationProbe {
    private var count = 0
    func record() { count += 1 }
    func countValue() -> Int { count }
}

private enum AppRuntimeTestError: Error {
    case unexpectedFactoryCall
}
