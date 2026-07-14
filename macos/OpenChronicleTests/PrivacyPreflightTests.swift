import XCTest
@testable import OpenChronicle

final class PrivacyPreflightTests: XCTestCase {
    private let evaluator = PrivacyEvaluator()

    func testSeededPasswordManagerIsDeniedWithoutPersistingIdentity() {
        let snapshot = snapshot(bundleID: "com.1password.1password")

        XCTAssertEqual(
            evaluator.evaluate(snapshot: snapshot, expectedIdentity: nil, proof: nil),
            .deny(.applicationExcluded)
        )
    }

    func testMailIsNotSilentlyExcludedByDefault() {
        let identity = identity(bundleID: "com.apple.mail")
        XCTAssertEqual(
            evaluator.evaluate(
                snapshot: snapshot(identity: identity),
                expectedIdentity: nil,
                proof: nil
            ),
            .allow(identity)
        )
    }

    func testPostCaptureIdentityChangeIsCoarseForegroundDenial() {
        let before = identity(windowID: 10)
        let after = identity(windowID: 11)

        XCTAssertEqual(
            evaluator.evaluate(
                snapshot: snapshot(identity: after),
                expectedIdentity: before,
                proof: nil
            ),
            .deny(.foregroundChanged)
        )
    }

    func testPermittedTitleChangeDoesNotChangeWindowIdentity() {
        let before = identity(title: "Before")
        let after = identity(title: "After")

        XCTAssertEqual(
            evaluator.evaluate(
                snapshot: snapshot(identity: after),
                expectedIdentity: before,
                proof: nil
            ),
            .allow(after)
        )
    }

    func testSecureInputWinsBeforeContentPolicy() {
        var value = snapshot(bundleID: "com.example.private", title: "secret")
        value.secureInputEnabled = true

        XCTAssertEqual(
            evaluator.evaluate(snapshot: value, expectedIdentity: nil, proof: nil),
            .deny(.secureInput)
        )
    }

    func testTitlePatternAndChronicleSelfAreDeniedCoarsely() {
        let titlePolicy = CapturePrivacyPolicy(
            policyVersion: "test-v1",
            excludedBundleIdentifiers: [],
            excludedTitleFragments: ["private vault"],
            chronicleBundleIdentifier: CapturePrivacyPolicy.default.chronicleBundleIdentifier
        )
        var titleSnapshot = snapshot(title: "Client Private Vault")
        titleSnapshot.policy = titlePolicy
        XCTAssertEqual(
            evaluator.evaluate(snapshot: titleSnapshot, expectedIdentity: nil, proof: nil),
            .deny(.titleExcluded)
        )

        let selfSnapshot = snapshot(
            bundleID: CapturePrivacyPolicy.default.chronicleBundleIdentifier
        )
        XCTAssertEqual(
            evaluator.evaluate(snapshot: selfSnapshot, expectedIdentity: nil, proof: nil),
            .deny(.chronicleSelf)
        )
    }

    func testPausedExpiredLockedAndAsleepUseCoarseOutcomes() {
        var paused = snapshot()
        paused.isPaused = true
        XCTAssertEqual(
            evaluator.evaluate(snapshot: paused, expectedIdentity: nil, proof: nil),
            .deny(.userPaused)
        )

        var expired = snapshot()
        expired.isStudyExpired = true
        XCTAssertEqual(
            evaluator.evaluate(snapshot: expired, expectedIdentity: nil, proof: nil),
            .deny(.studyExpired)
        )

        var locked = snapshot()
        locked.isLocked = true
        XCTAssertEqual(
            evaluator.evaluate(snapshot: locked, expectedIdentity: nil, proof: nil),
            .deny(.locked)
        )

        var asleep = snapshot()
        asleep.isAsleep = true
        XCTAssertEqual(
            evaluator.evaluate(snapshot: asleep, expectedIdentity: nil, proof: nil),
            .deny(.asleep)
        )
    }

    func testNoExactAndAmbiguousWindowNeverBecomeCaptureAuthorization() {
        var missing = snapshot()
        missing.window = .noExactWindow
        XCTAssertEqual(
            evaluator.evaluate(snapshot: missing, expectedIdentity: nil, proof: nil),
            .deny(.noExactWindow)
        )

        var ambiguous = snapshot()
        ambiguous.window = .ambiguousWindow
        XCTAssertEqual(
            evaluator.evaluate(snapshot: ambiguous, expectedIdentity: nil, proof: nil),
            .deny(.ambiguousWindow)
        )
    }

    private func identity(
        windowID: UInt32 = 10,
        bundleID: String = "com.example.editor",
        title: String? = "Notes"
    ) -> ActiveWindowIdentity {
        ActiveWindowIdentity(
            processID: 42,
            windowID: windowID,
            bundleIdentifier: bundleID,
            processName: "Editor",
            windowTitle: title
        )
    }

    private func snapshot(
        identity: ActiveWindowIdentity? = nil,
        bundleID: String = "com.example.editor",
        title: String? = "Notes"
    ) -> CapturePrivacySnapshot {
        CapturePrivacySnapshot(
            window: .exact(identity ?? self.identity(bundleID: bundleID, title: title)),
            isPaused: false,
            isStudyExpired: false,
            isLocked: false,
            isAsleep: false,
            hasScreenCapturePermission: true,
            secureInputEnabled: false,
            policy: .default
        )
    }
}
