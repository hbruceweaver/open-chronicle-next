import Foundation

enum AppLifecycleEvent: Equatable, Sendable {
    case willSleep
    case didWake
    case sessionResigned
    case sessionBecameActive
    case wallClockChanged
}

enum CapturePresentationState: Equatable, Sendable {
    case setupRequired
    case starting
    case recording
    case paused
    case protected
    case unavailable(CaptureDenial)
    case sleeping
    case studyNotStarted
    case studyExpired
    case storageBlocked
    case stopped
    case repairRequired(String)
}

struct AppRuntimeConfiguration: Equatable, Sendable {
    let recordingEnabled: Bool
    let cadenceSeconds: UInt32
    let screenshotRetentionSeconds: UInt32
}

protocol AppRuntimeControlling: Sendable {
    func runtimeConfiguration(at: Date) async throws -> AppRuntimeConfiguration
    func startupReconcile(sessionID: String, at: Date) async throws
    func prepareTermination(sessionID: String, at: Date) async throws
}

protocol CaptureCoordinating: Sendable {
    func setUpdateSink(_ sink: @escaping CaptureCoordinatorUpdateSink) async
    func start() async
    func stop() async
    func suspend() async
    func resume() async
    func resumeAfterStorageRecovery() async
    func wallClockChanged() async
    func recordingPreferenceChanged(enabled: Bool) async
    func privacyBoundaryChanged() async
    func snapshot() async -> CaptureCoordinatorSnapshot
}

extension CaptureCoordinator: CaptureCoordinating {}

actor CoreAppRuntimeController: AppRuntimeControlling {
    private let core: any CoreService
    private let deviceID: String
    private let displayTimezone: String

    init(core: any CoreService, deviceID: String, displayTimezone: String) {
        self.core = core
        self.deviceID = deviceID
        self.displayTimezone = displayTimezone
    }

    func runtimeConfiguration(at date: Date) async throws -> AppRuntimeConfiguration {
        let payload: RuntimeStatePayload = try await call(
            control: ["type": "runtime-state"],
            at: date
        )
        return AppRuntimeConfiguration(
            recordingEnabled: payload.recordingPreference,
            cadenceSeconds: payload.cadence.seconds,
            screenshotRetentionSeconds: payload.screenshotRetention.seconds
        )
    }

    func startupReconcile(sessionID: String, at date: Date) async throws {
        let _: StartupPayload = try await call(
            control: [
                "type": "startup-reconcile",
                "session_id": sessionID,
                "device_id": deviceID,
                "display_timezone": displayTimezone,
            ],
            at: date
        )
    }

    func prepareTermination(sessionID: String, at date: Date) async throws {
        let _: TerminationPayload = try await call(
            control: [
                "type": "prepare-termination",
                "session_id": sessionID,
            ],
            at: date
        )
    }

    private func call<Result: Codable & Sendable>(
        control: [String: Any],
        at date: Date
    ) async throws -> Result {
        let request = try JSONSerialization.data(withJSONObject: [
            "schema_version": "1.0",
            "now": Self.timestamp(date),
            "control": control,
        ])
        let response = try await core.call(request)
        let envelope = try JSONDecoder().decode(
            ChronicleEnvelope<Result>.self,
            from: response
        )
        guard envelope.ok else {
            throw ChronicleBridgeError.bridgeStatus(1, envelope.error)
        }
        return try envelope.requireCompatibleMajor()
    }

    private static func timestamp(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.string(from: date)
    }
}

actor AppCaptureRuntime {
    typealias StatusSink = @Sendable (CapturePresentationState) async -> Void

    private let sessionID: String
    private let control: any AppRuntimeControlling
    private let coordinator: any CaptureCoordinating
    private let environment: SystemCaptureEnvironmentSource
    private let statusSink: StatusSink
    private var recordingEnabled: Bool
    private var started = false
    private var sessionLocked = false

    init(
        sessionID: String,
        recordingEnabled: Bool,
        control: any AppRuntimeControlling,
        coordinator: any CaptureCoordinating,
        environment: SystemCaptureEnvironmentSource,
        statusSink: @escaping StatusSink
    ) {
        self.sessionID = sessionID
        self.recordingEnabled = recordingEnabled
        self.control = control
        self.coordinator = coordinator
        self.environment = environment
        self.statusSink = statusSink
    }

    func start(at date: Date = Date()) async throws {
        guard !started else { return }
        await statusSink(.starting)
        try await control.startupReconcile(sessionID: sessionID, at: date)
        started = true
        await environment.update(paused: !recordingEnabled)
        await coordinator.setUpdateSink { [weak self] snapshot in
            await self?.coordinatorDidUpdate(snapshot)
        }
        await coordinator.start()
    }

    func handle(_ event: AppLifecycleEvent) async {
        guard started else { return }
        switch event {
        case .willSleep:
            await environment.update(asleep: true)
            await coordinator.suspend()
        case .didWake:
            await environment.update(asleep: false)
            await coordinator.resume()
        case .sessionResigned:
            sessionLocked = true
            await environment.update(locked: true)
            await coordinator.privacyBoundaryChanged()
        case .sessionBecameActive:
            sessionLocked = false
            await environment.update(locked: false)
            await coordinator.privacyBoundaryChanged()
        case .wallClockChanged:
            await coordinator.wallClockChanged()
        }
    }

    func setRecordingEnabled(_ enabled: Bool) async {
        guard started else { return }
        await environment.update(paused: !enabled)
        await coordinator.recordingPreferenceChanged(enabled: enabled)
        recordingEnabled = enabled
        await publishStatus()
    }

    func retryStorageRecovery() async {
        guard started else { return }
        await coordinator.resumeAfterStorageRecovery()
    }

    func shutdown(at date: Date = Date()) async throws {
        guard started else { return }
        await coordinator.stop()
        do {
            try await control.prepareTermination(sessionID: sessionID, at: date)
            started = false
            await statusSink(.stopped)
        } catch {
            started = false
            await statusSink(.repairRequired(error.localizedDescription))
            throw error
        }
    }

    private func publishStatus() async {
        let snapshot = await coordinator.snapshot()
        await publishStatus(snapshot)
    }

    private func coordinatorDidUpdate(_ snapshot: CaptureCoordinatorSnapshot) async {
        guard started else { return }
        await publishStatus(snapshot)
    }

    private func publishStatus(_ snapshot: CaptureCoordinatorSnapshot) async {
        let status: CapturePresentationState
        switch snapshot.state {
        case .stopped:
            status = .stopped
        case .running:
            if !recordingEnabled {
                status = .paused
            } else if sessionLocked {
                status = .protected
            } else if let denial = snapshot.lastDenial {
                status = Self.presentation(for: denial)
            } else {
                status = .recording
            }
        case .suspended:
            status = .sleeping
        case .studyNotStarted:
            status = .studyNotStarted
        case .studyExpired:
            status = .studyExpired
        case .storageFailure:
            status = .storageBlocked
        case let .repairRequired(failure):
            status = .repairRequired(failure.code ?? "capture-runtime-error")
        }
        await statusSink(status)
    }

    private static func presentation(for denial: CaptureDenial) -> CapturePresentationState {
        switch denial {
        case .userPaused:
            .paused
        case .studyExpired:
            .studyExpired
        case .locked, .secureInput, .applicationExcluded, .titleExcluded, .chronicleSelf:
            .protected
        case .permissionDenied, .asleep, .noExactWindow, .ambiguousWindow, .foregroundChanged:
            .unavailable(denial)
        }
    }
}

private struct RuntimeStatePayload: Codable, Sendable {
    let recordingPreference: Bool
    let cadence: RuntimeCadence
    let screenshotRetention: RuntimeScreenshotRetention

    enum CodingKeys: String, CodingKey {
        case recordingPreference = "recording_preference"
        case cadence
        case screenshotRetention = "screenshot_retention"
    }
}

private enum RuntimeScreenshotRetention: String, Codable, Sendable {
    case oneHour = "one-hour"
    case twentyFourHours = "twenty-four-hours"
    case sevenDays = "seven-days"
    case thirtyDays = "thirty-days"

    var seconds: UInt32 {
        switch self {
        case .oneHour: 60 * 60
        case .twentyFourHours: 24 * 60 * 60
        case .sevenDays: 7 * 24 * 60 * 60
        case .thirtyDays: 30 * 24 * 60 * 60
        }
    }
}

private enum RuntimeCadence: String, Codable, Sendable {
    case thirtySeconds = "thirty-seconds"
    case sixtySeconds = "sixty-seconds"

    var seconds: UInt32 {
        switch self {
        case .thirtySeconds: 30
        case .sixtySeconds: 60
        }
    }
}

private struct StartupPayload: Codable, Sendable {
    let gapEventIDs: [String]

    enum CodingKeys: String, CodingKey {
        case gapEventIDs = "gap_event_ids"
    }
}

private struct TerminationPayload: Codable, Sendable {
    let prepared: Bool
}
