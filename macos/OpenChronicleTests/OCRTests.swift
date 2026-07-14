import XCTest
@testable import OpenChronicle

final class OCRTests: XCTestCase {
    func testInjectedVisionResultsAreSortedAndCarryRequestedLanguageProvenance() async {
        let performer = StubVisionPerformer(result: VisionOCRRawResult(
            requestRevision: 3,
            observations: [
                observation("Bottom", x: 0.1, y: 0.1),
                observation("TopRight", x: 0.7, y: 0.8),
                observation("TopLeft", x: 0.1, y: 0.8),
            ]
        ))
        let service = VisionOCRService(
            configuration: VisionOCRConfiguration(
                automaticallyDetectsLanguage: false,
                recognitionLanguages: ["en-US", "de-DE"]
            ),
            performer: performer
        )

        let result = await service.recognize(normalizedFixture())
        guard case let .complete(text, confidence, provenance) = result else {
            return XCTFail("expected complete OCR")
        }
        XCTAssertEqual(text, "TopLeft\nTopRight\nBottom")
        XCTAssertEqual(confidence, 0.8, accuracy: 0.0001)
        XCTAssertEqual(provenance.engineAdapter, "apple-vision-vnrecognizetextrequest")
        XCTAssertTrue(provenance.engineVersion.contains("request-revision-3"))
        XCTAssertFalse(provenance.automaticLanguageDetection)
        XCTAssertEqual(provenance.recognitionLanguages, ["en-US", "de-DE"])
    }

    func testReadingOrderUsesTransitiveRowsForChainedNearYObservations() async {
        let service = VisionOCRService(performer: StubVisionPerformer(
            result: VisionOCRRawResult(
                requestRevision: 3,
                observations: [
                    observation("C", x: 0.1, y: 0.7988),
                    observation("B", x: 0.8, y: 0.7994),
                    observation("A", x: 0.2, y: 0.8000),
                ]
            )
        ))

        guard case let .complete(text, _, _) = await service.recognize(normalizedFixture()) else {
            return XCTFail("expected complete OCR")
        }
        XCTAssertEqual(text, "A\nB\nC")
    }

    func testMissingCandidateIsPartialOnlyWhenTextIsNonempty() async {
        let partial = VisionOCRService(performer: StubVisionPerformer(
            result: VisionOCRRawResult(
                requestRevision: 3,
                observations: [
                    observation("kept", x: 0, y: 1),
                    VisionOCRObservation(
                        text: nil,
                        confidence: nil,
                        boundingBox: .zero
                    ),
                ]
            )
        ))
        guard case let .partial(text, _, _) = await partial.recognize(normalizedFixture()) else {
            return XCTFail("expected partial OCR")
        }
        XCTAssertEqual(text, "kept")

        let empty = VisionOCRService(performer: StubVisionPerformer(
            result: VisionOCRRawResult(
                requestRevision: 3,
                observations: [
                    VisionOCRObservation(text: nil, confidence: nil, boundingBox: .zero),
                ]
            )
        ))
        guard case .empty = await empty.recognize(normalizedFixture()) else {
            return XCTFail("missing-only OCR must be empty, never partial-empty")
        }
    }

    func testUTF8OutputIsDeterministicallyBoundedAndMarkedPartial() async {
        let text = String(repeating: "é", count: 40_000)
        let service = VisionOCRService(performer: StubVisionPerformer(
            result: VisionOCRRawResult(
                requestRevision: 3,
                observations: [observation(text, x: 0, y: 1)],
            )
        ))

        guard case let .partial(output, _, _) = await service.recognize(normalizedFixture()) else {
            return XCTFail("bounded output must be partial")
        }
        XCTAssertLessThanOrEqual(output.utf8.count, 64 * 1_024)
        XCTAssertFalse(output.isEmpty)
    }

    func testVisionFailureRemainsCoarseFailedWithFactualAdapterProvenance() async {
        let service = VisionOCRService(performer: StubVisionPerformer(error: TestVisionError()))
        guard case let .failed(provenance) = await service.recognize(normalizedFixture()) else {
            return XCTFail("expected failed OCR")
        }
        XCTAssertEqual(provenance.engineAdapter, "apple-vision-vnrecognizetextrequest")
        XCTAssertTrue(provenance.engineVersion.contains("request-revision-3"))
    }

    func testNormalizationIsDeterministicAndBounded() throws {
        let source = testImage(width: 3_000, height: 1_000)
        let normalizer = DeterministicImageNormalizer()
        let first = try normalizer.normalize(source)
        let second = try normalizer.normalize(source)

        XCTAssertEqual(first.width, 2_560)
        XCTAssertLessThanOrEqual(first.width * first.height, 8_000_000)
        XCTAssertEqual(first.contentHash, second.contentHash)
        XCTAssertEqual(first.pixelBytes, second.pixelBytes)
    }

    func testHEICEncodingNeverExceedsFourMiB() throws {
        let normalized = try DeterministicImageNormalizer().normalize(testImage(width: 64, height: 64))
        let encoded = try BoundedHEICEncoder().encodeHEIC(normalized)
        XCTAssertLessThanOrEqual(encoded.data.count, BoundedHEICEncoder.maximumBytes)
        XCTAssertGreaterThan(encoded.width, 0)
        XCTAssertGreaterThan(encoded.height, 0)
    }

    private func normalizedFixture() -> NormalizedImage {
        let image = testImage()
        return NormalizedImage(
            cgImage: image,
            pixelBytes: Data(repeating: 0, count: 16),
            width: 2,
            height: 2,
            contentHash: "sha256-fixture"
        )
    }

    private func observation(
        _ text: String,
        x: CGFloat,
        y: CGFloat
    ) -> VisionOCRObservation {
        VisionOCRObservation(
            text: text,
            confidence: 0.8,
            boundingBox: CGRect(x: x, y: y, width: 0.1, height: 0.1)
        )
    }
}

private struct StubVisionPerformer: VisionRequestPerforming {
    let result: VisionOCRRawResult?
    let error: Error?

    init(result: VisionOCRRawResult) {
        self.result = result
        error = nil
    }

    init(error: Error) {
        result = nil
        self.error = error
    }

    func perform(
        image: CGImage,
        configuration: VisionOCRConfiguration
    ) throws -> VisionOCRRawResult {
        if let error { throw error }
        return result!
    }
}

private struct TestVisionError: Error {}
