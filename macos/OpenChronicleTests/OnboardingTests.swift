import XCTest
@testable import OpenChronicle

@MainActor
final class OnboardingTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_784_016_000)

    func testFreshStoreStartsAtWelcomeAndIsIncomplete() {
        let store = MemoryOnboardingStore(state: .fresh(now: now))
        let model = makeModel(store: store)

        XCTAssertEqual(model.currentStep, .welcome)
        XCTAssertEqual(model.furthestStepIndex, 0)
        XCTAssertFalse(model.isComplete)
        XCTAssertNil(model.restoreIssue)
    }

    func testForwardAndBackwardTransitionsPersistAcrossModelRecreation() {
        let store = MemoryOnboardingStore(state: .fresh(now: now))
        let first = makeModel(store: store)
        XCTAssertTrue(first.advance())
        XCTAssertEqual(first.currentStep, .mode)
        XCTAssertTrue(first.advance())
        XCTAssertEqual(first.currentStep, .privacy)
        first.goBack()
        XCTAssertEqual(first.currentStep, .mode)

        let restored = makeModel(store: store)
        XCTAssertEqual(restored.currentStep, .mode)
        XCTAssertEqual(restored.furthestStepIndex, 2)
        restored.navigate(to: .privacy)
        XCTAssertEqual(restored.currentStep, .privacy)
    }

    func testSkipAheadAndOutOfOrderTransitionsAreRejectedWithoutMutation() {
        let store = MemoryOnboardingStore(state: .fresh(now: now))
        let model = makeModel(store: store)

        model.navigate(to: .permission)

        XCTAssertEqual(model.currentStep, .welcome)
        XCTAssertEqual(model.furthestStepIndex, 0)
    }

    func testUnknownSchemaAndCorruptPayloadFailClosed() throws {
        let suite = "OnboardingTests.\(UUID().uuidString)"
        guard let defaults = UserDefaults(suiteName: suite) else {
            return XCTFail("could not create isolated defaults")
        }
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = UserDefaultsOnboardingStateStore(defaults: defaults)

        defaults.set(Data("not-json".utf8), forKey: UserDefaultsOnboardingStateStore.stateKey)
        guard case .repairRequired = store.load(now: now) else {
            return XCTFail("corrupt progress must fail closed")
        }

        var unknown = OnboardingPersistedState.fresh(now: now)
        unknown.schemaVersion = 99
        defaults.set(
            try JSONEncoder().encode(unknown),
            forKey: UserDefaultsOnboardingStateStore.stateKey
        )
        let model = makeModel(store: store)
        XCTAssertNotNil(model.restoreIssue)
        XCTAssertFalse(model.canAdvance)

        model.restartSetupProgress()
        XCTAssertNil(model.restoreIssue)
        XCTAssertEqual(model.currentStep, .welcome)
        XCTAssertTrue(model.canAdvance)
    }

    func testLegacyBooleanCannotAuthorizeCaptureWithoutProofRecord() {
        let suite = "OnboardingTests.\(UUID().uuidString)"
        guard let defaults = UserDefaults(suiteName: suite) else {
            return XCTFail("could not create isolated defaults")
        }
        defer { defaults.removePersistentDomain(forName: suite) }
        defaults.set(true, forKey: UserDefaultsOnboardingStateStore.completionKey)

        XCTAssertFalse(AppRuntimeFactory.hasCompletedOnboarding(defaults: defaults))
        let model = makeModel(store: UserDefaultsOnboardingStateStore(defaults: defaults))
        XCTAssertNotNil(model.restoreIssue)
    }

    func testCompletionCannotBeEnteredWithoutLiveProof() {
        let permission = StubScreenRecordingPermission(granted: true)
        let model = makeModel(permission: permission)
        XCTAssertTrue(model.advance())
        XCTAssertTrue(model.advance())
        model.draft.privacyAcknowledged = true
        XCTAssertTrue(model.advance())
        XCTAssertTrue(model.advance())
        XCTAssertEqual(model.currentStep, .captureProof)
        XCTAssertFalse(model.advance())
        XCTAssertEqual(model.currentStep, .captureProof)
    }

    func testPermissionRevocationInvalidatesPersistedProofBeforeCompletion() async {
        let permission = StubScreenRecordingPermission(granted: true)
        let model = makeModel(permission: permission, proof: StubCaptureProof(result: .passed))
        moveToProof(model)
        await model.runCaptureProof()
        XCTAssertEqual(model.proofState, .passed)

        permission.granted = false
        model.refreshPermission()

        XCTAssertEqual(model.proofState, .notRun)
        XCTAssertEqual(model.proofFailure, .permissionDenied)
        XCTAssertFalse(model.canAdvance)
    }

    func testSuccessfulFinishCommitsConfigurationBeforeCompletion() async {
        let permission = StubScreenRecordingPermission(granted: true)
        var received: OnboardingRuntimeConfiguration?
        let model = makeModel(
            permission: permission,
            proof: StubCaptureProof(result: .passed),
            finish: { received = $0 }
        )
        moveToProof(model)
        await model.runCaptureProof()
        XCTAssertTrue(model.advance())
        XCTAssertTrue(model.advance())
        XCTAssertTrue(model.advance())
        XCTAssertEqual(model.currentStep, .completion)

        await model.finish()

        XCTAssertEqual(received, model.runtimeConfiguration)
        XCTAssertTrue(model.isComplete)
    }

    func testBacktrackingCannotBypassPrivacyAtFinish() async {
        let permission = StubScreenRecordingPermission(granted: true)
        var finishCalls = 0
        let model = makeModel(
            permission: permission,
            proof: StubCaptureProof(result: .passed),
            finish: { _ in finishCalls += 1 }
        )
        moveToProof(model)
        await model.runCaptureProof()
        XCTAssertTrue(model.advance())
        XCTAssertTrue(model.advance())
        XCTAssertTrue(model.advance())
        model.navigate(to: .privacy)
        model.draft.privacyAcknowledged = false
        model.navigate(to: .completion)

        XCTAssertFalse(model.canAdvance)
        await model.finish()
        XCTAssertEqual(finishCalls, 0)
        XCTAssertFalse(model.isComplete)
    }

    func testCompletedFixtureIsTerminalAndIdempotent() {
        var completed = OnboardingPersistedState.fresh(now: now)
        completed.currentStep = .completion
        completed.furthestStepIndex = OnboardingStep.allCases.count - 1
        completed.draft.privacyAcknowledged = true
        completed.proofState = .passed
        completed.isComplete = true
        let model = makeModel(store: MemoryOnboardingStore(state: completed))

        XCTAssertTrue(model.isComplete)
        XCTAssertFalse(model.canAdvance)
        XCTAssertFalse(model.advance())
        model.navigate(to: .welcome)
        XCTAssertEqual(model.currentStep, .completion)
    }

    func testCompletedStudyRemainsValidAfterItsEndBoundary() {
        var completed = OnboardingPersistedState.fresh(now: now)
        completed.currentStep = .completion
        completed.furthestStepIndex = OnboardingStep.allCases.count - 1
        completed.draft.recordingMode = .study
        completed.draft.studyStart = now.addingTimeInterval(-7_200)
        completed.draft.studyEnd = now.addingTimeInterval(-3_600)
        completed.draft.privacyAcknowledged = true
        completed.proofState = .passed
        completed.isComplete = true

        let model = makeModel(store: MemoryOnboardingStore(state: completed))

        XCTAssertNil(model.restoreIssue)
        XCTAssertTrue(model.isComplete)
    }

    func testInconsistentCompletedStateFailsClosed() {
        var corrupt = OnboardingPersistedState.fresh(now: now)
        corrupt.currentStep = .welcome
        corrupt.furthestStepIndex = 0
        corrupt.draft.privacyAcknowledged = true
        corrupt.proofState = .passed
        corrupt.isComplete = true

        let model = makeModel(store: MemoryOnboardingStore(state: corrupt))

        XCTAssertNotNil(model.restoreIssue)
        XCTAssertFalse(model.isComplete)
    }

    func testProgressPayloadContainsNoPermissionTruthPathsOrObservedContent() throws {
        let state = OnboardingPersistedState.fresh(now: now)
        let encoded = try JSONEncoder().encode(state)
        let text = try XCTUnwrap(String(data: encoded, encoding: .utf8))

        XCTAssertFalse(text.contains("permissionGranted"))
        XCTAssertFalse(text.contains("/Users/"))
        XCTAssertFalse(text.contains(CaptureProofTokenStore.fixedText))
        XCTAssertFalse(text.contains("windowTitle"))
    }

    func testProofWindowExposesOnlyTheExactExpectedOCRPhrase() {
        XCTAssertEqual(CaptureProofContent.ocrVisibleText, CaptureProofTokenStore.fixedText)
        XCTAssertFalse(CaptureProofContent.ocrVisibleText.contains("\n"))
        let authorization = CaptureProofAuthorization(
            windowID: 42,
            expectedTextDigest: CaptureProofAuthorization.digest(
                CaptureProofTokenStore.fixedText
            )
        )
        XCTAssertTrue(
            authorization.accepts(
                windowID: 42,
                recognizedText: CaptureProofContent.ocrVisibleText
            )
        )
        XCTAssertFalse(
            authorization.accepts(
                windowID: 42,
                recognizedText: CaptureProofContent.ocrVisibleText + "\nextra"
            )
        )
    }

    private func makeModel(
        store: (any OnboardingStateStoring)? = nil,
        permission: StubScreenRecordingPermission? = nil,
        proof: StubCaptureProof? = nil,
        finish: @escaping OnboardingModel.FinishHandler = { _ in }
    ) -> OnboardingModel {
        OnboardingModel(
            store: store ?? MemoryOnboardingStore(state: .fresh(now: now)),
            permission: permission ?? StubScreenRecordingPermission(granted: false),
            proofRunner: proof ?? StubCaptureProof(result: .failed(.captureFailed)),
            now: { self.now },
            finishHandler: finish
        )
    }

    private func moveToProof(_ model: OnboardingModel) {
        XCTAssertTrue(model.advance())
        XCTAssertTrue(model.advance())
        model.draft.privacyAcknowledged = true
        XCTAssertTrue(model.advance())
        XCTAssertTrue(model.advance())
        XCTAssertEqual(model.currentStep, .captureProof)
    }
}

final class OnboardingConfigurationTests: XCTestCase {
    func testAuthoritativeCoreRoundTripsCadenceAndScreenshotRetention() async throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let now = Date(timeIntervalSince1970: 1_784_016_000)
        let core = try InProcessCore(applicationSupportURL: directory, now: now)
        let configuration = OnboardingRuntimeConfiguration(
            recordingMode: .personal,
            cadenceSeconds: 30,
            screenshotRetentionSeconds: 7 * 24 * 60 * 60,
            studyStart: now,
            studyEnd: now.addingTimeInterval(30 * 24 * 60 * 60)
        )

        try await CoreOnboardingConfigurationService(core: core).apply(configuration, at: now)
        let state = try await CoreAppRuntimeController(
            core: core,
            deviceID: "onboarding-config-test",
            displayTimezone: "Europe/Zurich"
        ).runtimeConfiguration(at: now)

        XCTAssertTrue(state.recordingEnabled)
        XCTAssertEqual(state.cadenceSeconds, 30)
        XCTAssertEqual(state.screenshotRetentionSeconds, 7 * 24 * 60 * 60)
        try await core.close()
    }

    func testExpiredStudyIsRejectedBeforeCoreMutation() async throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let now = Date(timeIntervalSince1970: 1_784_016_000)
        let core = try InProcessCore(applicationSupportURL: directory, now: now)
        let configuration = OnboardingRuntimeConfiguration(
            recordingMode: .study,
            cadenceSeconds: 60,
            screenshotRetentionSeconds: 24 * 60 * 60,
            studyStart: now.addingTimeInterval(-7_200),
            studyEnd: now.addingTimeInterval(-3_600)
        )

        do {
            try await CoreOnboardingConfigurationService(core: core).apply(configuration, at: now)
            XCTFail("expired study must fail")
        } catch OnboardingConfigurationError.expiredStudy {
            // Expected.
        }
        let state = try await CoreAppRuntimeController(
            core: core,
            deviceID: "expired-study-test",
            displayTimezone: "Europe/Zurich"
        ).runtimeConfiguration(at: now)
        XCTAssertFalse(state.recordingEnabled)
        XCTAssertEqual(state.cadenceSeconds, 60)
        XCTAssertEqual(state.screenshotRetentionSeconds, 24 * 60 * 60)
        try await core.close()
    }
}

@MainActor
private final class MemoryOnboardingStore: OnboardingStateStoring {
    var result: OnboardingLoadResult

    init(state: OnboardingPersistedState) {
        result = .restored(state)
    }

    func load(now: Date) -> OnboardingLoadResult { result }

    func save(_ state: OnboardingPersistedState) {
        result = .restored(state)
    }
}

@MainActor
private final class StubScreenRecordingPermission: ScreenRecordingPermissionServicing {
    var granted: Bool

    init(granted: Bool) {
        self.granted = granted
    }

    func isGranted() -> Bool { granted }
    func request() -> Bool { granted }
    func openSystemSettings() {}
}

@MainActor
private final class StubCaptureProof: OnboardingCaptureProofRunning {
    let result: OnboardingCaptureProofResult

    init(result: OnboardingCaptureProofResult) {
        self.result = result
    }

    func run() async -> OnboardingCaptureProofResult { result }
}
