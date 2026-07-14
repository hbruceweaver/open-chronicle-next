import CoreGraphics
import Foundation
@testable import OpenChronicle

func testImage(width: Int = 2, height: Int = 2) -> CGImage {
    let bytesPerRow = width * 4
    let data = Data(repeating: 127, count: bytesPerRow * height)
    let provider = CGDataProvider(data: data as CFData)!
    return CGImage(
        width: width,
        height: height,
        bitsPerComponent: 8,
        bitsPerPixel: 32,
        bytesPerRow: bytesPerRow,
        space: CGColorSpace(name: CGColorSpace.sRGB)!,
        bitmapInfo: [.byteOrder32Little, CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedFirst.rawValue)],
        provider: provider,
        decode: nil,
        shouldInterpolate: false,
        intent: .defaultIntent
    )!
}

func testIdentity(
    processID: pid_t = 42,
    windowID: UInt32 = 10,
    bundleID: String = "com.example.editor",
    title: String? = "Notes"
) -> ActiveWindowIdentity {
    ActiveWindowIdentity(
        processID: processID,
        windowID: windowID,
        bundleIdentifier: bundleID,
        processName: "Editor",
        windowTitle: title
    )
}

let allowedEnvironment = CaptureEnvironmentState(
    isPaused: false,
    isStudyExpired: false,
    isLocked: false,
    isAsleep: false,
    hasScreenCapturePermission: true,
    secureInputEnabled: false
)

actor TestWindowProvider: ActiveWindowProviding {
    private var values: [ActiveWindowLookup]
    private(set) var calls = 0

    init(_ values: [ActiveWindowLookup]) {
        self.values = values
    }

    func resolveActiveWindow() throws -> ActiveWindowLookup {
        calls += 1
        guard !values.isEmpty else { return .noExactWindow }
        if values.count == 1 { return values[0] }
        return values.removeFirst()
    }
}

actor TestEnvironment: CaptureEnvironmentProviding {
    private var values: [CaptureEnvironmentState]
    private(set) var calls = 0

    init(_ values: [CaptureEnvironmentState]) {
        self.values = values
    }

    func currentEnvironment() -> CaptureEnvironmentState {
        calls += 1
        guard !values.isEmpty else { return allowedEnvironment }
        if values.count == 1 { return values[0] }
        return values.removeFirst()
    }
}

enum TestCaptureError: Error { case failed }

actor TestCapturer: ExactWindowCapturing {
    private(set) var calls = 0
    var shouldFail = false
    let result: CapturedWindowImage

    init(scaleMilli: UInt32 = 2_000) {
        result = CapturedWindowImage(image: testImage(), scaleMilli: scaleMilli)
    }

    func capture(_ window: ResolvedActiveWindow) throws -> CapturedWindowImage {
        calls += 1
        if shouldFail { throw TestCaptureError.failed }
        return result
    }

    func setShouldFail(_ value: Bool) {
        shouldFail = value
    }
}

final class TestNormalizer: ImageNormalizing, @unchecked Sendable {
    private let lock = NSLock()
    private(set) var calls = 0
    let hash: String

    init(hash: String = "sha256-test") {
        self.hash = hash
    }

    func normalize(_ image: CGImage) throws -> NormalizedImage {
        lock.lock()
        calls += 1
        lock.unlock()
        return NormalizedImage(
            cgImage: image,
            pixelBytes: Data(repeating: 1, count: image.width * image.height * 4),
            width: image.width,
            height: image.height,
            contentHash: hash
        )
    }
}

actor TestOCR: OCRRecognizing {
    private(set) var calls = 0
    var result: OCRRecognition

    init(text: String = "Synthetic text") {
        result = .complete(
            text: text,
            confidence: 0.9,
            provenance: OCRProvenance(
                engineAdapter: "test-vision",
                engineVersion: "1",
                automaticLanguageDetection: true,
                recognitionLanguages: []
            )
        )
    }

    func recognize(_ image: NormalizedImage) -> OCRRecognition {
        calls += 1
        return result
    }
}

final class TestEncoder: ImageEncoding, @unchecked Sendable {
    private let lock = NSLock()
    private(set) var calls = 0

    func encodeHEIC(_ image: NormalizedImage) throws -> EncodedHEICImage {
        lock.lock()
        calls += 1
        lock.unlock()
        return EncodedHEICImage(
            data: Data([1, 2, 3]),
            width: UInt32(image.width),
            height: UInt32(image.height)
        )
    }
}

actor TestIngestor: CaptureIngesting {
    struct Entry: Equatable {
        let record: CaptureIngestRecord
        let image: Data?
    }

    private(set) var entries: [Entry] = []
    var acknowledgements: [CaptureIngestAcknowledgement]

    init(_ acknowledgements: [CaptureIngestAcknowledgement] = []) {
        self.acknowledgements = acknowledgements
    }

    func ingest(
        record: CaptureIngestRecord,
        image: Data?
    ) -> CaptureIngestAcknowledgement {
        entries.append(Entry(record: record, image: image))
        if acknowledgements.isEmpty {
            return CaptureIngestAcknowledgement(
                durability: .durable,
                eventID: "event-\(entries.count)",
                ocrEventID: "event-\(entries.count)",
                imageArtifactID: "image-1"
            )
        }
        return acknowledgements.removeFirst()
    }
}

struct FixedIdleReader: IdleSecondsReading {
    let value: TimeInterval?
    func idleSeconds() -> TimeInterval? { value }
}

func testPipeline(
    provider: TestWindowProvider,
    environment: TestEnvironment,
    capturer: TestCapturer,
    normalizer: TestNormalizer = TestNormalizer(),
    deduplicator: ContentDeduplicator = ContentDeduplicator(),
    ocr: TestOCR = TestOCR(),
    encoder: TestEncoder = TestEncoder(),
    proofTokens: CaptureProofTokenStore = CaptureProofTokenStore(),
    ingestor: TestIngestor
) -> CaptureAttemptPipeline {
    CaptureAttemptPipeline(
        windowProvider: provider,
        environment: environment,
        capturer: capturer,
        normalizer: normalizer,
        deduplicator: deduplicator,
        recognizer: ocr,
        encoder: encoder,
        idleState: IdleStateSource(reader: FixedIdleReader(value: 0)),
        proofTokens: proofTokens,
        ingestor: ingestor
    )
}
