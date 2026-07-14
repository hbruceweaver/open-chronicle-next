import Foundation
import XCTest
@testable import OpenChronicle

final class CaptureIngestorTests: XCTestCase {
    func testClockDiscontinuityIsRejectedBeforeCoreCall() async throws {
        let core = SpyCoreService()
        let instant = Date(timeIntervalSince1970: 1_784_016_000)
        let ingestor = CoreCaptureIngestor(
            core: core,
            recordingTime: FixedRecordingTimeSource(instant.addingTimeInterval(2))
        )

        do {
            _ = try await ingestor.ingest(
                record: .denied(reason: .permissionDenied, presence: .active),
                image: nil,
                context: context(instant: instant),
                observedAt: instant.addingTimeInterval(-1),
                permit: permit()
            )
            XCTFail("clock rollback should be rejected")
        } catch CoreCaptureIngestorError.clockDiscontinuity {
            // Expected: no event may span a wall-clock discontinuity.
        } catch {
            XCTFail("unexpected error: \(error)")
        }
        let capturedRequest = await core.lastRequest
        XCTAssertNil(capturedRequest)
    }

    func testFactoryRoundTripsChangedImageAndLifecycleThroughRealRustCore() async throws {
        let temporary = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: temporary, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: temporary) }
        let instant = Date(timeIntervalSince1970: 1_784_016_000)
        let core = try InProcessCore(applicationSupportURL: temporary, now: instant)
        try await startRecording(core, at: instant)
        let ingestor = CoreCaptureIngestor(
            core: core,
            recordingTime: FixedRecordingTimeSource(instant.addingTimeInterval(2))
        )

        let acknowledgement = try await ingestor.ingest(
            record: changedRecord(
                ocr: .complete(
                    text: "factual OCR",
                    confidence: 0.9,
                    provenance: provenance
                ),
                dimensions: CaptureImageDimensions(
                    width: 2,
                    height: 2,
                    scaleMilli: 2_000
                )
            ),
            image: Data([1, 2, 3]),
            context: context(instant: instant),
            observedAt: instant.addingTimeInterval(1),
            permit: permit()
        )

        XCTAssertTrue(acknowledgement.durability.isCanonicalDurable)
        XCTAssertEqual(acknowledgement.eventID, "event-u7-1")
        XCTAssertEqual(acknowledgement.ocrEventID, "event-u7-1")
        XCTAssertEqual(acknowledgement.imageArtifactID, "image-u7-1")
        try await core.close()
    }

    private func startRecording(_ core: InProcessCore, at instant: Date) async throws {
        let now = ISO8601DateFormatter().string(from: instant)
        for control in [
            [
                "type": "startup-reconcile",
                "session_id": "swift-u7-capture-test",
                "device_id": "device-u7",
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

    func testFailedOCRFactoryEmitsNoCanonicalOCRPayload() async throws {
        let core = SpyCoreService()
        let instant = Date(timeIntervalSince1970: 1_784_016_000)
        let ingestor = CoreCaptureIngestor(
            core: core,
            recordingTime: FixedRecordingTimeSource(instant.addingTimeInterval(2))
        )

        _ = try await ingestor.ingest(
            record: changedRecord(
                ocr: .failed(provenance: provenance),
                dimensions: nil
            ),
            image: nil,
            context: context(instant: instant),
            observedAt: instant.addingTimeInterval(1),
            permit: permit()
        )

        let capturedRequest = await core.lastRequest
        let request = try XCTUnwrap(capturedRequest)
        let json = try JSONSerialization.jsonObject(with: request) as! [String: Any]
        let event = json["event"] as! [String: Any]
        XCTAssertEqual(event["scheduled_at"] as? String, timestamp(instant))
        XCTAssertEqual(
            event["observed_at"] as? String,
            timestamp(instant.addingTimeInterval(1))
        )
        XCTAssertEqual(
            event["recorded_at"] as? String,
            timestamp(instant.addingTimeInterval(2))
        )
        let payload = event["payload"] as! [String: Any]
        let attempt = payload["data"] as! [String: Any]
        let content = attempt["content"] as! [String: Any]
        let data = content["data"] as! [String: Any]
        XCTAssertEqual(attempt["ocr_state"] as? String, "failed")
        XCTAssertTrue(data["ocr"] is NSNull)
    }

    func testProtectedFactoryContainsOnlyCoarsePolicyOutcome() async throws {
        let core = SpyCoreService()
        let instant = Date(timeIntervalSince1970: 1_784_016_000)
        let ingestor = CoreCaptureIngestor(
            core: core,
            recordingTime: FixedRecordingTimeSource(instant.addingTimeInterval(2))
        )

        _ = try await ingestor.ingest(
            record: .denied(reason: .applicationExcluded, presence: .active),
            image: nil,
            context: context(instant: instant),
            observedAt: instant.addingTimeInterval(1),
            permit: permit()
        )

        let capturedRequest = await core.lastRequest
        let request = try XCTUnwrap(capturedRequest)
        let text = try XCTUnwrap(String(data: request, encoding: .utf8))
        for forbidden in ["window_title", "application_bundle_id", "process_name", "ocr", "image"] {
            XCTAssertFalse(text.contains("\"\(forbidden)\""), "coarse denial leaked \(forbidden)")
        }
        XCTAssertTrue(text.contains("application-excluded"))
    }

    func testPausedAndUnavailableFactoriesUseClosedNoEvidencePayloads() async throws {
        for (reason, expectedEvidence) in [
            (CaptureDenial.userPaused, "paused"),
            (CaptureDenial.studyExpired, "paused"),
            (CaptureDenial.permissionDenied, "unavailable"),
            (CaptureDenial.noExactWindow, "unavailable"),
            (CaptureDenial.ambiguousWindow, "unavailable"),
        ] {
            let core = SpyCoreService()
            let instant = Date(timeIntervalSince1970: 1_784_016_000)
            let ingestor = CoreCaptureIngestor(
                core: core,
                recordingTime: FixedRecordingTimeSource(instant.addingTimeInterval(2))
            )
            _ = try await ingestor.ingest(
                record: .denied(reason: reason, presence: .active),
                image: nil,
                context: context(instant: instant),
                observedAt: instant.addingTimeInterval(1),
                permit: permit()
            )
            let capturedRequest = await core.lastRequest
            let request = try XCTUnwrap(capturedRequest)
            let json = try JSONSerialization.jsonObject(with: request) as! [String: Any]
            let event = json["event"] as! [String: Any]
            let payload = event["payload"] as! [String: Any]
            let attempt = payload["data"] as! [String: Any]
            let content = attempt["content"] as! [String: Any]
            XCTAssertEqual(attempt["evidence_state"] as? String, expectedEvidence)
            XCTAssertEqual(content["type"] as? String, "no-evidence")
            let contentData = content["data"] as! [String: Any]
            XCTAssertEqual(Set(contentData.keys), ["reason"])
            XCTAssertEqual(contentData["reason"] as? String, reason.rawValue)
        }
    }

    func testLockedAndAsleepUseMatchingPresenceWithoutSensitiveContext() async throws {
        for (reason, expectedPresence) in [
            (CaptureDenial.locked, "locked"),
            (CaptureDenial.asleep, "asleep"),
        ] {
            let core = SpyCoreService()
            let instant = Date(timeIntervalSince1970: 1_784_016_000)
            let ingestor = CoreCaptureIngestor(
                core: core,
                recordingTime: FixedRecordingTimeSource(instant.addingTimeInterval(2))
            )
            _ = try await ingestor.ingest(
                record: .denied(reason: reason, presence: .unknown),
                image: nil,
                context: context(instant: instant),
                observedAt: instant.addingTimeInterval(1),
                permit: permit()
            )
            let capturedRequest = await core.lastRequest
            let request = try XCTUnwrap(capturedRequest)
            let json = try JSONSerialization.jsonObject(with: request) as! [String: Any]
            let event = json["event"] as! [String: Any]
            let payload = event["payload"] as! [String: Any]
            let attempt = payload["data"] as! [String: Any]
            XCTAssertEqual(attempt["presence_state"] as? String, expectedPresence)
            let content = attempt["content"] as! [String: Any]
            XCTAssertEqual(content["type"] as? String, "no-evidence")
        }
    }

    private var provenance: OCRProvenance {
        OCRProvenance(
            engineAdapter: "apple-vision-vnrecognizetextrequest",
            engineVersion: "request-revision-3;macos-test",
            automaticLanguageDetection: true,
            recognitionLanguages: []
        )
    }

    private func changedRecord(
        ocr: OCRRecognition,
        dimensions: CaptureImageDimensions?
    ) -> CaptureIngestRecord {
        .changed(
            context: ApprovedWindowContext(
                applicationBundleID: "com.example.editor",
                processName: "Editor",
                windowTitle: "Notes"
            ),
            contentHash: "sha256-u7-test",
            ocrChange: .new,
            ocr: ocr,
            dimensions: dimensions,
            presence: .active
        )
    }

    private func context(instant: Date) -> CaptureAttemptContext {
        CaptureAttemptContext(
            eventID: "event-u7-1",
            lifecycleEventID: "lifecycle-u7-1",
            imageArtifactID: "image-u7-1",
            deviceID: "device-u7",
            scheduledAt: instant,
            displayTimezone: "Europe/Zurich",
            sourceVersion: "test-1",
            cadenceSeconds: 30,
            bootSequence: "boot-u7",
            monotonicTick: 1,
            retentionSeconds: 86_400,
            executionGeneration: 1
        )
    }

    private func timestamp(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.string(from: date)
    }

    private func permit() -> CapturePersistencePermit {
        CapturePersistencePermit(id: UUID(), executionGeneration: 1)
    }
}

private struct FixedRecordingTimeSource: CaptureRecordingTimeProviding {
    let value: Date
    init(_ value: Date) { self.value = value }
    func now() -> Date { value }
}

private actor SpyCoreService: CoreService {
    private(set) var lastRequest: Data?
    private(set) var lastImage: Data?

    func schemaIdentity() -> ChronicleSchemaIdentity {
        ChronicleSchemaIdentity(abiSchemaVersion: "1.0", contractSchemaVersion: "1.0")
    }

    func call(_ request: Data) -> Data { Data() }

    func ingest(_ request: Data, image: Data?) -> Data {
        lastRequest = request
        lastImage = image
        return Data("""
        {"schema_version":"1.0","ok":true,"result":{"acknowledgement":"durable"}}
        """.utf8)
    }

    func imageRead(artifactID: String, generation: UInt64, maxBytes: UInt64) -> Data {
        Data()
    }

    func close() {}
}
