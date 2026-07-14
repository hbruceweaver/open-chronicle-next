import Carbon
import CoreGraphics
import CryptoKit
import Foundation

enum CaptureDenial: String, Codable, Equatable, Sendable {
    case userPaused = "user-paused"
    case studyExpired = "study-expired"
    case permissionDenied = "permission-denied"
    case locked
    case asleep
    case noExactWindow = "no-exact-window"
    case ambiguousWindow = "ambiguous-window"
    case secureInput = "secure-input"
    case applicationExcluded = "application-excluded"
    case titleExcluded = "title-excluded"
    case chronicleSelf = "chronicle-self"
    case foregroundChanged = "foreground-changed"
}

struct CapturePrivacyPolicy: Equatable, Sendable {
    let policyVersion: String
    let excludedBundleIdentifiers: Set<String>
    let excludedTitleFragments: [String]
    let chronicleBundleIdentifier: String

    static let `default` = CapturePrivacyPolicy(
        policyVersion: "capture-privacy-v1",
        excludedBundleIdentifiers: [
            "com.1password.1password",
            "com.agilebits.onepassword7",
            "com.bitwarden.desktop",
            "com.dashlane.Dashlane",
            "com.lastpass.LastPass",
            "com.apple.Passwords",
            "com.apple.keychainaccess",
            "com.apple.SecurityAgent",
        ],
        excludedTitleFragments: [],
        chronicleBundleIdentifier: Bundle.main.bundleIdentifier
            ?? "com.screenata.openchronicle"
    )
}

struct CapturePrivacySnapshot: Equatable, Sendable {
    var window: ActiveWindowResolution
    var isPaused: Bool
    var isStudyExpired: Bool
    var isLocked: Bool
    var isAsleep: Bool
    var hasScreenCapturePermission: Bool
    var secureInputEnabled: Bool
    var policy: CapturePrivacyPolicy
}

enum PrivacyDecision: Equatable, Sendable {
    case allow(ActiveWindowIdentity)
    case deny(CaptureDenial)
}

struct CaptureProofAuthorization: Equatable, Sendable {
    let windowID: CGWindowID
    let expectedTextDigest: String

    func accepts(windowID: CGWindowID, recognizedText: String) -> Bool {
        self.windowID == windowID &&
            expectedTextDigest == Self.digest(recognizedText)
    }

    static func digest(_ text: String) -> String {
        SHA256.hash(data: Data(text.utf8)).map { String(format: "%02x", $0) }.joined()
    }
}

struct CaptureProofToken: Equatable, Sendable {
    fileprivate let value: UUID
}

actor CaptureProofTokenStore {
    static let fixedText = "Open Chronicle capture proof 7F3A-19C2"

    private struct PendingProof {
        let token: UUID
        let windowID: CGWindowID
        let digest: String
    }

    private var pending: PendingProof?

    func mint(forTestWindowID windowID: CGWindowID) -> CaptureProofToken {
        let token = UUID()
        pending = PendingProof(
            token: token,
            windowID: windowID,
            digest: CaptureProofAuthorization.digest(Self.fixedText)
        )
        return CaptureProofToken(value: token)
    }

    /// Consumption happens before any capture decision. A wrong token, wrong window,
    /// denied attempt, or successful attempt all invalidate the sole pending proof.
    func beginAttempt(
        token: CaptureProofToken?,
        windowID: CGWindowID?
    ) -> CaptureProofAuthorization? {
        let attempt = pending
        pending = nil
        guard let attempt,
              let token,
              token.value == attempt.token,
              windowID == attempt.windowID
        else {
            return nil
        }
        return CaptureProofAuthorization(
            windowID: attempt.windowID,
            expectedTextDigest: attempt.digest
        )
    }
}

struct PrivacyEvaluator: Sendable {
    func evaluate(
        snapshot: CapturePrivacySnapshot,
        expectedIdentity: ActiveWindowIdentity?,
        proof: CaptureProofAuthorization?
    ) -> PrivacyDecision {
        if snapshot.isStudyExpired { return .deny(.studyExpired) }
        if snapshot.isPaused { return .deny(.userPaused) }
        if snapshot.isLocked { return .deny(.locked) }
        if snapshot.isAsleep { return .deny(.asleep) }
        if !snapshot.hasScreenCapturePermission { return .deny(.permissionDenied) }
        if snapshot.secureInputEnabled { return .deny(.secureInput) }

        let identity: ActiveWindowIdentity
        switch snapshot.window {
        case let .exact(value): identity = value
        case .noExactWindow: return .deny(.noExactWindow)
        case .ambiguousWindow: return .deny(.ambiguousWindow)
        }

        if let expectedIdentity,
           !identity.hasSameCaptureIdentity(as: expectedIdentity) {
            return .deny(.foregroundChanged)
        }
        if identity.bundleIdentifier == snapshot.policy.chronicleBundleIdentifier {
            guard proof?.windowID == identity.windowID else {
                return .deny(.chronicleSelf)
            }
        } else if snapshot.policy.excludedBundleIdentifiers.contains(
            identity.bundleIdentifier
        ) {
            return .deny(.applicationExcluded)
        }
        if let title = identity.windowTitle,
           snapshot.policy.excludedTitleFragments.contains(where: {
               !($0.isEmpty) && title.localizedCaseInsensitiveContains($0)
           }) {
            return .deny(.titleExcluded)
        }
        return .allow(identity)
    }
}

struct CaptureEnvironmentState: Equatable, Sendable {
    let isPaused: Bool
    let isStudyExpired: Bool
    let isLocked: Bool
    let isAsleep: Bool
    let hasScreenCapturePermission: Bool
    let secureInputEnabled: Bool
}

protocol CaptureEnvironmentProviding: Sendable {
    func currentEnvironment() async -> CaptureEnvironmentState
}

actor SystemCaptureEnvironmentSource: CaptureEnvironmentProviding {
    private var isPaused = false
    private var isStudyExpired = false
    private var isLocked = false
    private var isAsleep = false

    func currentEnvironment() -> CaptureEnvironmentState {
        CaptureEnvironmentState(
            isPaused: isPaused,
            isStudyExpired: isStudyExpired,
            isLocked: isLocked,
            isAsleep: isAsleep,
            hasScreenCapturePermission: CGPreflightScreenCaptureAccess(),
            secureInputEnabled: IsSecureEventInputEnabled()
        )
    }

    func update(
        paused: Bool? = nil,
        studyExpired: Bool? = nil,
        locked: Bool? = nil,
        asleep: Bool? = nil
    ) {
        if let paused { isPaused = paused }
        if let studyExpired { isStudyExpired = studyExpired }
        if let locked { isLocked = locked }
        if let asleep { isAsleep = asleep }
    }
}
