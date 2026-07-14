import Foundation

struct CaptureClockSample: Equatable, Sendable {
    let wallTime: Date
    let monotonicNanoseconds: UInt64
}

protocol CaptureSchedulerClock: Sendable {
    func sample() async -> CaptureClockSample
    func sleep(untilMonotonicNanoseconds deadline: UInt64) async throws
}

struct SystemCaptureSchedulerClock: CaptureSchedulerClock {
    func sample() -> CaptureClockSample {
        CaptureClockSample(
            wallTime: Date(),
            monotonicNanoseconds: DispatchTime.now().uptimeNanoseconds
        )
    }

    func sleep(untilMonotonicNanoseconds deadline: UInt64) async throws {
        let now = DispatchTime.now().uptimeNanoseconds
        guard deadline > now else { return }
        try await Task.sleep(nanoseconds: deadline - now)
    }
}

struct PlannedCaptureTick: Equatable, Sendable {
    let deadlineNanoseconds: UInt64
    let scheduledAt: Date
    let ordinal: UInt64
}

enum CapturePlannerDecision: Equatable, Sendable {
    case wait(PlannedCaptureTick)
    case execute(PlannedCaptureTick)
    case missed(PlannedCaptureTick)
}

struct CaptureCadencePlanner: Equatable, Sendable {
    let cadenceSeconds: UInt32
    private(set) var nextTick: PlannedCaptureTick?
    private var nextOrdinal: UInt64 = 1

    private var cadenceNanoseconds: UInt64 {
        UInt64(cadenceSeconds) * 1_000_000_000
    }

    init(cadenceSeconds: UInt32) {
        precondition(cadenceSeconds == 30 || cadenceSeconds == 60)
        self.cadenceSeconds = cadenceSeconds
    }

    mutating func rebase(at sample: CaptureClockSample) {
        let cadence = TimeInterval(cadenceSeconds)
        let phase: TimeInterval = cadenceSeconds == 30 ? 15 : 30
        let wall = sample.wallTime.timeIntervalSince1970
        let interval = floor((wall - phase) / cadence) + 1
        let scheduled = phase + interval * cadence
        let delta = max(0, scheduled - wall)
        let deltaNanoseconds = UInt64((delta * 1_000_000_000).rounded())
        nextTick = PlannedCaptureTick(
            deadlineNanoseconds: adding(deltaNanoseconds, to: sample.monotonicNanoseconds),
            scheduledAt: Date(timeIntervalSince1970: scheduled),
            ordinal: takeOrdinal()
        )
    }

    mutating func disarm() {
        nextTick = nil
    }

    mutating func due(at sample: CaptureClockSample) -> CapturePlannerDecision? {
        guard let tick = nextTick else { return nil }
        guard sample.monotonicNanoseconds >= tick.deadlineNanoseconds else {
            return .wait(tick)
        }
        let lateness = sample.monotonicNanoseconds - tick.deadlineNanoseconds
        if lateness >= cadenceNanoseconds {
            rebase(at: sample)
            return .missed(tick)
        }
        nextTick = PlannedCaptureTick(
            deadlineNanoseconds: adding(cadenceNanoseconds, to: tick.deadlineNanoseconds),
            scheduledAt: tick.scheduledAt.addingTimeInterval(TimeInterval(cadenceSeconds)),
            ordinal: takeOrdinal()
        )
        return .execute(tick)
    }

    private mutating func takeOrdinal() -> UInt64 {
        let value = nextOrdinal
        nextOrdinal &+= 1
        return value
    }

    private func adding(_ interval: UInt64, to value: UInt64) -> UInt64 {
        let (sum, overflow) = value.addingReportingOverflow(interval)
        return overflow ? UInt64.max : sum
    }
}

struct CaptureAttemptContext: Equatable, Sendable {
    let eventID: String
    let lifecycleEventID: String
    let imageArtifactID: String
    let deviceID: String
    let scheduledAt: Date
    let displayTimezone: String
    let sourceVersion: String
    let cadenceSeconds: UInt32
    let bootSequence: String
    let monotonicTick: UInt64
    let retentionSeconds: TimeInterval
    let executionGeneration: UInt64
}

protocol CaptureAttemptContextCreating: Sendable {
    func makeContext(
        for tick: PlannedCaptureTick,
        executionGeneration: UInt64
    ) async -> CaptureAttemptContext
}

actor SystemCaptureAttemptContextSource: CaptureAttemptContextCreating {
    private let deviceID: String
    private let cadenceSeconds: UInt32
    private let retentionSeconds: TimeInterval
    private let sourceVersion: String
    private let bootSequence: String

    init(
        deviceID: String,
        cadenceSeconds: UInt32,
        retentionSeconds: TimeInterval,
        sourceVersion: String = "macos-capture-1",
        bootSequence: String = "boot-\(UUID().uuidString.lowercased())"
    ) {
        precondition(cadenceSeconds == 30 || cadenceSeconds == 60)
        precondition(retentionSeconds >= 0)
        self.deviceID = deviceID
        self.cadenceSeconds = cadenceSeconds
        self.retentionSeconds = retentionSeconds
        self.sourceVersion = sourceVersion
        self.bootSequence = bootSequence
    }

    func makeContext(
        for tick: PlannedCaptureTick,
        executionGeneration: UInt64
    ) -> CaptureAttemptContext {
        let token = UUID().uuidString.lowercased()
        return CaptureAttemptContext(
            eventID: "event-\(token)",
            lifecycleEventID: "lifecycle-\(UUID().uuidString.lowercased())",
            imageArtifactID: "image-\(UUID().uuidString.lowercased())",
            deviceID: deviceID,
            scheduledAt: tick.scheduledAt,
            displayTimezone: TimeZone.current.identifier,
            sourceVersion: sourceVersion,
            cadenceSeconds: cadenceSeconds,
            bootSequence: bootSequence,
            monotonicTick: tick.ordinal,
            retentionSeconds: retentionSeconds,
            executionGeneration: executionGeneration
        )
    }
}

actor CaptureExecutionEpoch: CaptureAttemptValidityChecking {
    private var currentGeneration: UInt64 = 0
    private var invalidations: [UInt64: CaptureInvalidation] = [:]
    private var activePermits: [UUID: UInt64] = [:]
    private var permitWaiters: [UInt64: [CheckedContinuation<Void, Never>]] = [:]

    func beginGeneration() -> UInt64 {
        currentGeneration &+= 1
        return currentGeneration
    }

    func invalidate(
        generation: UInt64?,
        reason: CaptureInvalidation
    ) async {
        guard let generation else { return }
        invalidations[generation] = reason
        if generation == currentGeneration {
            currentGeneration &+= 1
        }
        if invalidations.count > 16,
           let oldest = invalidations.keys.sorted().first {
            invalidations.removeValue(forKey: oldest)
        }
        guard activePermits.values.contains(generation) else { return }
        await withCheckedContinuation { continuation in
            permitWaiters[generation, default: []].append(continuation)
        }
    }

    func invalidation(for executionGeneration: UInt64) -> CaptureInvalidation? {
        if let reason = invalidations[executionGeneration] { return reason }
        return executionGeneration == currentGeneration ? nil : .superseded
    }

    func claimPersistence(
        for executionGeneration: UInt64
    ) -> CapturePersistencePermit? {
        guard executionGeneration == currentGeneration,
              invalidations[executionGeneration] == nil
        else {
            return nil
        }
        let permit = CapturePersistencePermit(
            id: UUID(),
            executionGeneration: executionGeneration
        )
        activePermits[permit.id] = executionGeneration
        return permit
    }

    func releasePersistence(_ permit: CapturePersistencePermit) {
        guard activePermits.removeValue(forKey: permit.id) == permit.executionGeneration else {
            return
        }
        guard !activePermits.values.contains(permit.executionGeneration) else { return }
        let waiters = permitWaiters.removeValue(forKey: permit.executionGeneration) ?? []
        for waiter in waiters { waiter.resume() }
    }
}

protocol CaptureAttemptExecuting: Sendable {
    func attempt(
        context: CaptureAttemptContext,
        directive: CaptureAttemptDirective,
        proofToken: CaptureProofToken?
    ) async -> CaptureAttemptResult
}

enum CaptureAdmissionDecision: Equatable, Sendable {
    case allowed
    case userPaused
    case runtimeInactive
    case studyNotStarted
    case studyExpired
    case storageFreeSpace
    case storageImageQuota
    case failed(CapturePersistenceFailure)
}

protocol CaptureAdmissionChecking: Sendable {
    func admission(at: Date) async -> CaptureAdmissionDecision
}

protocol CaptureRecordingPreferenceSetting: Sendable {
    func setRecordingEnabled(
        _ enabled: Bool,
        at: Date
    ) async -> CapturePersistenceFailure?
}

enum CaptureStorageHealthDecision: Equatable, Sendable {
    case writable
    case blockedFreeSpace
    case blockedImageQuota
    case failed(CapturePersistenceFailure)
}

protocol CaptureStorageHealthChecking: Sendable {
    func storageHealth(at: Date) async -> CaptureStorageHealthDecision
}

enum CaptureRuntimeGapReason: String, Equatable, Sendable {
    case sleep
    case storageOutage = "storage-outage"
    case clockCorrection = "clock-correction"
}

protocol CaptureRuntimeGapReconciling: Sendable {
    func reconcile(reason: CaptureRuntimeGapReason, at: Date) async -> CapturePersistenceFailure?
}

actor CoreCaptureControlClient:
    CaptureAdmissionChecking,
    CaptureRecordingPreferenceSetting,
    CaptureStorageHealthChecking,
    CaptureRuntimeGapReconciling
{
    private let core: any CoreService
    private let deviceID: String
    private let displayTimezone: String

    init(core: any CoreService, deviceID: String, displayTimezone: String) {
        self.core = core
        self.deviceID = deviceID
        self.displayTimezone = displayTimezone
    }

    func admission(at date: Date) async -> CaptureAdmissionDecision {
        do {
            let response = try await call(
                control: ["type": "capture-admission"],
                at: date
            )
            let envelope = try JSONDecoder().decode(
                ControlEnvelope<CaptureAdmissionPayload>.self,
                from: response
            )
            guard envelope.ok, let result = envelope.result else {
                return .failed(Self.failure(from: envelope.error))
            }
            switch result.reason {
            case "allowed": return .allowed
            case "user-paused": return .userPaused
            case "runtime-inactive": return .runtimeInactive
            case "study-not-started": return .studyNotStarted
            case "study-expired": return .studyExpired
            case "storage-free-space": return .storageFreeSpace
            case "storage-image-quota": return .storageImageQuota
            default: return .failed(.unknown)
            }
        } catch {
            return .failed(Self.failure(from: error))
        }
    }

    func reconcile(
        reason: CaptureRuntimeGapReason,
        at date: Date
    ) async -> CapturePersistenceFailure? {
        do {
            let response = try await call(
                control: [
                    "type": "reconcile-runtime-gap",
                    "reason": reason.rawValue,
                    "device_id": deviceID,
                    "display_timezone": displayTimezone,
                ],
                at: date
            )
            let envelope = try JSONDecoder().decode(
                ControlEnvelope<RuntimeGapPayload>.self,
                from: response
            )
            return envelope.ok ? nil : Self.failure(from: envelope.error)
        } catch {
            return Self.failure(from: error)
        }
    }

    func setRecordingEnabled(
        _ enabled: Bool,
        at date: Date
    ) async -> CapturePersistenceFailure? {
        do {
            let response = try await call(
                control: [
                    "type": "set-recording-preference",
                    "enabled": enabled,
                ],
                at: date
            )
            let envelope = try JSONDecoder().decode(
                ControlStatusEnvelope.self,
                from: response
            )
            return envelope.ok ? nil : Self.failure(from: envelope.error)
        } catch {
            return Self.failure(from: error)
        }
    }

    func storageHealth(at date: Date) async -> CaptureStorageHealthDecision {
        do {
            let response = try await call(
                control: ["type": "storage-health"],
                at: date
            )
            let envelope = try JSONDecoder().decode(
                ControlEnvelope<StorageHealthPayload>.self,
                from: response
            )
            guard envelope.ok, let result = envelope.result else {
                return .failed(Self.failure(from: envelope.error))
            }
            switch result.state {
            case "healthy", "warning": return .writable
            case "blocked-free-space": return .blockedFreeSpace
            case "blocked-image-quota": return .blockedImageQuota
            default: return .failed(.unknown)
            }
        } catch {
            return .failed(Self.failure(from: error))
        }
    }

    private func call(control: [String: Any], at date: Date) async throws -> Data {
        let request = try JSONSerialization.data(withJSONObject: [
            "schema_version": "1.0",
            "now": Self.timestamp(date),
            "control": control,
        ])
        return try await core.call(request)
    }

    private static func timestamp(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.string(from: date)
    }

    private static func failure(from error: Error) -> CapturePersistenceFailure {
        guard case let ChronicleBridgeError.bridgeStatus(_, payload) = error else {
            return .unknown
        }
        return failure(from: payload)
    }

    private static func failure(
        from payload: ChronicleErrorPayload?
    ) -> CapturePersistenceFailure {
        guard let payload else { return .unknown }
        let category: CapturePersistenceFailureCategory
        switch payload.code {
        case "screenshot-free-space", "screenshot-image-quota", "io-error":
            category = .retryableStorage
        case "study-expired", "study-not-started":
            category = .studyBoundary
        case "stale-generation", "invalid-handle", "closed":
            category = .staleGeneration
        case "contract-error", "ingest-contract-error", "schema-mismatch",
             "invalid-call-envelope":
            category = .contractRepair
        default:
            category = payload.retryable ? .retryableStorage : .unknownFatal
        }
        return CapturePersistenceFailure(
            category: category,
            code: payload.code,
            retryable: category == .retryableStorage
        )
    }
}

private struct ControlEnvelope<Result: Decodable & Sendable>: Decodable, Sendable {
    let ok: Bool
    let result: Result?
    let error: ChronicleErrorPayload?
}

private struct ControlStatusEnvelope: Decodable, Sendable {
    let ok: Bool
    let error: ChronicleErrorPayload?
}

private struct CaptureAdmissionPayload: Decodable, Sendable {
    let allowed: Bool
    let reason: String
}

private struct RuntimeGapPayload: Decodable, Sendable {
    let gapEventIDs: [String]

    enum CodingKeys: String, CodingKey {
        case gapEventIDs = "gap_event_ids"
    }
}

private struct StorageHealthPayload: Decodable, Sendable {
    let state: String
}

enum CaptureCoordinatorState: Equatable, Sendable {
    case stopped
    case running
    case suspended
    case studyNotStarted
    case studyExpired
    case storageFailure
    case repairRequired(CapturePersistenceFailure)
}

struct CaptureCoordinatorSnapshot: Equatable, Sendable {
    let state: CaptureCoordinatorState
    let nextTick: PlannedCaptureTick?
    let attemptInFlight: Bool
    let executionGeneration: UInt64?
    let lastDenial: CaptureDenial?
}

typealias CaptureCoordinatorUpdateSink = @Sendable (CaptureCoordinatorSnapshot) async -> Void

actor CaptureCoordinator {
    private let clock: any CaptureSchedulerClock
    private let contextSource: any CaptureAttemptContextCreating
    private let executor: any CaptureAttemptExecuting
    private let admission: any CaptureAdmissionChecking
    private let preferences: any CaptureRecordingPreferenceSetting
    private let storage: any CaptureStorageHealthChecking
    private let gaps: any CaptureRuntimeGapReconciling
    private let epoch: CaptureExecutionEpoch
    private var planner: CaptureCadencePlanner
    private var state: CaptureCoordinatorState = .stopped
    private var attemptInFlight = false
    private var executionGeneration: UInt64?
    private var loopTask: Task<Void, Never>?
    private var loopGeneration: UInt64 = 0
    private var lifecycleOperationInFlight = false
    private var lifecycleOperationGeneration: UInt64 = 0
    private var pendingRecordingPreference: Bool?
    private var pendingClockChange: CaptureClockSample?
    private var pendingWake = false
    private var suspendTransitionInFlight = false
    private var lastDenial: CaptureDenial?
    private var updateSink: CaptureCoordinatorUpdateSink?
    private var updateDeliveryTask: Task<Void, Never>?

    init(
        cadenceSeconds: UInt32,
        clock: any CaptureSchedulerClock = SystemCaptureSchedulerClock(),
        contextSource: any CaptureAttemptContextCreating,
        executor: any CaptureAttemptExecuting,
        admission: any CaptureAdmissionChecking,
        preferences: any CaptureRecordingPreferenceSetting,
        storage: any CaptureStorageHealthChecking,
        gaps: any CaptureRuntimeGapReconciling,
        epoch: CaptureExecutionEpoch
    ) {
        planner = CaptureCadencePlanner(cadenceSeconds: cadenceSeconds)
        self.clock = clock
        self.contextSource = contextSource
        self.executor = executor
        self.admission = admission
        self.preferences = preferences
        self.storage = storage
        self.gaps = gaps
        self.epoch = epoch
    }

    func start() async {
        guard state == .stopped,
              let operation = beginLifecycleOperation()
        else { return }
        defer { finishLifecycleOperation(operation) }
        _ = await activateAfterAdmission(
            at: await clock.sample(),
            startLoop: true,
            operation: operation
        )
    }

    func setUpdateSink(_ sink: @escaping CaptureCoordinatorUpdateSink) {
        updateSink = sink
    }

    func stop() async {
        let operation = beginPreemptingLifecycleOperation()
        defer { finishLifecycleOperation(operation) }
        await transition(to: .stopped, invalidation: .stopping)
        pendingClockChange = nil
        pendingWake = false
    }

    func suspend() async {
        guard state == .running else { return }
        suspendTransitionInFlight = true
        let operation = beginPreemptingLifecycleOperation()
        defer {
            suspendTransitionInFlight = false
            finishLifecycleOperation(operation)
        }
        await transition(to: .suspended, invalidation: .sleep)
        guard isCurrentLifecycleOperation(operation), pendingWake else { return }
        pendingWake = false
        suspendTransitionInFlight = false
        await recover(reason: .sleep, operation: operation)
    }

    func resume() async {
        guard state == .suspended else {
            if suspendTransitionInFlight { pendingWake = true }
            return
        }
        guard let operation = beginLifecycleOperation() else {
            if suspendTransitionInFlight { pendingWake = true }
            return
        }
        pendingWake = false
        defer { finishLifecycleOperation(operation) }
        await recover(reason: .sleep, operation: operation)
    }

    func resumeAfterStudyExtension() async {
        guard state == .studyExpired || state == .studyNotStarted,
              let operation = beginLifecycleOperation()
        else { return }
        defer { finishLifecycleOperation(operation) }
        _ = await activateAfterAdmission(
            at: await clock.sample(),
            startLoop: true,
            operation: operation
        )
    }

    func resumeAfterStorageRecovery() async {
        guard state == .storageFailure,
              let operation = beginLifecycleOperation()
        else { return }
        defer { finishLifecycleOperation(operation) }
        await recover(reason: .storageOutage, operation: operation)
    }

    func wallClockChanged() async {
        let clockChange = await clock.sample()
        guard state == .running || state == .suspended || state == .storageFailure else {
            return
        }
        pendingClockChange = clockChange
        guard state == .running,
              let operation = beginLifecycleOperation()
        else { return }
        defer { finishLifecycleOperation(operation) }
        await transition(to: .suspended, invalidation: .clockChanged)
        guard isCurrentLifecycleOperation(operation) else { return }
        await recover(reason: nil, operation: operation)
    }

    func recordingPreferenceChanged(enabled: Bool) async {
        pendingRecordingPreference = enabled
        guard state == .running,
              let operation = beginLifecycleOperation()
        else { return }
        defer { finishLifecycleOperation(operation) }
        guard await applyPendingRecordingPreference(operation: operation),
              isCurrentLifecycleOperation(operation),
              state == .running
        else { return }
        if pendingClockChange != nil {
            await transition(to: .suspended, invalidation: .clockChanged)
            guard isCurrentLifecycleOperation(operation) else { return }
            await recover(reason: nil, operation: operation)
        }
    }

    func privacyBoundaryChanged() async {
        guard state == .running,
              let operation = beginLifecycleOperation()
        else { return }
        defer { finishLifecycleOperation(operation) }
        let previousGeneration = executionGeneration
        await epoch.invalidate(
            generation: previousGeneration,
            reason: .superseded
        )
        guard isCurrentLifecycleOperation(operation), state == .running else { return }
        if executionGeneration == previousGeneration {
            executionGeneration = await epoch.beginGeneration()
        }
        lastDenial = nil
        publishSnapshot()
    }

    func snapshot() -> CaptureCoordinatorSnapshot {
        CaptureCoordinatorSnapshot(
            state: state,
            nextTick: planner.nextTick,
            attemptInFlight: attemptInFlight,
            executionGeneration: executionGeneration,
            lastDenial: lastDenial
        )
    }

    // Deterministic seam shared by the production loop and unit tests.
    func processDue(at sample: CaptureClockSample) async {
        guard state == .running,
              !attemptInFlight,
              !lifecycleOperationInFlight,
              pendingRecordingPreference == nil
        else { return }
        guard let decision = planner.due(at: sample) else { return }
        switch decision {
        case .wait, .missed:
            return
        case let .execute(tick):
            guard let generation = executionGeneration else { return }
            attemptInFlight = true
            await execute(
                tick: tick,
                observedSample: sample,
                generation: generation
            )
            attemptInFlight = false
        }
    }

    // Internal deterministic seam: start() is the production entry point.
    func activate(at sample: CaptureClockSample, startLoop: Bool = true) async {
        planner.rebase(at: sample)
        executionGeneration = await epoch.beginGeneration()
        state = .running
        lastDenial = nil
        if startLoop { restartLoop() }
        publishSnapshot()
    }

    private func execute(
        tick: PlannedCaptureTick,
        observedSample: CaptureClockSample,
        generation: UInt64
    ) async {
        let directive: CaptureAttemptDirective
        let admissionDecision = await admission.admission(at: observedSample.wallTime)
        if await epoch.invalidation(for: generation) != nil { return }
        switch admissionDecision {
        case .allowed:
            directive = .normal
        case .userPaused:
            directive = .forceDenial(.userPaused)
        case .runtimeInactive:
            await transition(to: .stopped, invalidation: .stopping)
            return
        case .studyNotStarted:
            await transition(to: .studyNotStarted, invalidation: .studyExpired)
            return
        case .studyExpired:
            await transition(to: .studyExpired, invalidation: .studyExpired)
            return
        case .storageFreeSpace, .storageImageQuota:
            await transition(to: .storageFailure, invalidation: .superseded)
            return
        case let .failed(failure):
            await handle(failure: failure)
            return
        }

        let context = await contextSource.makeContext(
            for: tick,
            executionGeneration: generation
        )
        let result = await executor.attempt(
            context: context,
            directive: directive,
            proofToken: nil
        )

        if await epoch.invalidation(for: generation) != nil,
           !isPersistenceLinearized(result) {
            return
        }

        switch result {
        case .denied(.studyExpired):
            await transition(to: .studyExpired, invalidation: .studyExpired)
        case let .persistenceFailed(failure):
            await handle(failure: failure)
        case .invalidated(.clockChanged):
            await captureClockDiscontinuityDetected()
        case .stored, .proofSucceeded:
            lastDenial = nil
            publishSnapshot()
        case let .denied(reason):
            lastDenial = reason
            publishSnapshot()
        case .invalidated:
            break
        }
    }

    private func isPersistenceLinearized(_ result: CaptureAttemptResult) -> Bool {
        switch result {
        case .stored, .denied, .persistenceFailed:
            true
        case .proofSucceeded, .invalidated:
            false
        }
    }

    private func handle(failure: CapturePersistenceFailure) async {
        switch failure.category {
        case .retryableStorage:
            await transition(to: .storageFailure, invalidation: .superseded)
        case .studyBoundary:
            await transition(to: .studyExpired, invalidation: .studyExpired)
        case .staleGeneration, .contractRepair, .unknownFatal:
            await transition(to: .repairRequired(failure), invalidation: .stopping)
        }
    }

    private func recover(
        reason: CaptureRuntimeGapReason?,
        operation: UInt64
    ) async {
        var sample = await clock.sample()
        guard isCurrentLifecycleOperation(operation) else { return }
        if reason == .storageOutage {
            switch await storage.storageHealth(at: sample.wallTime) {
            case .writable:
                break
            case .blockedFreeSpace, .blockedImageQuota:
                state = .storageFailure
                lastDenial = nil
                publishSnapshot()
                return
            case let .failed(failure):
                await handle(failure: failure)
                return
            }
            guard isCurrentLifecycleOperation(operation) else { return }
        }
        if reason != nil {
            while let correction = pendingClockChange {
                pendingClockChange = nil
                if let failure = await gaps.reconcile(
                    reason: .clockCorrection,
                    at: correction.wallTime
                ) {
                    await handle(failure: failure)
                    return
                }
                guard isCurrentLifecycleOperation(operation) else { return }
            }
            sample = await clock.sample()
            guard isCurrentLifecycleOperation(operation) else { return }
        }
        if let reason,
           let failure = await gaps.reconcile(reason: reason, at: sample.wallTime) {
            await handle(failure: failure)
            return
        }
        guard isCurrentLifecycleOperation(operation) else { return }
        while isCurrentLifecycleOperation(operation) {
            while let correction = pendingClockChange {
                pendingClockChange = nil
                if let failure = await gaps.reconcile(
                    reason: .clockCorrection,
                    at: correction.wallTime
                ) {
                    await handle(failure: failure)
                    return
                }
                guard isCurrentLifecycleOperation(operation) else { return }
            }
            guard await applyPendingRecordingPreference(operation: operation),
                  isCurrentLifecycleOperation(operation)
            else { return }
            if pendingClockChange != nil { continue }
            sample = await clock.sample()
            guard isCurrentLifecycleOperation(operation) else { return }
            let settled = await activateAfterAdmission(
                at: sample,
                startLoop: true,
                operation: operation
            )
            if !settled, pendingClockChange != nil { continue }
            return
        }
    }

    private func activateAfterAdmission(
        at sample: CaptureClockSample,
        startLoop: Bool,
        operation: UInt64
    ) async -> Bool {
        while isCurrentLifecycleOperation(operation) {
            guard await applyPendingRecordingPreference(operation: operation) else {
                return true
            }
            let decision = await admission.admission(at: sample.wallTime)
            guard isCurrentLifecycleOperation(operation) else { return true }
            if pendingRecordingPreference != nil { continue }
            if pendingClockChange != nil { return false }
            switch decision {
            case .allowed, .userPaused:
                planner.rebase(at: sample)
                let generation = await epoch.beginGeneration()
                guard isCurrentLifecycleOperation(operation) else {
                    await epoch.invalidate(generation: generation, reason: .superseded)
                    return true
                }
                if pendingRecordingPreference != nil {
                    executionGeneration = generation
                    continue
                }
                if pendingClockChange != nil {
                    await epoch.invalidate(generation: generation, reason: .clockChanged)
                    planner.disarm()
                    return false
                }
                executionGeneration = generation
                state = .running
                lastDenial = nil
                if startLoop { restartLoop() }
                publishSnapshot()
                return true
            case .runtimeInactive:
                await transition(to: .stopped, invalidation: .stopping)
            case .studyNotStarted:
                await transition(to: .studyNotStarted, invalidation: .studyExpired)
            case .studyExpired:
                await transition(to: .studyExpired, invalidation: .studyExpired)
            case .storageFreeSpace, .storageImageQuota:
                await transition(to: .storageFailure, invalidation: .superseded)
            case let .failed(failure):
                await handle(failure: failure)
            }
            return true
        }
        return true
    }

    // Deterministic seam for the pipeline's fallback clock-discontinuity signal.
    // The signal is queued before taking the lifecycle gate so a concurrent
    // preference or wake operation cannot silently consume it.
    func captureClockDiscontinuityDetected() async {
        let correction = await clock.sample()
        guard state == .running || state == .suspended || state == .storageFailure else {
            return
        }
        pendingClockChange = correction
        guard state == .running,
              let operation = beginLifecycleOperation()
        else { return }
        defer { finishLifecycleOperation(operation) }
        await transition(to: .suspended, invalidation: .clockChanged)
        guard isCurrentLifecycleOperation(operation) else { return }
        await recover(reason: nil, operation: operation)
    }

    private func beginLifecycleOperation() -> UInt64? {
        guard !lifecycleOperationInFlight else { return nil }
        lifecycleOperationGeneration &+= 1
        lifecycleOperationInFlight = true
        return lifecycleOperationGeneration
    }

    private func finishLifecycleOperation(_ operation: UInt64) {
        guard operation == lifecycleOperationGeneration else { return }
        lifecycleOperationInFlight = false
    }

    private func beginPreemptingLifecycleOperation() -> UInt64 {
        lifecycleOperationGeneration &+= 1
        lifecycleOperationInFlight = true
        return lifecycleOperationGeneration
    }

    private func isCurrentLifecycleOperation(_ operation: UInt64) -> Bool {
        lifecycleOperationInFlight && operation == lifecycleOperationGeneration
    }

    private func applyPendingRecordingPreference(
        operation: UInt64
    ) async -> Bool {
        while let enabled = pendingRecordingPreference {
            pendingRecordingPreference = nil
            let previousGeneration = executionGeneration
            await epoch.invalidate(
                generation: previousGeneration,
                reason: .userPaused
            )
            guard isCurrentLifecycleOperation(operation) else {
                if pendingRecordingPreference == nil {
                    pendingRecordingPreference = enabled
                }
                return false
            }
            executionGeneration = nil
            let sample = await clock.sample()
            guard isCurrentLifecycleOperation(operation) else {
                if pendingRecordingPreference == nil {
                    pendingRecordingPreference = enabled
                }
                return false
            }
            if let failure = await preferences.setRecordingEnabled(
                enabled,
                at: sample.wallTime
            ) {
                await handle(failure: failure)
                return false
            }
            guard isCurrentLifecycleOperation(operation) else { return false }
            if state == .running {
                executionGeneration = await epoch.beginGeneration()
                guard isCurrentLifecycleOperation(operation) else { return false }
            }
        }
        return true
    }

    private func transition(
        to newState: CaptureCoordinatorState,
        invalidation: CaptureInvalidation
    ) async {
        let invalidatedGeneration = executionGeneration
        state = newState
        lastDenial = nil
        planner.disarm()
        loopGeneration &+= 1
        loopTask?.cancel()
        loopTask = nil
        await epoch.invalidate(
            generation: invalidatedGeneration,
            reason: invalidation
        )
        if executionGeneration == invalidatedGeneration {
            executionGeneration = nil
        }
        publishSnapshot()
    }

    func flushUpdates() async {
        await updateDeliveryTask?.value
    }

    private func publishSnapshot() {
        guard let updateSink else { return }
        let previous = updateDeliveryTask
        let update = snapshot()
        updateDeliveryTask = Task {
            await previous?.value
            guard !Task.isCancelled else { return }
            await updateSink(update)
        }
    }

    private func restartLoop() {
        loopGeneration &+= 1
        let generation = loopGeneration
        loopTask?.cancel()
        loopTask = Task { [weak self] in
            await self?.runLoop(generation: generation)
        }
    }

    private func runLoop(generation: UInt64) async {
        while !Task.isCancelled,
              generation == loopGeneration,
              state == .running,
              let tick = planner.nextTick {
            if attemptInFlight {
                try? await Task.sleep(nanoseconds: 100_000_000)
                continue
            }
            do {
                try await clock.sleep(
                    untilMonotonicNanoseconds: tick.deadlineNanoseconds
                )
            } catch {
                return
            }
            guard !Task.isCancelled, generation == loopGeneration else { return }
            await processDue(at: await clock.sample())
        }
    }
}

extension CaptureAttemptPipeline: CaptureAttemptExecuting {}
