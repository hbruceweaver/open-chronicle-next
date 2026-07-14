import Foundation
import XCTest
@testable import OpenChronicle

final class NotificationTests: XCTestCase {
    func testPermissionNotificationDeduplicatesAndRepeatsAfterRateLimit() async {
        let backend = NotificationBackendProbe(authorization: .authorized)
        let service = NotificationService(backend: backend)
        let started = Date(timeIntervalSince1970: 1_784_016_000)

        await service.evaluate(
            captureStatus: .unavailable(.permissionDenied),
            health: nil,
            at: started
        )
        await service.evaluate(
            captureStatus: .unavailable(.permissionDenied),
            health: nil,
            at: started.addingTimeInterval(60)
        )
        var delivered = await backend.deliveredValues()
        XCTAssertEqual(delivered.map(\.kind), [.permissionLost])
        XCTAssertEqual(delivered.first?.route, .settings)

        await service.evaluate(
            captureStatus: .unavailable(.permissionDenied),
            health: nil,
            at: started.addingTimeInterval(NotificationService.incidentRepeatInterval)
        )
        delivered = await backend.deliveredValues()
        XCTAssertEqual(delivered.map(\.kind), [.permissionLost, .permissionLost])
    }

    func testDeniedNotificationPermissionKeepsSystemDeliveryOptional() async {
        let backend = NotificationBackendProbe(authorization: .denied)
        let service = NotificationService(backend: backend)

        await service.evaluate(
            captureStatus: .storageBlocked,
            health: nil,
            at: Date(timeIntervalSince1970: 1)
        )

        let delivered = await backend.deliveredValues()
        XCTAssertTrue(delivered.isEmpty)
        let requestCount = await backend.authorizationRequestCount()
        XCTAssertEqual(requestCount, 0, "runtime evaluation must not prompt for permission")
    }

    func testRecoveryAppearsOnceAfterAllBlockingConditionsClear() async {
        let backend = NotificationBackendProbe(authorization: .authorized)
        let service = NotificationService(backend: backend)
        let started = Date(timeIntervalSince1970: 1_784_016_000)

        await service.evaluate(
            captureStatus: .unavailable(.permissionDenied),
            health: blockedStorageHealth(observedAt: started),
            at: started
        )
        await service.evaluate(
            captureStatus: .sleeping,
            health: blockedStorageHealth(observedAt: started),
            at: started.addingTimeInterval(0.25)
        )
        await service.evaluate(
            captureStatus: .paused,
            health: blockedStorageHealth(observedAt: started),
            at: started.addingTimeInterval(0.5)
        )
        await service.evaluate(
            captureStatus: .stopped,
            health: blockedStorageHealth(observedAt: started),
            at: started.addingTimeInterval(0.75)
        )
        var delivered = await backend.deliveredValues()
        XCTAssertEqual(delivered.map(\.kind), [.permissionLost, .storageFailure])

        await service.evaluate(
            captureStatus: .recording,
            health: healthyNotificationHealth(
                observedAt: started.addingTimeInterval(2),
                successfulCaptureAt: started.addingTimeInterval(1.5)
            ),
            at: started.addingTimeInterval(2)
        )
        await service.evaluate(
            captureStatus: .recording,
            health: healthyNotificationHealth(
                observedAt: started.addingTimeInterval(3),
                successfulCaptureAt: started.addingTimeInterval(1.5)
            ),
            at: started.addingTimeInterval(3)
        )

        delivered = await backend.deliveredValues()
        XCTAssertEqual(delivered.map(\.kind), [
            .permissionLost,
            .storageFailure,
            .recovered,
        ])
        XCTAssertEqual(delivered.last?.route, .health)
    }

    func testPermissionIncidentDoesNotRecoverThroughPauseSleepOrStop() async {
        let backend = NotificationBackendProbe(authorization: .authorized)
        let service = NotificationService(backend: backend)
        let started = Date(timeIntervalSince1970: 1_784_016_000)

        await service.evaluate(
            captureStatus: .unavailable(.permissionDenied),
            health: healthyNotificationHealth(observedAt: started),
            at: started
        )
        for (offset, status) in [
            (1.0, CapturePresentationState.paused),
            (2.0, CapturePresentationState.sleeping),
            (3.0, CapturePresentationState.stopped),
            (4.0, CapturePresentationState.recording),
        ] {
            await service.evaluate(
                captureStatus: status,
                health: healthyNotificationHealth(
                    observedAt: started.addingTimeInterval(offset)
                ),
                at: started.addingTimeInterval(offset)
            )
        }

        var delivered = await backend.deliveredValues()
        XCTAssertEqual(delivered.map(\.kind), [.permissionLost])
        await service.evaluate(
            captureStatus: .recording,
            health: healthyNotificationHealth(
                observedAt: started.addingTimeInterval(6),
                successfulCaptureAt: started.addingTimeInterval(5)
            ),
            at: started.addingTimeInterval(6)
        )
        delivered = await backend.deliveredValues()
        XCTAssertEqual(delivered.map(\.kind), [.permissionLost, .recovered])
    }

    func testStudyWarningKeysDedupeToConfiguredEndAndExpiry() async {
        let backend = NotificationBackendProbe(authorization: .authorized)
        let service = NotificationService(backend: backend)
        let started = Date(timeIntervalSince1970: 1_784_016_000)
        let firstEnd = started.addingTimeInterval(10 * 60)
        let secondEnd = started.addingTimeInterval(12 * 60)

        await service.evaluate(
            captureStatus: .recording,
            health: studyHealth(state: .active, end: firstEnd),
            at: started
        )
        await service.evaluate(
            captureStatus: .recording,
            health: studyHealth(state: .active, end: firstEnd),
            at: started.addingTimeInterval(1)
        )
        await service.evaluate(
            captureStatus: .recording,
            health: studyHealth(state: .active, end: secondEnd),
            at: started.addingTimeInterval(2)
        )
        await service.evaluate(
            captureStatus: .studyExpired,
            health: studyHealth(state: .expired, end: secondEnd),
            at: secondEnd
        )

        let delivered = await backend.deliveredValues()
        XCTAssertEqual(delivered.map(\.kind), [
            .studyPreExpiry,
            .studyPreExpiry,
            .studyExpired,
        ])
    }

    @MainActor
    func testNotificationRouteDecodesOnlyTypedDestinations() {
        XCTAssertEqual(
            ChronicleNotificationRoute(userInfo: ["route": "health"]),
            .health
        )
        XCTAssertNil(ChronicleNotificationRoute(userInfo: ["route": "timeline/event/secret"]))
        let navigation = NavigationModel()
        navigation.show(notificationRoute: .settings)
        XCTAssertEqual(navigation.path, [.settings])
    }
}

private enum NotificationProbeError: Error {
    case failed
}

private actor NotificationBackendProbe: ChronicleNotificationDelivering {
    private var authorization: ChronicleNotificationAuthorization
    private var delivered: [ChronicleNotificationMessage] = []
    private var requests = 0
    private var deliveryFails = false

    init(authorization: ChronicleNotificationAuthorization) {
        self.authorization = authorization
    }

    func authorizationState() -> ChronicleNotificationAuthorization {
        authorization
    }

    func requestAuthorization() throws -> Bool {
        requests += 1
        return authorization == .authorized
    }

    func deliver(_ message: ChronicleNotificationMessage) throws {
        if deliveryFails { throw NotificationProbeError.failed }
        delivered.append(message)
    }

    func deliveredValues() -> [ChronicleNotificationMessage] { delivered }
    func authorizationRequestCount() -> Int { requests }
}

private func healthyNotificationHealth(
    observedAt: Date = Date(timeIntervalSince1970: 1_784_016_000),
    successfulCaptureAt: Date? = nil
) -> DiagnosticHealthSnapshot {
    var snapshot = notificationHealth(
        observedAt: observedAt,
        successfulCaptureAt: successfulCaptureAt,
        study: DiagnosticStudySummary(state: .personal, start: nil, end: nil, expiredAt: nil),
        storage: DiagnosticStorageSummary(
            managedBytes: 0,
            availableBytes: 100 * OperationalStoragePolicy.gibibyte
        )
    )
    snapshot.screenshotStorage = screenshotStorage(state: .healthy)
    return snapshot
}

private func blockedStorageHealth(observedAt: Date) -> DiagnosticHealthSnapshot {
    var snapshot = notificationHealth(
        observedAt: observedAt,
        study: DiagnosticStudySummary(state: .personal, start: nil, end: nil, expiredAt: nil),
        storage: DiagnosticStorageSummary(
            managedBytes: 0,
            availableBytes: 100 * OperationalStoragePolicy.gibibyte
        )
    )
    snapshot.screenshotStorage = screenshotStorage(state: .blockedImageQuota)
    return snapshot
}

private func studyHealth(
    state: DiagnosticStudyState,
    end: Date
) -> DiagnosticHealthSnapshot {
    notificationHealth(
        observedAt: end.addingTimeInterval(-600),
        study: DiagnosticStudySummary(
            state: state,
            start: ChronicleTimestamp.string(end.addingTimeInterval(-3_600)),
            end: ChronicleTimestamp.string(end),
            expiredAt: state == .expired ? ChronicleTimestamp.string(end) : nil
        ),
        storage: DiagnosticStorageSummary(
            managedBytes: 0,
            availableBytes: 100 * OperationalStoragePolicy.gibibyte
        )
    )
}

private func notificationHealth(
    observedAt: Date,
    successfulCaptureAt: Date? = nil,
    study: DiagnosticStudySummary,
    storage: DiagnosticStorageSummary
) -> DiagnosticHealthSnapshot {
    DiagnosticHealthSnapshot(
        schemaVersion: "1.0",
        observedAt: ChronicleTimestamp.string(observedAt),
        storeGeneration: 1,
        projection: .current,
        acknowledgement: .durable,
        latest: DiagnosticOperationTimes(
            lastScheduledAttemptAt: nil,
            lastSuccessfulCaptureAt: successfulCaptureAt.map(ChronicleTimestamp.string),
            lastSuccessfulOCRAt: nil,
            lastJournalAt: nil,
            lastProjectionAt: nil,
            lastChunkAt: nil
        ),
        aggregationWatermark: nil,
        aggregationPendingBuckets: 0,
        projectionLagSeconds: 0,
        projectionPendingRecords: 0,
        storage: storage,
        study: study,
        screenshotRetention: DiagnosticScreenshotRetentionSummary(
            writePending: 0,
            retained: 0,
            deletePending: 0,
            expired: 0,
            userDeleted: 0,
            missing: 0,
            writeFailed: 0,
            nextExpiryAt: nil
        ),
        mcp: DiagnosticMCPHealthSummary(
            activeGrants: 0,
            revokedGrants: 0,
            expiredGrants: 0,
            exhaustedGrants: 0,
            staleGenerationGrants: 0
        ),
        issues: []
    )
}

private func screenshotStorage(
    state: DiagnosticScreenshotStorageState
) -> DiagnosticScreenshotStorageSummary {
    let gib = OperationalStoragePolicy.gibibyte
    return DiagnosticScreenshotStorageSummary(
        managedImageBytes: state == .blockedImageQuota ? 20 * gib : 0,
        availableBytes: 100 * gib,
        warningFreeBytes: 4 * gib,
        minimumFreeBytes: 2 * gib,
        managedImageQuotaBytes: 20 * gib,
        journalReserveBytes: 4 * 1024 * 1024,
        state: state
    )
}
