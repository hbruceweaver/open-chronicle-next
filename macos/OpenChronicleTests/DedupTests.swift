import XCTest
@testable import OpenChronicle

final class DedupTests: XCTestCase {
    func testUnchangedContentSkipsVisionAndHEICAndAdvancesLatestEvent() async {
        let identity = testIdentity()
        let provider = TestWindowProvider([
            .exact(.testFixture(identity: identity)),
            .exact(.testFixture(identity: identity)),
            .exact(.testFixture(identity: identity)),
            .exact(.testFixture(identity: identity)),
        ])
        let normalizer = TestNormalizer(hash: "sha256-same")
        let ocr = TestOCR()
        let encoder = TestEncoder()
        let deduplicator = ContentDeduplicator()
        let ingestor = TestIngestor([
            acknowledgement(.durable, event: "event-1", ocr: "event-1", image: "image-1"),
            acknowledgement(.durable, event: "event-2", ocr: nil, image: nil),
        ])
        let pipeline = testPipeline(
            provider: provider,
            environment: TestEnvironment([allowedEnvironment]),
            capturer: TestCapturer(),
            normalizer: normalizer,
            deduplicator: deduplicator,
            ocr: ocr,
            encoder: encoder,
            ingestor: ingestor
        )

        _ = await pipeline.attempt(context: testAttemptContext(token: "dedup-1"))
        _ = await pipeline.attempt(context: testAttemptContext(token: "dedup-2"))

        let ocrCalls = await ocr.calls
        XCTAssertEqual(ocrCalls, 1)
        XCTAssertEqual(encoder.calls, 1)
        let entries = await ingestor.entries
        XCTAssertEqual(entries.count, 2)
        guard case .unchanged = entries[1].record else {
            return XCTFail("second identical capture was not unchanged")
        }
        let latest = await deduplicator.latest()
        XCTAssertEqual(latest?.eventID, "event-2")
        XCTAssertEqual(latest?.ocrEventID, "event-1")
        XCTAssertEqual(latest?.imageArtifactID, "image-1")
    }

    func testNotDurableIngestDoesNotAdvanceDedupe() async {
        let identity = testIdentity()
        let provider = TestWindowProvider([
            .exact(.testFixture(identity: identity)),
            .exact(.testFixture(identity: identity)),
            .exact(.testFixture(identity: identity)),
            .exact(.testFixture(identity: identity)),
        ])
        let ocr = TestOCR()
        let encoder = TestEncoder()
        let deduplicator = ContentDeduplicator()
        let ingestor = TestIngestor([
            acknowledgement(.notDurable, event: nil, ocr: nil, image: nil),
            acknowledgement(.durable, event: "event-2", ocr: "event-2", image: "image-2"),
        ])
        let pipeline = testPipeline(
            provider: provider,
            environment: TestEnvironment([allowedEnvironment]),
            capturer: TestCapturer(),
            deduplicator: deduplicator,
            ocr: ocr,
            encoder: encoder,
            ingestor: ingestor
        )

        let first = await pipeline.attempt(context: testAttemptContext(token: "retry-1"))
        XCTAssertEqual(first, .persistenceFailed(.unknown))
        _ = await pipeline.attempt(context: testAttemptContext(token: "retry-2"))

        let ocrCalls = await ocr.calls
        XCTAssertEqual(ocrCalls, 2)
        XCTAssertEqual(encoder.calls, 2)
        let entries = await ingestor.entries
        guard case .changed = entries[1].record else {
            return XCTFail("non-durable ingest incorrectly advanced dedupe")
        }
    }

    func testProjectionPendingIsCanonicalDurableForDedupe() async {
        let identity = testIdentity()
        let provider = TestWindowProvider([
            .exact(.testFixture(identity: identity)),
            .exact(.testFixture(identity: identity)),
            .exact(.testFixture(identity: identity)),
            .exact(.testFixture(identity: identity)),
        ])
        let ocr = TestOCR()
        let ingestor = TestIngestor([
            acknowledgement(
                .journalDurableProjectionPending,
                event: "event-1",
                ocr: "event-1",
                image: "image-1"
            ),
            acknowledgement(.durable, event: "event-2", ocr: nil, image: nil),
        ])
        let pipeline = testPipeline(
            provider: provider,
            environment: TestEnvironment([allowedEnvironment]),
            capturer: TestCapturer(),
            deduplicator: ContentDeduplicator(),
            ocr: ocr,
            ingestor: ingestor
        )

        _ = await pipeline.attempt(context: testAttemptContext(token: "change-1"))
        _ = await pipeline.attempt(context: testAttemptContext(token: "change-2"))

        let ocrCalls = await ocr.calls
        XCTAssertEqual(ocrCalls, 1)
        let entries = await ingestor.entries
        guard case .unchanged = entries[1].record else {
            return XCTFail("projection-pending canonical event did not dedupe")
        }
    }

    private func acknowledgement(
        _ durability: CaptureDurability,
        event: String?,
        ocr: String?,
        image: String?
    ) -> CaptureIngestAcknowledgement {
        CaptureIngestAcknowledgement(
            durability: durability,
            eventID: event,
            ocrEventID: ocr,
            imageArtifactID: image
        )
    }
}
