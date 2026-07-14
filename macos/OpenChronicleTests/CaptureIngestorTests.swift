import Foundation
import XCTest
@testable import OpenChronicle

final class CaptureIngestorTests: XCTestCase {
    func testFactoryRoundTripsChangedImageAndLifecycleThroughRealRustCore() async throws {
        let temporary = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: temporary, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: temporary) }
        let instant = Date(timeIntervalSince1970: 1_784_016_000)
        let core = try InProcessCore(applicationSupportURL: temporary, now: instant)
        let source = FixedMetadataSource(metadata(instant: instant))
        let ingestor = CoreCaptureIngestor(core: core, metadata: source)

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
            image: Data([1, 2, 3])
        )

        XCTAssertTrue(acknowledgement.durability.isCanonicalDurable)
        XCTAssertEqual(acknowledgement.eventID, "event-u7-1")
        XCTAssertEqual(acknowledgement.ocrEventID, "event-u7-1")
        XCTAssertEqual(acknowledgement.imageArtifactID, "image-u7-1")
        try await core.close()
    }

    func testFailedOCRFactoryEmitsNoCanonicalOCRPayload() async throws {
        let core = SpyCoreService()
        let instant = Date(timeIntervalSince1970: 1_784_016_000)
        let ingestor = CoreCaptureIngestor(
            core: core,
            metadata: FixedMetadataSource(metadata(instant: instant))
        )

        _ = try await ingestor.ingest(
            record: changedRecord(
                ocr: .failed(provenance: provenance),
                dimensions: nil
            ),
            image: nil
        )

        let capturedRequest = await core.lastRequest
        let request = try XCTUnwrap(capturedRequest)
        let json = try JSONSerialization.jsonObject(with: request) as! [String: Any]
        let event = json["event"] as! [String: Any]
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
            metadata: FixedMetadataSource(metadata(instant: instant))
        )

        _ = try await ingestor.ingest(
            record: .denied(reason: .applicationExcluded, presence: .active),
            image: nil
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
                metadata: FixedMetadataSource(metadata(instant: instant))
            )
            _ = try await ingestor.ingest(
                record: .denied(reason: reason, presence: .active),
                image: nil
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
                metadata: FixedMetadataSource(metadata(instant: instant))
            )
            _ = try await ingestor.ingest(
                record: .denied(reason: reason, presence: .unknown),
                image: nil
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

    private func metadata(instant: Date) -> CaptureEventMetadata {
        CaptureEventMetadata(
            eventID: "event-u7-1",
            lifecycleEventID: "lifecycle-u7-1",
            imageArtifactID: "image-u7-1",
            deviceID: "device-u7",
            scheduledAt: instant,
            observedAt: instant,
            recordedAt: instant,
            displayTimezone: "Europe/Zurich",
            sourceVersion: "test-1",
            cadenceSeconds: 30,
            bootSequence: "boot-u7",
            monotonicTick: 1,
            screenshotExpiresAt: instant.addingTimeInterval(86_400)
        )
    }
}

private actor FixedMetadataSource: CaptureEventMetadataProviding {
    let value: CaptureEventMetadata
    init(_ value: CaptureEventMetadata) { self.value = value }
    func nextMetadata() -> CaptureEventMetadata { value }
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
