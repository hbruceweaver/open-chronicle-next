import Foundation
import XCTest
@testable import OpenChronicle

private actor CoordinatorUpdateRecorder {
    private(set) var values: [CaptureCoordinatorSnapshot] = []

    func append(_ value: CaptureCoordinatorSnapshot) {
        values.append(value)
    }
}

private actor BlockingFirstCoordinatorUpdateSink {
    private var snapshots: [CaptureCoordinatorSnapshot] = []
    private var firstStarted = false
    private var firstStartedWaiters: [CheckedContinuation<Void, Never>] = []
    private var firstRelease: CheckedContinuation<Void, Never>?

    func receive(_ snapshot: CaptureCoordinatorSnapshot) async {
        snapshots.append(snapshot)
        guard snapshots.count == 1 else { return }

        firstStarted = true
        let waiters = firstStartedWaiters
        firstStartedWaiters.removeAll()
        for waiter in waiters {
            waiter.resume()
        }
        await withCheckedContinuation { continuation in
            firstRelease = continuation
        }
    }

    func waitUntilFirstStarted() async {
        guard !firstStarted else { return }
        await withCheckedContinuation { continuation in
            firstStartedWaiters.append(continuation)
        }
    }

    func releaseFirst() {
        firstRelease?.resume()
        firstRelease = nil
    }

    func values() -> [CaptureCoordinatorSnapshot] {
        snapshots
    }
}

final class CoordinatorTests: XCTestCase {
    func testUpdateSinkPublishesTickDenialAndStorageTransition() async {
        let failure = CapturePersistenceFailure(
            category: .retryableStorage,
            code: "fixture-storage",
            retryable: true
        )
        let executor = RecordingCaptureExecutor(results: [
            .denied(.permissionDenied),
            .persistenceFailed(failure),
        ])
        let updates = CoordinatorUpdateRecorder()
        let coordinator = makeCoordinator(executor: executor)
        await coordinator.setUpdateSink { snapshot in
            await updates.append(snapshot)
        }
        await coordinator.activate(at: sample(wall: 0, monotonic: 0), startLoop: false)

        await coordinator.processDue(at: sample(wall: 15, monotonic: 15_000_000_000))
        await coordinator.processDue(at: sample(wall: 45, monotonic: 45_000_000_000))
        await coordinator.flushUpdates()

        let snapshots = await updates.values
        XCTAssertTrue(snapshots.contains(where: { snapshot in
            snapshot.state == .running && snapshot.lastDenial == .permissionDenied
        }))
        XCTAssertEqual(snapshots.last?.state, .storageFailure)
        XCTAssertNil(snapshots.last?.lastDenial)
    }

    func testUpdateSinkDeliversTransitionsSeriallyInPublicationOrder() async {
        let updates = BlockingFirstCoordinatorUpdateSink()
        let coordinator = makeCoordinator(executor: RecordingCaptureExecutor())
        await coordinator.setUpdateSink { snapshot in
            await updates.receive(snapshot)
        }

        await coordinator.activate(at: sample(wall: 0, monotonic: 0), startLoop: false)
        await updates.waitUntilFirstStarted()
        await coordinator.stop()
        for _ in 0..<100 {
            await Task.yield()
        }

        let whileBlocked = await updates.values()
        XCTAssertEqual(whileBlocked.map(\.state), [.running])

        await updates.releaseFirst()
        await coordinator.flushUpdates()
        let delivered = await updates.values()
        XCTAssertEqual(delivered.map(\.state), [.running, .stopped])
    }

    func testCoreControlClientRoundTripsAdmissionAndRuntimeGap() async throws {
        let temporary = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: temporary, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: temporary) }
        let startedAt = Date(timeIntervalSince1970: 1_784_016_000)
        let core = try InProcessCore(applicationSupportURL: temporary, now: startedAt)
        try await callControl(
            core,
            at: startedAt,
            control: [
                "type": "startup-reconcile",
                "session_id": "swift-coordinator-control",
                "device_id": "device-control",
                "display_timezone": "Europe/Zurich",
            ]
        )
        try await callControl(
            core,
            at: startedAt,
            control: ["type": "set-recording-preference", "enabled": true]
        )
        let client = CoreCaptureControlClient(
            core: core,
            deviceID: "device-control",
            displayTimezone: "Europe/Zurich"
        )

        let admission = await client.admission(at: startedAt)
        XCTAssertEqual(admission, .allowed)
        let pauseFailure = await client.setRecordingEnabled(false, at: startedAt)
        let pausedAdmission = await client.admission(at: startedAt)
        let resumeFailure = await client.setRecordingEnabled(true, at: startedAt)
        let resumedAdmission = await client.admission(at: startedAt)
        XCTAssertNil(pauseFailure)
        XCTAssertEqual(pausedAdmission, .userPaused)
        XCTAssertNil(resumeFailure)
        XCTAssertEqual(resumedAdmission, .allowed)
        let storage = await client.storageHealth(at: startedAt)
        XCTAssertEqual(storage, .writable)
        let wake = startedAt.addingTimeInterval(60)
        let firstGap = await client.reconcile(reason: .sleep, at: wake)
        let repeatedGap = await client.reconcile(reason: .sleep, at: wake)
        XCTAssertNil(firstGap)
        XCTAssertNil(repeatedGap)
        try await core.close()
    }

    func testPlannerCentersThirtyAndSixtySecondCadence() {
        var thirty = CaptureCadencePlanner(cadenceSeconds: 30)
        thirty.rebase(at: sample(wall: 0, monotonic: 100))
        XCTAssertEqual(thirty.nextTick?.scheduledAt, Date(timeIntervalSince1970: 15))
        XCTAssertEqual(thirty.nextTick?.deadlineNanoseconds, 15_000_000_100)

        var sixty = CaptureCadencePlanner(cadenceSeconds: 60)
        sixty.rebase(at: sample(wall: 0, monotonic: 900))
        XCTAssertEqual(sixty.nextTick?.scheduledAt, Date(timeIntervalSince1970: 30))
        XCTAssertEqual(sixty.nextTick?.deadlineNanoseconds, 30_000_000_900)
    }

    func testSmallLatenessRetainsOriginalGridAndScheduledTime() {
        var planner = CaptureCadencePlanner(cadenceSeconds: 30)
        planner.rebase(at: sample(wall: 0, monotonic: 0))

        XCTAssertEqual(
            planner.due(at: sample(wall: 14, monotonic: 14_000_000_000)),
            .wait(PlannedCaptureTick(
                deadlineNanoseconds: 15_000_000_000,
                scheduledAt: Date(timeIntervalSince1970: 15),
                ordinal: 1
            ))
        )
        guard case let .execute(executed) = planner.due(
            at: sample(wall: 16, monotonic: 16_000_000_000)
        ) else {
            return XCTFail("expected one due tick")
        }
        XCTAssertEqual(executed.scheduledAt, Date(timeIntervalSince1970: 15))
        XCTAssertEqual(planner.nextTick?.scheduledAt, Date(timeIntervalSince1970: 45))
        XCTAssertEqual(planner.nextTick?.deadlineNanoseconds, 45_000_000_000)
    }

    func testOneCadenceLatenessSkipsAndRebasesWithoutImmediateCapture() {
        var planner = CaptureCadencePlanner(cadenceSeconds: 30)
        planner.rebase(at: sample(wall: 0, monotonic: 0))

        guard case let .missed(missed) = planner.due(
            at: sample(wall: 50, monotonic: 50_000_000_000)
        ) else {
            return XCTFail("expected missed tick")
        }
        XCTAssertEqual(missed.scheduledAt, Date(timeIntervalSince1970: 15))
        XCTAssertEqual(planner.nextTick?.scheduledAt, Date(timeIntervalSince1970: 75))
        XCTAssertEqual(planner.nextTick?.deadlineNanoseconds, 75_000_000_000)
    }

    func testContextSourceUsesPlannerOrdinalAndGeneration() async {
        let source = SystemCaptureAttemptContextSource(
            deviceID: "device-scheduler",
            cadenceSeconds: 30,
            retentionSeconds: 3_600,
            sourceVersion: "test-scheduler",
            bootSequence: "boot-fixed"
        )
        let first = await source.makeContext(
            for: PlannedCaptureTick(
                deadlineNanoseconds: 15,
                scheduledAt: Date(timeIntervalSince1970: 15),
                ordinal: 4
            ),
            executionGeneration: 7
        )
        let second = await source.makeContext(
            for: PlannedCaptureTick(
                deadlineNanoseconds: 45,
                scheduledAt: Date(timeIntervalSince1970: 45),
                ordinal: 5
            ),
            executionGeneration: 7
        )

        XCTAssertEqual(first.scheduledAt, Date(timeIntervalSince1970: 15))
        XCTAssertEqual(first.retentionSeconds, 3_600)
        XCTAssertEqual(first.bootSequence, "boot-fixed")
        XCTAssertEqual(first.monotonicTick, 4)
        XCTAssertEqual(second.monotonicTick, 5)
        XCTAssertEqual(first.executionGeneration, 7)
        XCTAssertNotEqual(first.eventID, second.eventID)
    }

    func testCoordinatorForwardsPlannedTickAndNeverOverlaps() async {
        let executor = RecordingCaptureExecutor(delayNanoseconds: 40_000_000)
        let coordinator = makeCoordinator(executor: executor)
        await coordinator.activate(at: sample(wall: 0, monotonic: 0), startLoop: false)

        async let first: Void = coordinator.processDue(
            at: sample(wall: 16, monotonic: 16_000_000_000)
        )
        try? await Task.sleep(nanoseconds: 5_000_000)
        async let overlapping: Void = coordinator.processDue(
            at: sample(wall: 46, monotonic: 46_000_000_000)
        )
        _ = await (first, overlapping)

        let execution = await executor.snapshot()
        XCTAssertEqual(execution.contexts.count, 1)
        XCTAssertEqual(execution.maximumConcurrent, 1)
        XCTAssertEqual(execution.contexts[0].scheduledAt, Date(timeIntervalSince1970: 15))
    }

    func testAdmissionResultCannotCrossARecordingPreferenceEpochChange() async {
        let executor = RecordingCaptureExecutor()
        let admission = BlockingAdmission(result: .allowed)
        let coordinator = makeCoordinator(executor: executor, admission: admission)
        await coordinator.activate(at: sample(wall: 0, monotonic: 0), startLoop: false)

        let due = Task {
            await coordinator.processDue(
                at: sample(wall: 15, monotonic: 15_000_000_000)
            )
        }
        await admission.waitUntilStarted()
        await coordinator.recordingPreferenceChanged(enabled: false)
        await admission.release()
        await due.value

        let execution = await executor.snapshot()
        let state = await coordinator.snapshot()
        XCTAssertTrue(execution.contexts.isEmpty)
        XCTAssertFalse(state.attemptInFlight)
        XCTAssertEqual(state.state, .running)
    }

    func testUserPauseForcesPixelFreeAttemptAndPermissionDenialKeepsCadence() async {
        let executor = RecordingCaptureExecutor(results: [
            .denied(.userPaused),
            .denied(.permissionDenied),
        ])
        let admission = SequenceAdmission([.userPaused, .allowed])
        let coordinator = makeCoordinator(executor: executor, admission: admission)
        await coordinator.activate(at: sample(wall: 0, monotonic: 0), startLoop: false)

        await coordinator.processDue(at: sample(wall: 15, monotonic: 15_000_000_000))
        await coordinator.processDue(at: sample(wall: 45, monotonic: 45_000_000_000))

        let state = await coordinator.snapshot()
        let execution = await executor.snapshot()
        XCTAssertEqual(state.state, .running)
        XCTAssertEqual(execution.directives, [.forceDenial(.userPaused), .normal])
        XCTAssertEqual(execution.contexts.count, 2)
    }

    func testStudyExpiryAdmissionStopsWithoutCallingPipeline() async {
        let executor = RecordingCaptureExecutor()
        let coordinator = makeCoordinator(
            executor: executor,
            admission: SequenceAdmission([.studyExpired])
        )
        await coordinator.activate(at: sample(wall: 0, monotonic: 0), startLoop: false)

        await coordinator.processDue(at: sample(wall: 15, monotonic: 15_000_000_000))

        let state = await coordinator.snapshot()
        let execution = await executor.snapshot()
        XCTAssertEqual(state.state, .studyExpired)
        XCTAssertNil(state.nextTick)
        XCTAssertTrue(execution.contexts.isEmpty)
    }

    func testOnlyRetryableStorageFailureStartsStorageOutage() async {
        let storage = CapturePersistenceFailure(
            category: .retryableStorage,
            code: "screenshot-free-space",
            retryable: true
        )
        let storageCoordinator = makeCoordinator(
            executor: RecordingCaptureExecutor(results: [.persistenceFailed(storage)])
        )
        await storageCoordinator.activate(
            at: sample(wall: 0, monotonic: 0),
            startLoop: false
        )
        await storageCoordinator.processDue(
            at: sample(wall: 15, monotonic: 15_000_000_000)
        )
        let storageState = await storageCoordinator.snapshot()
        XCTAssertEqual(storageState.state, .storageFailure)

        let contract = CapturePersistenceFailure(
            category: .contractRepair,
            code: "event-contract-error",
            retryable: false
        )
        let contractCoordinator = makeCoordinator(
            executor: RecordingCaptureExecutor(results: [.persistenceFailed(contract)])
        )
        await contractCoordinator.activate(
            at: sample(wall: 0, monotonic: 0),
            startLoop: false
        )
        await contractCoordinator.processDue(
            at: sample(wall: 15, monotonic: 15_000_000_000)
        )
        let contractState = await contractCoordinator.snapshot()
        XCTAssertEqual(contractState.state, .repairRequired(contract))
    }

    func testSleepRecoveryReconcilesGapBeforeFutureRearm() async {
        let executor = RecordingCaptureExecutor()
        let gap = RecordingGapReconciler()
        let clock = MutableCaptureClock(sample: sample(wall: 100, monotonic: 100_000_000_000))
        let coordinator = makeCoordinator(
            executor: executor,
            clock: clock,
            gaps: gap
        )
        await coordinator.activate(at: sample(wall: 0, monotonic: 0), startLoop: false)
        await coordinator.suspend()
        await coordinator.resume()

        let gapReasons = await gap.reasons
        let state = await coordinator.snapshot()
        XCTAssertEqual(gapReasons, [.sleep])
        XCTAssertEqual(state.state, .running)
        XCTAssertEqual(state.nextTick?.scheduledAt, Date(timeIntervalSince1970: 105))
        let execution = await executor.snapshot()
        XCTAssertTrue(execution.contexts.isEmpty)
    }

    func testDuplicateWakeCallbacksReconcileOneGap() async {
        let gap = RecordingGapReconciler(delayNanoseconds: 30_000_000)
        let clock = MutableCaptureClock(
            sample: sample(wall: 100, monotonic: 100_000_000_000)
        )
        let coordinator = makeCoordinator(
            executor: RecordingCaptureExecutor(),
            clock: clock,
            gaps: gap
        )
        await coordinator.activate(at: sample(wall: 0, monotonic: 0), startLoop: false)
        await coordinator.suspend()

        async let first: Void = coordinator.resume()
        async let duplicate: Void = coordinator.resume()
        _ = await (first, duplicate)

        let reasons = await gap.reasons
        XCTAssertEqual(reasons, [.sleep])
    }

    func testStorageOutageDoesNotCloseUntilStorageIsWritable() async {
        let failure = CapturePersistenceFailure(
            category: .retryableStorage,
            code: "io-error",
            retryable: true
        )
        let gap = RecordingGapReconciler()
        let coordinator = makeCoordinator(
            executor: RecordingCaptureExecutor(results: [.persistenceFailed(failure)]),
            storage: SequenceStorageHealth([.blockedFreeSpace]),
            gaps: gap
        )
        await coordinator.activate(at: sample(wall: 0, monotonic: 0), startLoop: false)
        await coordinator.processDue(
            at: sample(wall: 15, monotonic: 15_000_000_000)
        )
        await coordinator.resumeAfterStorageRecovery()

        let state = await coordinator.snapshot()
        let reasons = await gap.reasons
        XCTAssertEqual(state.state, .storageFailure)
        XCTAssertTrue(reasons.isEmpty)
    }

    func testDetectedClockRollbackReconcilesAndRebases() async {
        let gap = RecordingGapReconciler()
        let coordinator = makeCoordinator(
            executor: RecordingCaptureExecutor(results: [.invalidated(.clockChanged)]),
            gaps: gap
        )
        await coordinator.activate(at: sample(wall: 0, monotonic: 0), startLoop: false)
        await coordinator.processDue(
            at: sample(wall: 15, monotonic: 15_000_000_000)
        )

        let state = await coordinator.snapshot()
        let reasons = await gap.reasons
        XCTAssertEqual(reasons, [.clockCorrection])
        XCTAssertEqual(state.state, .running)
        XCTAssertNotNil(state.nextTick)
    }

    func testDetectedClockChangeQueuesBehindPreferenceTransition() async {
        let preferences = BlockingPreferenceSetter()
        let gaps = RecordingGapReconciler()
        let coordinator = makeCoordinator(
            executor: RecordingCaptureExecutor(),
            preferences: preferences,
            gaps: gaps
        )
        await coordinator.activate(at: sample(wall: 0, monotonic: 0), startLoop: false)

        let preference = Task {
            await coordinator.recordingPreferenceChanged(enabled: false)
        }
        await preferences.waitUntilStarted()
        await coordinator.captureClockDiscontinuityDetected()
        await preferences.release()
        await preference.value

        let reasons = await gaps.reasons
        let state = await coordinator.snapshot()
        XCTAssertEqual(reasons, [.clockCorrection])
        XCTAssertEqual(state.state, .running)
        XCTAssertNotNil(state.nextTick)
    }

    func testStopRetainsPriorityOverClockChangeWhilePersistenceFinishes() async {
        let epoch = CaptureExecutionEpoch()
        let gaps = RecordingGapReconciler()
        let coordinator = makeCoordinator(
            executor: RecordingCaptureExecutor(),
            gaps: gaps,
            epoch: epoch
        )
        await coordinator.activate(at: sample(wall: 0, monotonic: 0), startLoop: false)
        let active = await coordinator.snapshot()
        guard let generation = active.executionGeneration,
              let permit = await epoch.claimPersistence(for: generation)
        else { return XCTFail("expected active stop generation and permit") }

        let stop = Task { await coordinator.stop() }
        await waitForInvalidation(.stopping, generation: generation, epoch: epoch)
        let clockChange = Task { await coordinator.wallClockChanged() }
        await epoch.releasePersistence(permit)
        await stop.value
        await clockChange.value

        let final = await coordinator.snapshot()
        let reasons = await gaps.reasons
        XCTAssertEqual(final.state, .stopped)
        XCTAssertNil(final.nextTick)
        XCTAssertTrue(reasons.isEmpty)
    }

    func testStopPreemptsPermitBlockedClockTransitionWithoutStaleRearm() async {
        let epoch = CaptureExecutionEpoch()
        let gaps = RecordingGapReconciler()
        let coordinator = makeCoordinator(
            executor: RecordingCaptureExecutor(),
            gaps: gaps,
            epoch: epoch
        )
        await coordinator.activate(at: sample(wall: 0, monotonic: 0), startLoop: false)
        let active = await coordinator.snapshot()
        guard let generation = active.executionGeneration,
              let permit = await epoch.claimPersistence(for: generation)
        else { return XCTFail("expected active clock generation and permit") }

        let clockChange = Task { await coordinator.wallClockChanged() }
        await waitForInvalidation(.clockChanged, generation: generation, epoch: epoch)
        let stop = Task { await coordinator.stop() }
        await waitForInvalidation(.stopping, generation: generation, epoch: epoch)
        await epoch.releasePersistence(permit)
        await clockChange.value
        await stop.value

        let final = await coordinator.snapshot()
        let reasons = await gaps.reasons
        XCTAssertEqual(final.state, .stopped)
        XCTAssertNil(final.nextTick)
        XCTAssertTrue(reasons.isEmpty)
    }

    func testSuspendRetainsPriorityOverClockChangeWhilePersistenceFinishes() async {
        let epoch = CaptureExecutionEpoch()
        let gaps = RecordingGapReconciler()
        let coordinator = makeCoordinator(
            executor: RecordingCaptureExecutor(),
            gaps: gaps,
            epoch: epoch
        )
        await coordinator.activate(at: sample(wall: 0, monotonic: 0), startLoop: false)
        let active = await coordinator.snapshot()
        guard let generation = active.executionGeneration,
              let permit = await epoch.claimPersistence(for: generation)
        else { return XCTFail("expected active suspend generation and permit") }

        let suspend = Task { await coordinator.suspend() }
        await waitForInvalidation(.sleep, generation: generation, epoch: epoch)
        let clockChange = Task { await coordinator.wallClockChanged() }
        await epoch.releasePersistence(permit)
        await suspend.value
        await clockChange.value

        let final = await coordinator.snapshot()
        let reasons = await gaps.reasons
        XCTAssertEqual(final.state, .suspended)
        XCTAssertNil(final.nextTick)
        XCTAssertTrue(reasons.isEmpty)
    }

    func testWakeDuringPermitBlockedSuspendIsNotLost() async {
        let epoch = CaptureExecutionEpoch()
        let gaps = RecordingGapReconciler()
        let coordinator = makeCoordinator(
            executor: RecordingCaptureExecutor(),
            gaps: gaps,
            epoch: epoch
        )
        await coordinator.activate(at: sample(wall: 0, monotonic: 0), startLoop: false)
        let active = await coordinator.snapshot()
        guard let generation = active.executionGeneration,
              let permit = await epoch.claimPersistence(for: generation)
        else { return XCTFail("expected active suspend generation and permit") }

        let suspend = Task { await coordinator.suspend() }
        await waitForInvalidation(.sleep, generation: generation, epoch: epoch)
        await coordinator.resume()
        await epoch.releasePersistence(permit)
        await suspend.value

        let final = await coordinator.snapshot()
        let reasons = await gaps.reasons
        XCTAssertEqual(reasons, [.sleep])
        XCTAssertEqual(final.state, .running)
        XCTAssertNotNil(final.nextTick)
    }

    func testDuplicateWakeDuringRecoveryDoesNotLeakIntoNextSuspend() async {
        let gaps = BlockingFirstGapReconciler()
        let coordinator = makeCoordinator(
            executor: RecordingCaptureExecutor(),
            gaps: gaps
        )
        await coordinator.activate(at: sample(wall: 0, monotonic: 0), startLoop: false)
        await coordinator.suspend()

        let wake = Task { await coordinator.resume() }
        await gaps.waitUntilStarted()
        await coordinator.resume()
        await gaps.release()
        await wake.value
        await coordinator.suspend()

        let final = await coordinator.snapshot()
        let reasons = await gaps.reasons
        XCTAssertEqual(reasons, [.sleep])
        XCTAssertEqual(final.state, .suspended)
        XCTAssertNil(final.nextTick)
    }

    func testClockChangeDuringWakeRecoveryPreservesSleepThenCorrectsClock() async {
        let gap = BlockingFirstGapReconciler()
        let coordinator = makeCoordinator(
            executor: RecordingCaptureExecutor(),
            gaps: gap
        )
        await coordinator.activate(at: sample(wall: 0, monotonic: 0), startLoop: false)
        await coordinator.suspend()

        let wake = Task { await coordinator.resume() }
        await gap.waitUntilStarted()
        await coordinator.wallClockChanged()
        await gap.release()
        await wake.value

        let reasons = await gap.reasons
        let state = await coordinator.snapshot()
        XCTAssertEqual(reasons, [.sleep, .clockCorrection])
        XCTAssertEqual(state.state, .running)
        XCTAssertNotNil(state.nextTick)
    }

    func testClockChangeBeforeWakeDoesNotReplaceOrPrematurelyCloseSleepGap() async {
        let gap = RecordingGapReconciler()
        let coordinator = makeCoordinator(
            executor: RecordingCaptureExecutor(),
            gaps: gap
        )
        await coordinator.activate(at: sample(wall: 0, monotonic: 0), startLoop: false)
        await coordinator.suspend()

        await coordinator.wallClockChanged()
        let beforeWake = await coordinator.snapshot()
        let beforeReasons = await gap.reasons
        XCTAssertEqual(beforeWake.state, .suspended)
        XCTAssertTrue(beforeReasons.isEmpty)

        await coordinator.resume()
        let afterWake = await coordinator.snapshot()
        let afterReasons = await gap.reasons
        XCTAssertEqual(afterReasons, [.clockCorrection, .sleep])
        XCTAssertEqual(afterWake.state, .running)
    }

    func testRealCoreBackwardClockChangeBeforeWakePreservesRecovery() async throws {
        let temporary = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: temporary, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: temporary) }
        let startedAt = Date(timeIntervalSince1970: 1_784_016_600)
        let core = try InProcessCore(applicationSupportURL: temporary, now: startedAt)
        try await callControl(
            core,
            at: startedAt,
            control: [
                "type": "startup-reconcile",
                "session_id": "swift-backward-wake",
                "device_id": "device-backward-wake",
                "display_timezone": "Europe/Zurich",
            ]
        )
        try await callControl(
            core,
            at: startedAt,
            control: ["type": "set-recording-preference", "enabled": true]
        )
        let client = CoreCaptureControlClient(
            core: core,
            deviceID: "device-backward-wake",
            displayTimezone: "Europe/Zurich"
        )
        let clock = MutableCaptureClock(
            sample: CaptureClockSample(
                wallTime: startedAt,
                monotonicNanoseconds: 1_000
            )
        )
        let coordinator = makeCoordinator(
            executor: RecordingCaptureExecutor(),
            admission: client,
            preferences: client,
            clock: clock,
            storage: client,
            gaps: client
        )
        await coordinator.activate(
            at: CaptureClockSample(wallTime: startedAt, monotonicNanoseconds: 1_000),
            startLoop: false
        )
        await coordinator.suspend()
        await clock.set(CaptureClockSample(
            wallTime: startedAt.addingTimeInterval(-300),
            monotonicNanoseconds: 2_000
        ))
        await coordinator.wallClockChanged()
        await clock.set(CaptureClockSample(
            wallTime: startedAt.addingTimeInterval(-240),
            monotonicNanoseconds: 3_000
        ))
        await coordinator.resume()

        let state = await coordinator.snapshot()
        XCTAssertEqual(state.state, .running)
        XCTAssertNotNil(state.nextTick)
        try await core.close()
    }

    func testRealCorePreferenceTransitionWaitsForPersistencePermitOnPauseAndResume() async throws {
        let temporary = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: temporary, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: temporary) }
        let startedAt = Date(timeIntervalSince1970: 1_784_016_000)
        let core = try InProcessCore(applicationSupportURL: temporary, now: startedAt)
        try await callControl(
            core,
            at: startedAt,
            control: [
                "type": "startup-reconcile",
                "session_id": "swift-preference-ordering",
                "device_id": "device-preference-ordering",
                "display_timezone": "Europe/Zurich",
            ]
        )
        try await callControl(
            core,
            at: startedAt,
            control: ["type": "set-recording-preference", "enabled": true]
        )
        let client = CoreCaptureControlClient(
            core: core,
            deviceID: "device-preference-ordering",
            displayTimezone: "Europe/Zurich"
        )
        let preferences = ObservingPreferenceSetter(base: client)
        let epoch = CaptureExecutionEpoch()
        let coordinator = makeCoordinator(
            executor: RecordingCaptureExecutor(),
            admission: client,
            preferences: preferences,
            storage: client,
            gaps: client,
            epoch: epoch
        )
        await coordinator.activate(at: sample(wall: 0, monotonic: 0), startLoop: false)
        let initialSnapshot = await coordinator.snapshot()
        guard let pauseGeneration = initialSnapshot.executionGeneration,
              let pausePermit = await epoch.claimPersistence(for: pauseGeneration)
        else { return XCTFail("expected active pause generation and permit") }

        let pause = Task { await coordinator.recordingPreferenceChanged(enabled: false) }
        await waitForInvalidation(.userPaused, generation: pauseGeneration, epoch: epoch)
        let callsBeforePauseRelease = await preferences.values
        let admissionBeforePauseRelease = await client.admission(at: startedAt)
        XCTAssertTrue(callsBeforePauseRelease.isEmpty)
        XCTAssertEqual(admissionBeforePauseRelease, .allowed)
        await epoch.releasePersistence(pausePermit)
        await pause.value
        let pauseCalls = await preferences.values
        let pausedAdmission = await client.admission(at: startedAt)
        XCTAssertEqual(pauseCalls, [false])
        XCTAssertEqual(pausedAdmission, .userPaused)

        let pausedSnapshot = await coordinator.snapshot()
        guard let resumeGeneration = pausedSnapshot.executionGeneration,
              let resumePermit = await epoch.claimPersistence(for: resumeGeneration)
        else { return XCTFail("expected active resume generation and permit") }
        let resume = Task { await coordinator.recordingPreferenceChanged(enabled: true) }
        await waitForInvalidation(.userPaused, generation: resumeGeneration, epoch: epoch)
        let callsBeforeResumeRelease = await preferences.values
        let admissionBeforeResumeRelease = await client.admission(at: startedAt)
        XCTAssertEqual(callsBeforeResumeRelease, [false])
        XCTAssertEqual(admissionBeforeResumeRelease, .userPaused)
        await epoch.releasePersistence(resumePermit)
        await resume.value
        let resumeCalls = await preferences.values
        let resumedAdmission = await client.admission(at: startedAt)
        let resumedSnapshot = await coordinator.snapshot()
        XCTAssertEqual(resumeCalls, [false, true])
        XCTAssertEqual(resumedAdmission, .allowed)
        XCTAssertEqual(resumedSnapshot.state, .running)
        try await core.close()
    }

    private func makeCoordinator(
        executor: RecordingCaptureExecutor,
        admission: any CaptureAdmissionChecking = SequenceAdmission([.allowed]),
        preferences: any CaptureRecordingPreferenceSetting = RecordingPreferenceSetter(),
        clock: any CaptureSchedulerClock = MutableCaptureClock(
            sample: CaptureClockSample(
                wallTime: Date(timeIntervalSince1970: 0),
                monotonicNanoseconds: 0
            )
        ),
        storage: any CaptureStorageHealthChecking = SequenceStorageHealth([.writable]),
        gaps: any CaptureRuntimeGapReconciling = RecordingGapReconciler(),
        epoch: CaptureExecutionEpoch = CaptureExecutionEpoch()
    ) -> CaptureCoordinator {
        return CaptureCoordinator(
            cadenceSeconds: 30,
            clock: clock,
            contextSource: DeterministicContextSource(),
            executor: executor,
            admission: admission,
            preferences: preferences,
            storage: storage,
            gaps: gaps,
            epoch: epoch
        )
    }

    private func waitForInvalidation(
        _ expected: CaptureInvalidation,
        generation: UInt64,
        epoch: CaptureExecutionEpoch
    ) async {
        while await epoch.invalidation(for: generation) != expected {
            await Task.yield()
        }
    }

    private func callControl(
        _ core: InProcessCore,
        at date: Date,
        control: [String: Any]
    ) async throws {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        let request = try JSONSerialization.data(withJSONObject: [
            "schema_version": "1.0",
            "now": formatter.string(from: date),
            "control": control,
        ])
        _ = try await core.call(request)
    }

    private func sample(
        wall: TimeInterval,
        monotonic: UInt64
    ) -> CaptureClockSample {
        CaptureClockSample(
            wallTime: Date(timeIntervalSince1970: wall),
            monotonicNanoseconds: monotonic
        )
    }
}

private actor MutableCaptureClock: CaptureSchedulerClock {
    private var value: CaptureClockSample

    init(sample: CaptureClockSample) { value = sample }

    func sample() -> CaptureClockSample { value }

    func set(_ sample: CaptureClockSample) {
        value = sample
    }

    func sleep(untilMonotonicNanoseconds deadline: UInt64) async throws {
        throw CancellationError()
    }
}

private actor DeterministicContextSource: CaptureAttemptContextCreating {
    func makeContext(
        for tick: PlannedCaptureTick,
        executionGeneration: UInt64
    ) -> CaptureAttemptContext {
        testAttemptContext(
            token: "scheduled-\(tick.ordinal)",
            scheduledAt: tick.scheduledAt,
            monotonicTick: tick.ordinal,
            executionGeneration: executionGeneration
        )
    }
}

private actor SequenceAdmission: CaptureAdmissionChecking {
    private var values: [CaptureAdmissionDecision]

    init(_ values: [CaptureAdmissionDecision]) { self.values = values }

    func admission(at: Date) -> CaptureAdmissionDecision {
        guard !values.isEmpty else { return .allowed }
        if values.count == 1 { return values[0] }
        return values.removeFirst()
    }
}

private actor RecordingPreferenceSetter: CaptureRecordingPreferenceSetting {
    private(set) var values: [Bool] = []

    func setRecordingEnabled(
        _ enabled: Bool,
        at: Date
    ) -> CapturePersistenceFailure? {
        values.append(enabled)
        return nil
    }
}

private actor ObservingPreferenceSetter: CaptureRecordingPreferenceSetting {
    private let base: any CaptureRecordingPreferenceSetting
    private(set) var values: [Bool] = []

    init(base: any CaptureRecordingPreferenceSetting) {
        self.base = base
    }

    func setRecordingEnabled(
        _ enabled: Bool,
        at date: Date
    ) async -> CapturePersistenceFailure? {
        values.append(enabled)
        return await base.setRecordingEnabled(enabled, at: date)
    }
}

private actor BlockingPreferenceSetter: CaptureRecordingPreferenceSetting {
    private var started = false
    private var continuation: CheckedContinuation<Void, Never>?

    func setRecordingEnabled(
        _ enabled: Bool,
        at: Date
    ) async -> CapturePersistenceFailure? {
        started = true
        await withCheckedContinuation { continuation = $0 }
        return nil
    }

    func waitUntilStarted() async {
        while !started { await Task.yield() }
    }

    func release() {
        continuation?.resume()
        continuation = nil
    }
}

private actor BlockingAdmission: CaptureAdmissionChecking {
    private let result: CaptureAdmissionDecision
    private var started = false
    private var continuation: CheckedContinuation<Void, Never>?

    init(result: CaptureAdmissionDecision) { self.result = result }

    func admission(at: Date) async -> CaptureAdmissionDecision {
        started = true
        await withCheckedContinuation { continuation = $0 }
        return result
    }

    func waitUntilStarted() async {
        while !started { await Task.yield() }
    }

    func release() {
        continuation?.resume()
        continuation = nil
    }
}

private actor SequenceStorageHealth: CaptureStorageHealthChecking {
    private var values: [CaptureStorageHealthDecision]

    init(_ values: [CaptureStorageHealthDecision]) { self.values = values }

    func storageHealth(at: Date) -> CaptureStorageHealthDecision {
        guard !values.isEmpty else { return .writable }
        if values.count == 1 { return values[0] }
        return values.removeFirst()
    }
}

private actor RecordingGapReconciler: CaptureRuntimeGapReconciling {
    private(set) var reasons: [CaptureRuntimeGapReason] = []
    var failure: CapturePersistenceFailure?
    private let delayNanoseconds: UInt64

    init(delayNanoseconds: UInt64 = 0) {
        self.delayNanoseconds = delayNanoseconds
    }

    func reconcile(
        reason: CaptureRuntimeGapReason,
        at: Date
    ) async -> CapturePersistenceFailure? {
        reasons.append(reason)
        if delayNanoseconds > 0 {
            try? await Task.sleep(nanoseconds: delayNanoseconds)
        }
        return failure
    }
}

private actor BlockingFirstGapReconciler: CaptureRuntimeGapReconciling {
    private(set) var reasons: [CaptureRuntimeGapReason] = []
    private var firstStarted = false
    private var continuation: CheckedContinuation<Void, Never>?

    func reconcile(
        reason: CaptureRuntimeGapReason,
        at: Date
    ) async -> CapturePersistenceFailure? {
        reasons.append(reason)
        if reasons.count == 1 {
            firstStarted = true
            await withCheckedContinuation { continuation = $0 }
        }
        return nil
    }

    func waitUntilStarted() async {
        while !firstStarted { await Task.yield() }
    }

    func release() {
        continuation?.resume()
        continuation = nil
    }
}

private actor RecordingCaptureExecutor: CaptureAttemptExecuting {
    struct Snapshot {
        let contexts: [CaptureAttemptContext]
        let directives: [CaptureAttemptDirective]
        let maximumConcurrent: Int
    }

    private var results: [CaptureAttemptResult]
    private let delayNanoseconds: UInt64
    private var contexts: [CaptureAttemptContext] = []
    private var directives: [CaptureAttemptDirective] = []
    private var concurrent = 0
    private var maximumConcurrent = 0

    init(
        results: [CaptureAttemptResult] = [],
        delayNanoseconds: UInt64 = 0
    ) {
        self.results = results
        self.delayNanoseconds = delayNanoseconds
    }

    func attempt(
        context: CaptureAttemptContext,
        directive: CaptureAttemptDirective,
        proofToken: CaptureProofToken?
    ) async -> CaptureAttemptResult {
        contexts.append(context)
        directives.append(directive)
        concurrent += 1
        maximumConcurrent = max(maximumConcurrent, concurrent)
        if delayNanoseconds > 0 {
            try? await Task.sleep(nanoseconds: delayNanoseconds)
        }
        concurrent -= 1
        if results.isEmpty {
            return .stored(CaptureIngestAcknowledgement(
                durability: .durable,
                eventID: context.eventID,
                ocrEventID: nil,
                imageArtifactID: nil
            ))
        }
        return results.removeFirst()
    }

    func snapshot() -> Snapshot {
        Snapshot(
            contexts: contexts,
            directives: directives,
            maximumConcurrent: maximumConcurrent
        )
    }
}
