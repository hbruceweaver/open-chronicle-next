import Foundation
import XCTest
@testable import OpenChronicle

final class HealthTests: XCTestCase {
    func testRealCoreHealthClientReturnsContentFreeOperationalSnapshot() async throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let now = Date(timeIntervalSince1970: 1_784_016_000)
        let core = try InProcessCore(applicationSupportURL: directory, now: now)
        let client = CoreDiagnosticHealthClient(core: core)

        let snapshot = try await client.fetch(at: now.addingTimeInterval(1))

        XCTAssertEqual(snapshot.schemaVersion, "1.0")
        XCTAssertEqual(snapshot.storeGeneration, 1)
        XCTAssertEqual(snapshot.projection, .current)
        XCTAssertEqual(snapshot.acknowledgement, .durable)
        XCTAssertEqual(snapshot.study.state, .personal)
        XCTAssertGreaterThan(snapshot.storage.availableBytes, 0)
        let screenshotStorage = try XCTUnwrap(snapshot.screenshotStorage)
        let expectedScreenshotState: DiagnosticScreenshotStorageState =
            if screenshotStorage.managedImageBytes >= screenshotStorage.managedImageQuotaBytes {
                .blockedImageQuota
            } else if screenshotStorage.availableBytes < screenshotStorage.minimumFreeBytes {
                .blockedFreeSpace
            } else if screenshotStorage.availableBytes < screenshotStorage.warningFreeBytes {
                .warning
            } else {
                .healthy
            }
        XCTAssertEqual(screenshotStorage.state, expectedScreenshotState)
        XCTAssertEqual(screenshotStorage.managedImageBytes, 0)
        let encoded = String(decoding: try JSONEncoder().encode(snapshot), as: UTF8.self)
        for forbidden in ["ocr_text", "window_title", "application_name", "screenshot_path"] {
            XCTAssertFalse(encoded.contains(forbidden), "health leaked \(forbidden)")
        }
        try await core.close()
    }

    func testStorageMonitorPublishesSuccessAndTypedFailure() async {
        let recorder = StorageMonitorRecorder()
        let fetcher = SequenceHealthFetcher(results: [
            .success(healthFixture()),
            .failure(HealthTestError.unavailable),
        ])
        let monitor = StorageMonitor(fetcher: fetcher) { update in
            await recorder.append(update)
        }

        await monitor.refresh(at: Date(timeIntervalSince1970: 1))
        await monitor.refresh(at: Date(timeIntervalSince1970: 2))

        let updates = await recorder.values
        XCTAssertEqual(updates.first, .snapshot(healthFixture()))
        guard case let .failed(message) = updates.last else {
            return XCTFail("expected a typed monitor failure")
        }
        XCTAssertFalse(message.isEmpty)
    }

    @MainActor
    func testHealthViewModelKeepsLastGoodSnapshotWhenRefreshFails() async {
        let fixture = healthFixture()
        let fetcher = SequenceHealthFetcher(results: [
            .success(fixture),
            .failure(HealthTestError.unavailable),
        ])
        let model = HealthViewModel()
        model.attach(fetcher: fetcher)

        await model.refresh(at: Date(timeIntervalSince1970: 1))
        XCTAssertEqual(model.snapshot, fixture)
        XCTAssertNil(model.lastError)
        await model.refresh(at: Date(timeIntervalSince1970: 2))
        XCTAssertEqual(model.snapshot, fixture)
        XCTAssertNotNil(model.lastError)
        XCTAssertFalse(model.isRefreshing)
    }

    @MainActor
    func testStorageStateUsesWarningFloorAndManagedQuotaBoundaries() {
        let gib = HealthViewModel.gibibyte
        XCTAssertEqual(
            HealthViewModel.storageState(for: DiagnosticStorageSummary(
                managedBytes: 0,
                availableBytes: 4 * gib
            )),
            .healthy
        )
        XCTAssertEqual(
            HealthViewModel.storageState(for: DiagnosticStorageSummary(
                managedBytes: 0,
                availableBytes: 4 * gib - 1
            )),
            .warning
        )
        XCTAssertEqual(
            HealthViewModel.storageState(for: DiagnosticStorageSummary(
                managedBytes: 0,
                availableBytes: 2 * gib - 1
            )),
            .blocked
        )
        XCTAssertEqual(
            HealthViewModel.storageState(for: DiagnosticStorageSummary(
                managedBytes: 20 * gib,
                availableBytes: 100 * gib
            )),
            .healthy,
            "total managed data is not the managed screenshot quota"
        )
        var screenshotQuota = healthFixture()
        screenshotQuota.screenshotStorage = DiagnosticScreenshotStorageSummary(
            managedImageBytes: 20 * gib,
            availableBytes: 100 * gib,
            warningFreeBytes: 4 * gib,
            minimumFreeBytes: 2 * gib,
            managedImageQuotaBytes: 20 * gib,
            journalReserveBytes: 4 * 1024 * 1024,
            state: .blockedImageQuota
        )
        XCTAssertEqual(HealthViewModel.storageState(for: screenshotQuota), .blocked)
    }
}

private enum HealthTestError: Error {
    case unavailable
}

private actor SequenceHealthFetcher: DiagnosticHealthFetching {
    private var results: [Result<DiagnosticHealthSnapshot, HealthTestError>]

    init(results: [Result<DiagnosticHealthSnapshot, HealthTestError>]) {
        self.results = results
    }

    func fetch(at _: Date) throws -> DiagnosticHealthSnapshot {
        guard !results.isEmpty else { throw HealthTestError.unavailable }
        return try results.removeFirst().get()
    }
}

private actor StorageMonitorRecorder {
    private(set) var values: [StorageMonitorUpdate] = []

    func append(_ update: StorageMonitorUpdate) {
        values.append(update)
    }
}

private func healthFixture() -> DiagnosticHealthSnapshot {
    DiagnosticHealthSnapshot(
        schemaVersion: "1.0",
        observedAt: "2026-07-13T09:00:00Z",
        storeGeneration: 1,
        projection: .current,
        acknowledgement: .durable,
        latest: DiagnosticOperationTimes(
            lastScheduledAttemptAt: nil,
            lastSuccessfulCaptureAt: nil,
            lastSuccessfulOCRAt: nil,
            lastJournalAt: nil,
            lastProjectionAt: nil,
            lastChunkAt: nil
        ),
        aggregationWatermark: nil,
        aggregationPendingBuckets: 0,
        projectionLagSeconds: 0,
        projectionPendingRecords: 0,
        storage: DiagnosticStorageSummary(
            managedBytes: 0,
            availableBytes: 100 * HealthViewModel.gibibyte
        ),
        study: DiagnosticStudySummary(state: .personal, start: nil, end: nil, expiredAt: nil),
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
