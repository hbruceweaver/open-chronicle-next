import Foundation
import XCTest
@testable import OpenChronicle

final class ReportParityTests: XCTestCase {
    @MainActor
    func testNonEmptyRustFactualReportDrivesHomeTotalsAndEvidenceIDs() async throws {
        let temporary = FileManager.default.temporaryDirectory
            .appendingPathComponent("open-chronicle-report-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: temporary, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: temporary) }
        let start = try date("2026-07-13T09:00:00Z")
        let core = try InProcessCore(applicationSupportURL: temporary, now: start)
        try await startRecording(core, at: start)
        try await ingestCaptured(
            core: core,
            scheduledAt: start.addingTimeInterval(15),
            token: "report-1",
            tick: 1
        )
        try await ingestCaptured(
            core: core,
            scheduledAt: start.addingTimeInterval(45),
            token: "report-2",
            tick: 2
        )
        try await ingestFinalizationTrigger(
            core: core,
            scheduledAt: start.addingTimeInterval(362),
            tick: 3
        )
        let now = start.addingTimeInterval(420)
        let snapshot = try await CoreFactualReportClient(core: core).report(
            range: FactualReportRange(
                start: start,
                end: start.addingTimeInterval(300)
            ),
            now: now
        )
        let model = HomeViewModel(
            client: ReportParityClient(snapshot: snapshot),
            calendar: Calendar(identifier: .gregorian),
            timeZone: try XCTUnwrap(TimeZone(identifier: "UTC")),
            nowProvider: { now }
        )

        await model.load(now: now)

        let writer = try XCTUnwrap(model.applicationBreakdown.first)
        XCTAssertEqual(writer.application.key, "com.example.report-writer")
        XCTAssertGreaterThan(writer.application.estimatedSeconds, 0)
        XCTAssertEqual(
            writer.application.supportingEventIDs,
            ["event-report-1", "event-report-2"]
        )
        XCTAssertEqual(writer.application.supportingChunkIDs.count, 1)
        XCTAssertEqual(model.recentBuckets.count, 1)
        XCTAssertEqual(
            model.recentBuckets.first?.chunkID,
            writer.application.supportingChunkIDs.first
        )
        XCTAssertEqual(model.metrics?.observedComputerSeconds, UInt64(
            writer.application.estimatedSeconds
        ))
        XCTAssertEqual(model.snapshot?.provenance.sourceEventIDs.contains(
            "event-report-1"
        ), true)
        XCTAssertTrue(model.domainBreakdown.isEmpty)
        try await core.close()
    }

    func testExplicitDisplayTimezoneChangesRenderedClockTime() throws {
        let instant = try date("2026-07-14T12:00:00Z")
        let zurich = try XCTUnwrap(TimeZone(identifier: "Europe/Zurich"))
        let losAngeles = try XCTUnwrap(TimeZone(identifier: "America/Los_Angeles"))
        let locale = Locale(identifier: "en_US_POSIX")

        let zurichText = HomeReportFormatter.clockTime(
            instant,
            timeZone: zurich,
            locale: locale
        )
        let losAngelesText = HomeReportFormatter.clockTime(
            instant,
            timeZone: losAngeles,
            locale: locale
        )
        XCTAssertTrue(zurichText.hasPrefix("2:00"), zurichText)
        XCTAssertTrue(losAngelesText.hasPrefix("5:00"), losAngelesText)
        XCTAssertNotEqual(zurichText, losAngelesText)
    }

    private func startRecording(_ core: InProcessCore, at instant: Date) async throws {
        let now = ChronicleTimestamp.string(instant)
        for control in [
            [
                "type": "startup-reconcile",
                "session_id": "swift-report-parity",
                "device_id": "device-report-parity",
                "display_timezone": "Europe/Zurich",
            ] as [String: Any],
            [
                "type": "set-recording-preference",
                "enabled": true,
            ] as [String: Any],
        ] {
            let request = try JSONSerialization.data(withJSONObject: [
                "schema_version": "1.0",
                "now": now,
                "control": control,
            ])
            _ = try await core.call(request)
        }
    }

    private func ingestCaptured(
        core: InProcessCore,
        scheduledAt: Date,
        token: String,
        tick: UInt64
    ) async throws {
        let context = testAttemptContext(
            token: token,
            scheduledAt: scheduledAt,
            monotonicTick: tick
        )
        let ingestor = CoreCaptureIngestor(
            core: core,
            recordingTime: FixedCaptureTimeSource(value: scheduledAt.addingTimeInterval(2))
        )
        _ = try await ingestor.ingest(
            record: .changed(
                context: ApprovedWindowContext(
                    applicationBundleID: "com.example.report-writer",
                    processName: "Report Writer",
                    windowTitle: "Quarterly facts"
                ),
                contentHash: "sha256-\(token)",
                ocrChange: .new,
                ocr: .complete(
                    text: "Factual report evidence \(token)",
                    confidence: 0.95,
                    provenance: OCRProvenance(
                        engineAdapter: "apple-vision-vnrecognizetextrequest",
                        engineVersion: "report-parity",
                        automaticLanguageDetection: true,
                        recognitionLanguages: []
                    )
                ),
                dimensions: nil,
                presence: .active
            ),
            image: nil,
            context: context,
            observedAt: scheduledAt.addingTimeInterval(1),
            permit: CapturePersistencePermit(
                id: UUID(),
                executionGeneration: context.executionGeneration
            )
        )
    }

    private func ingestFinalizationTrigger(
        core: InProcessCore,
        scheduledAt: Date,
        tick: UInt64
    ) async throws {
        let context = testAttemptContext(
            token: "report-trigger",
            scheduledAt: scheduledAt,
            monotonicTick: tick
        )
        let ingestor = CoreCaptureIngestor(
            core: core,
            recordingTime: FixedCaptureTimeSource(value: scheduledAt.addingTimeInterval(2))
        )
        _ = try await ingestor.ingest(
            record: .denied(reason: .permissionDenied, presence: .unknown),
            image: nil,
            context: context,
            observedAt: scheduledAt.addingTimeInterval(1),
            permit: CapturePersistencePermit(
                id: UUID(),
                executionGeneration: context.executionGeneration
            )
        )
    }
}

private actor ReportParityClient: FactualReportQuerying {
    let snapshot: FactualReportSnapshot

    init(snapshot: FactualReportSnapshot) {
        self.snapshot = snapshot
    }

    func report(range: FactualReportRange, now: Date) -> FactualReportSnapshot {
        snapshot
    }
}

private func date(_ value: String) throws -> Date {
    try XCTUnwrap(ChronicleTimestamp.date(value))
}
