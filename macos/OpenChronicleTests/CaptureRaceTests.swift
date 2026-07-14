import XCTest
@testable import OpenChronicle

final class CaptureRaceTests: XCTestCase {
    func testSleepInvalidationDuringCaptureDropsPixelsBeforeNormalization() async {
        let identity = testIdentity()
        let epoch = CaptureExecutionEpoch()
        let generation = await epoch.beginGeneration()
        let capturer = BlockingCapturer()
        let normalizer = TestNormalizer()
        let ocr = TestOCR()
        let encoder = TestEncoder()
        let ingestor = TestIngestor()
        let pipeline = testPipeline(
            provider: TestWindowProvider([
                .exact(.testFixture(identity: identity)),
                .exact(.testFixture(identity: identity)),
            ]),
            environment: TestEnvironment([allowedEnvironment]),
            capturer: capturer,
            normalizer: normalizer,
            ocr: ocr,
            encoder: encoder,
            ingestor: ingestor,
            validity: epoch
        )
        let context = testAttemptContext(
            token: "sleep-invalidation",
            executionGeneration: generation
        )

        let attempt = Task { await pipeline.attempt(context: context) }
        await capturer.waitUntilStarted()
        await epoch.invalidate(generation: generation, reason: .sleep)
        await capturer.release()
        let result = await attempt.value
        let ocrCalls = await ocr.calls
        let entries = await ingestor.entries

        XCTAssertEqual(result, .invalidated(.sleep))
        XCTAssertEqual(normalizer.calls, 0)
        XCTAssertEqual(ocrCalls, 0)
        XCTAssertEqual(encoder.calls, 0)
        XCTAssertTrue(entries.isEmpty)
    }

    func testPersistencePermitLinearizesBeforeLifecycleInvalidation() async {
        let identity = testIdentity()
        let epoch = CaptureExecutionEpoch()
        let generation = await epoch.beginGeneration()
        let ingestor = BlockingCaptureIngestor()
        let pipeline = testPipeline(
            provider: TestWindowProvider([
                .exact(.testFixture(identity: identity)),
                .exact(.testFixture(identity: identity)),
            ]),
            environment: TestEnvironment([allowedEnvironment]),
            capturer: TestCapturer(),
            ingestor: ingestor,
            validity: epoch
        )
        let context = testAttemptContext(
            token: "linearized-persistence",
            executionGeneration: generation
        )
        let probe = CompletionProbe()

        let attempt = Task { await pipeline.attempt(context: context) }
        await ingestor.waitUntilStarted()
        let invalidation = Task {
            await epoch.invalidate(generation: generation, reason: .sleep)
            await probe.markComplete()
        }
        try? await Task.sleep(nanoseconds: 10_000_000)
        let completedBeforeRelease = await probe.isComplete
        XCTAssertFalse(completedBeforeRelease)

        await ingestor.release()
        let result = await attempt.value
        await invalidation.value

        guard case .stored = result else {
            return XCTFail("the permit-linearized transaction should finish")
        }
        let completedAfterRelease = await probe.isComplete
        let ingestCalls = await ingestor.calls
        XCTAssertTrue(completedAfterRelease)
        XCTAssertEqual(ingestCalls, 1)
    }

    func testInvalidationAfterPixelsButBeforePermitPreventsPersistence() async {
        let identity = testIdentity()
        let epoch = CaptureExecutionEpoch()
        let generation = await epoch.beginGeneration()
        let ocr = BlockingOCR()
        let ingestor = TestIngestor()
        let pipeline = testPipeline(
            provider: TestWindowProvider([
                .exact(.testFixture(identity: identity)),
                .exact(.testFixture(identity: identity)),
            ]),
            environment: TestEnvironment([allowedEnvironment]),
            capturer: TestCapturer(),
            ocr: ocr,
            ingestor: ingestor,
            validity: epoch
        )
        let context = testAttemptContext(
            token: "pre-permit-invalidation",
            executionGeneration: generation
        )

        let attempt = Task { await pipeline.attempt(context: context) }
        await ocr.waitUntilStarted()
        await epoch.invalidate(generation: generation, reason: .sleep)
        await ocr.release()
        let result = await attempt.value

        XCTAssertEqual(result, .invalidated(.sleep))
        let entries = await ingestor.entries
        XCTAssertTrue(entries.isEmpty)
    }

    func testSecureInputTransitionDropsPixelsBeforeAllPixelConsumers() async {
        let identity = testIdentity()
        let provider = TestWindowProvider([
            .exact(.testFixture(identity: identity)),
        ])
        var secured = allowedEnvironment
        secured = CaptureEnvironmentState(
            isPaused: false,
            isStudyExpired: false,
            isLocked: false,
            isAsleep: false,
            hasScreenCapturePermission: true,
            secureInputEnabled: true
        )
        let environment = TestEnvironment([allowedEnvironment, allowedEnvironment, secured])
        let capturer = TestCapturer()
        let normalizer = TestNormalizer()
        let ocr = TestOCR()
        let encoder = TestEncoder()
        let ingestor = TestIngestor()
        let pipeline = testPipeline(
            provider: provider,
            environment: environment,
            capturer: capturer,
            normalizer: normalizer,
            ocr: ocr,
            encoder: encoder,
            ingestor: ingestor
        )

        let result = await pipeline.attempt(context: testAttemptContext(token: "secure-post"))
        XCTAssertEqual(result, .denied(.secureInput))
        let providerCalls = await provider.calls
        let captureCalls = await capturer.calls
        let ocrCalls = await ocr.calls
        let entries = await ingestor.entries
        XCTAssertEqual(providerCalls, 1, "post environment denial must skip lookup")
        XCTAssertEqual(captureCalls, 1)
        XCTAssertEqual(normalizer.calls, 0)
        XCTAssertEqual(ocrCalls, 0)
        XCTAssertEqual(encoder.calls, 0)
        XCTAssertEqual(entries.count, 1)
        XCTAssertNil(entries[0].image)
        guard case let .denied(reason, _) = entries[0].record else {
            return XCTFail("expected coarse denial")
        }
        XCTAssertEqual(reason, .secureInput)
    }

    func testSecureInputTransitionDuringPreflightLookupPreventsCapture() async {
        let identity = testIdentity()
        let secured = CaptureEnvironmentState(
            isPaused: false,
            isStudyExpired: false,
            isLocked: false,
            isAsleep: false,
            hasScreenCapturePermission: true,
            secureInputEnabled: true
        )
        let provider = TestWindowProvider([
            .exact(.testFixture(identity: identity)),
        ])
        let capturer = TestCapturer()
        let normalizer = TestNormalizer()
        let ingestor = TestIngestor()
        let pipeline = testPipeline(
            provider: provider,
            environment: TestEnvironment([allowedEnvironment, secured]),
            capturer: capturer,
            normalizer: normalizer,
            ingestor: ingestor
        )

        let result = await pipeline.attempt(context: testAttemptContext(token: "secure-pre"))
        let providerCalls = await provider.calls
        let captureCalls = await capturer.calls
        XCTAssertEqual(result, .denied(.secureInput))
        XCTAssertEqual(providerCalls, 1)
        XCTAssertEqual(captureCalls, 0)
        XCTAssertEqual(normalizer.calls, 0)
        let entries = await ingestor.entries
        XCTAssertEqual(entries.count, 1)
        XCTAssertNil(entries[0].image)
    }

    func testSecureInputTransitionDuringPostflightLookupDropsPixelsBeforeHashing() async {
        let identity = testIdentity()
        let secured = CaptureEnvironmentState(
            isPaused: false,
            isStudyExpired: false,
            isLocked: false,
            isAsleep: false,
            hasScreenCapturePermission: true,
            secureInputEnabled: true
        )
        let provider = TestWindowProvider([
            .exact(.testFixture(identity: identity)),
            .exact(.testFixture(identity: identity)),
        ])
        let capturer = TestCapturer()
        let normalizer = TestNormalizer()
        let ocr = TestOCR()
        let encoder = TestEncoder()
        let ingestor = TestIngestor()
        let pipeline = testPipeline(
            provider: provider,
            environment: TestEnvironment([
                allowedEnvironment,
                allowedEnvironment,
                allowedEnvironment,
                secured,
            ]),
            capturer: capturer,
            normalizer: normalizer,
            ocr: ocr,
            encoder: encoder,
            ingestor: ingestor
        )

        let result = await pipeline.attempt(context: testAttemptContext(token: "secure-lookup"))
        let providerCalls = await provider.calls
        let captureCalls = await capturer.calls
        let ocrCalls = await ocr.calls
        XCTAssertEqual(result, .denied(.secureInput))
        XCTAssertEqual(providerCalls, 2)
        XCTAssertEqual(captureCalls, 1)
        XCTAssertEqual(normalizer.calls, 0)
        XCTAssertEqual(ocrCalls, 0)
        XCTAssertEqual(encoder.calls, 0)
        let entries = await ingestor.entries
        XCTAssertEqual(entries.count, 1)
        XCTAssertNil(entries[0].image)
    }

    func testPreflightEnvironmentDenialDoesNotResolveAWindow() async {
        var denied = allowedEnvironment
        denied = CaptureEnvironmentState(
            isPaused: false,
            isStudyExpired: false,
            isLocked: false,
            isAsleep: false,
            hasScreenCapturePermission: false,
            secureInputEnabled: false
        )
        let provider = TestWindowProvider([])
        let capturer = TestCapturer()
        let ingestor = TestIngestor()
        let pipeline = testPipeline(
            provider: provider,
            environment: TestEnvironment([denied]),
            capturer: capturer,
            ingestor: ingestor
        )

        let result = await pipeline.attempt(context: testAttemptContext(token: "permission-pre"))
        let providerCalls = await provider.calls
        let captureCalls = await capturer.calls
        XCTAssertEqual(result, .denied(.permissionDenied))
        XCTAssertEqual(providerCalls, 0)
        XCTAssertEqual(captureCalls, 0)
    }

    func testForegroundSwitchAfterCapturePersistsOnlyCoarseReason() async {
        let provider = TestWindowProvider([
            .exact(.testFixture(identity: testIdentity(windowID: 10, title: "Secret A"))),
            .exact(.testFixture(identity: testIdentity(windowID: 11, title: "Secret B"))),
        ])
        let normalizer = TestNormalizer()
        let ingestor = TestIngestor()
        let pipeline = testPipeline(
            provider: provider,
            environment: TestEnvironment([allowedEnvironment]),
            capturer: TestCapturer(),
            normalizer: normalizer,
            ingestor: ingestor
        )

        let result = await pipeline.attempt(context: testAttemptContext(token: "foreground"))
        XCTAssertEqual(result, .denied(.foregroundChanged))
        XCTAssertEqual(normalizer.calls, 0)
        let entries = await ingestor.entries
        XCTAssertEqual(entries.count, 1)
        XCTAssertNil(entries[0].image)
        guard case let .denied(reason, _) = entries[0].record else {
            return XCTFail("expected denied record")
        }
        XCTAssertEqual(reason, .foregroundChanged)
    }

    func testCaptureFailureRechecksRevokedPermission() async {
        let identity = testIdentity()
        let provider = TestWindowProvider([.exact(.testFixture(identity: identity))])
        let permissionLost = CaptureEnvironmentState(
            isPaused: false,
            isStudyExpired: false,
            isLocked: false,
            isAsleep: false,
            hasScreenCapturePermission: false,
            secureInputEnabled: false
        )
        let capturer = TestCapturer()
        await capturer.setShouldFail(true)
        let ingestor = TestIngestor()
        let pipeline = testPipeline(
            provider: provider,
            environment: TestEnvironment([
                allowedEnvironment,
                allowedEnvironment,
                permissionLost,
            ]),
            capturer: capturer,
            ingestor: ingestor
        )

        let result = await pipeline.attempt(context: testAttemptContext(token: "permission-loss"))
        XCTAssertEqual(result, .denied(.permissionDenied))
        let entries = await ingestor.entries
        guard case let .denied(reason, _) = entries.first?.record else {
            return XCTFail("expected permission denial")
        }
        XCTAssertEqual(reason, .permissionDenied)
    }

    func testCaptureAPIFailureWithPermissionStillGrantedIsFactualFailure() async {
        let identity = testIdentity()
        let capturer = TestCapturer()
        await capturer.setShouldFail(true)
        let ingestor = TestIngestor()
        let pipeline = testPipeline(
            provider: TestWindowProvider([.exact(.testFixture(identity: identity))]),
            environment: TestEnvironment([allowedEnvironment]),
            capturer: capturer,
            ingestor: ingestor
        )

        let result = await pipeline.attempt(context: testAttemptContext(token: "capture-failure"))
        guard case .stored = result else {
            return XCTFail("coarse capture failure was not durably stored")
        }
        let entries = await ingestor.entries
        XCTAssertEqual(entries.count, 1)
        XCTAssertNil(entries[0].image)
        guard case .captureFailed = entries[0].record else {
            return XCTFail("expected capture-api-failure record")
        }
    }

    func testProofTokenIsExactMemoryOnlyAndSingleUse() async {
        let identity = testIdentity(
            windowID: 77,
            bundleID: CapturePrivacyPolicy.default.chronicleBundleIdentifier,
            title: "Capture proof"
        )
        let provider = TestWindowProvider([
            .exact(.testFixture(identity: identity)),
            .exact(.testFixture(identity: identity)),
            .exact(.testFixture(identity: identity)),
        ])
        let tokens = CaptureProofTokenStore()
        let token = await tokens.mint(forTestWindowID: identity.windowID)
        let capturer = TestCapturer()
        let ocr = TestOCR(text: CaptureProofTokenStore.fixedText)
        let ingestor = TestIngestor()
        let pipeline = testPipeline(
            provider: provider,
            environment: TestEnvironment([allowedEnvironment]),
            capturer: capturer,
            ocr: ocr,
            proofTokens: tokens,
            ingestor: ingestor
        )

        let first = await pipeline.attempt(
            context: testAttemptContext(token: "proof-first"),
            proofToken: token
        )
        let initialEntries = await ingestor.entries
        let second = await pipeline.attempt(
            context: testAttemptContext(token: "proof-second"),
            proofToken: token
        )
        let captureCalls = await capturer.calls
        XCTAssertEqual(first, .proofSucceeded)
        XCTAssertTrue(initialEntries.isEmpty)
        XCTAssertEqual(second, .denied(.chronicleSelf))
        XCTAssertEqual(captureCalls, 1, "reused token must not capture")
        let entries = await ingestor.entries
        XCTAssertEqual(entries.count, 1)
        XCTAssertNil(entries[0].image)
    }

    func testInvalidProofTextPersistsOnlyCoarseSelfDenial() async {
        let identity = testIdentity(
            windowID: 77,
            bundleID: CapturePrivacyPolicy.default.chronicleBundleIdentifier
        )
        let tokens = CaptureProofTokenStore()
        let token = await tokens.mint(forTestWindowID: identity.windowID)
        let encoder = TestEncoder()
        let ingestor = TestIngestor()
        let pipeline = testPipeline(
            provider: TestWindowProvider([
                .exact(.testFixture(identity: identity)),
                .exact(.testFixture(identity: identity)),
            ]),
            environment: TestEnvironment([allowedEnvironment]),
            capturer: TestCapturer(),
            ocr: TestOCR(text: "wrong text"),
            encoder: encoder,
            proofTokens: tokens,
            ingestor: ingestor
        )

        let result = await pipeline.attempt(
            context: testAttemptContext(token: "proof-text"),
            proofToken: token
        )
        XCTAssertEqual(result, .denied(.chronicleSelf))
        XCTAssertEqual(encoder.calls, 0)
        let entries = await ingestor.entries
        XCTAssertEqual(entries.count, 1)
        XCTAssertNil(entries[0].image)
        guard case let .denied(reason, _) = entries[0].record else {
            return XCTFail("expected coarse denial")
        }
        XCTAssertEqual(reason, .chronicleSelf)
    }

    func testInvalidProofNeverFallsThroughToNormalNonChronicleCapture() async {
        let identity = testIdentity(windowID: 10, bundleID: "com.example.editor")
        let tokens = CaptureProofTokenStore()
        let token = await tokens.mint(forTestWindowID: 77)
        let capturer = TestCapturer()
        let normalizer = TestNormalizer()
        let ocr = TestOCR()
        let encoder = TestEncoder()
        let ingestor = TestIngestor()
        let pipeline = testPipeline(
            provider: TestWindowProvider([
                .exact(.testFixture(identity: identity)),
            ]),
            environment: TestEnvironment([allowedEnvironment]),
            capturer: capturer,
            normalizer: normalizer,
            ocr: ocr,
            encoder: encoder,
            proofTokens: tokens,
            ingestor: ingestor
        )

        let result = await pipeline.attempt(
            context: testAttemptContext(token: "proof-scope"),
            proofToken: token
        )
        let captureCalls = await capturer.calls
        let ocrCalls = await ocr.calls
        XCTAssertEqual(result, .denied(.chronicleSelf))
        XCTAssertEqual(captureCalls, 0)
        XCTAssertEqual(normalizer.calls, 0)
        XCTAssertEqual(ocrCalls, 0)
        XCTAssertEqual(encoder.calls, 0)
        let entries = await ingestor.entries
        XCTAssertEqual(entries.count, 1)
        XCTAssertNil(entries[0].image)
    }
}

private actor BlockingCapturer: ExactWindowCapturing {
    private var started = false
    private var continuation: CheckedContinuation<Void, Never>?

    func capture(_ window: ResolvedActiveWindow) async -> CapturedWindowImage {
        started = true
        await withCheckedContinuation { continuation = $0 }
        return CapturedWindowImage(image: testImage(), scaleMilli: 2_000)
    }

    func waitUntilStarted() async {
        while !started { await Task.yield() }
    }

    func release() {
        continuation?.resume()
        continuation = nil
    }
}

private actor BlockingCaptureIngestor: CaptureIngesting {
    private var started = false
    private var continuation: CheckedContinuation<Void, Never>?
    private(set) var calls = 0

    func ingest(
        record: CaptureIngestRecord,
        image: Data?,
        context: CaptureAttemptContext,
        observedAt: Date,
        permit: CapturePersistencePermit
    ) async -> CaptureIngestAcknowledgement {
        calls += 1
        started = true
        await withCheckedContinuation { continuation = $0 }
        return CaptureIngestAcknowledgement(
            durability: .durable,
            eventID: context.eventID,
            ocrEventID: context.eventID,
            imageArtifactID: context.imageArtifactID
        )
    }

    func waitUntilStarted() async {
        while !started { await Task.yield() }
    }

    func release() {
        continuation?.resume()
        continuation = nil
    }
}

private actor CompletionProbe {
    private(set) var isComplete = false
    func markComplete() { isComplete = true }
}

private actor BlockingOCR: OCRRecognizing {
    private var started = false
    private var continuation: CheckedContinuation<Void, Never>?

    func recognize(_ image: NormalizedImage) async -> OCRRecognition {
        started = true
        await withCheckedContinuation { continuation = $0 }
        return .complete(
            text: "Synthetic text",
            confidence: 0.9,
            provenance: OCRProvenance(
                engineAdapter: "blocking-test",
                engineVersion: "1",
                automaticLanguageDetection: true,
                recognitionLanguages: []
            )
        )
    }

    func waitUntilStarted() async {
        while !started { await Task.yield() }
    }

    func release() {
        continuation?.resume()
        continuation = nil
    }
}
