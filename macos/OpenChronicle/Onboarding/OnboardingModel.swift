import Foundation

enum OnboardingStep: String, Codable, CaseIterable, Identifiable, Sendable {
    case welcome
    case mode
    case privacy
    case permission
    case captureProof = "capture-proof"
    case login
    case agent
    case completion

    var id: String { rawValue }

    var title: String {
        switch self {
        case .welcome: "Welcome"
        case .mode: "Observation mode"
        case .privacy: "Privacy"
        case .permission: "Screen Recording"
        case .captureProof: "Capture test"
        case .login: "Keep it running"
        case .agent: "AI access"
        case .completion: "Ready"
        }
    }
}

enum OnboardingRecordingMode: String, Codable, CaseIterable, Identifiable, Sendable {
    case personal
    case study

    var id: String { rawValue }
}

enum OnboardingCaptureProofState: String, Codable, Equatable, Sendable {
    case notRun = "not-run"
    case running
    case passed
    case failed
}

enum OnboardingCaptureProofFailure: String, Codable, Equatable, Sendable {
    case permissionDenied = "permission-denied"
    case proofWindowUnavailable = "proof-window-unavailable"
    case exactWindowUnavailable = "exact-window-unavailable"
    case foregroundChanged = "foreground-changed"
    case textMismatch = "text-mismatch"
    case captureFailed = "capture-failed"

    var message: String {
        switch self {
        case .permissionDenied:
            "Screen Recording permission is not currently available."
        case .proofWindowUnavailable:
            "Open Chronicle could not create its private test window."
        case .exactWindowUnavailable:
            "macOS did not expose the exact test window for capture."
        case .foregroundChanged:
            "The foreground window changed during the test. Keep Open Chronicle in front and retry."
        case .textMismatch:
            "The local OCR result did not match the synthetic test phrase."
        case .captureFailed:
            "The exact-window capture or local OCR test failed."
        }
    }
}

enum OnboardingCaptureProofResult: Equatable, Sendable {
    case passed
    case failed(OnboardingCaptureProofFailure)
}

@MainActor
protocol OnboardingCaptureProofRunning: AnyObject {
    func run() async -> OnboardingCaptureProofResult
}

struct OnboardingDraft: Codable, Equatable, Sendable {
    var recordingMode: OnboardingRecordingMode
    var cadenceSeconds: UInt32
    var screenshotRetentionSeconds: UInt32
    var studyStart: Date
    var studyEnd: Date
    var privacyAcknowledged: Bool
    var launchAtLogin: Bool
    var deferAgentSetup: Bool

    static func fresh(now: Date) -> OnboardingDraft {
        OnboardingDraft(
            recordingMode: .personal,
            cadenceSeconds: 60,
            screenshotRetentionSeconds: 24 * 60 * 60,
            studyStart: now,
            studyEnd: now.addingTimeInterval(30 * 24 * 60 * 60),
            privacyAcknowledged: false,
            launchAtLogin: true,
            deferAgentSetup: true
        )
    }

    func isModeValid(at now: Date) -> Bool {
        guard isStructurallyValid else { return false }
        switch recordingMode {
        case .personal:
            return true
        case .study:
            return studyStart < studyEnd && now < studyEnd
        }
    }

    var isStructurallyValid: Bool {
        guard cadenceSeconds == 30 || cadenceSeconds == 60 else { return false }
        guard [3_600, 86_400, 604_800, 2_592_000].contains(screenshotRetentionSeconds)
        else { return false }
        return recordingMode == .personal || studyStart < studyEnd
    }
}

struct OnboardingRuntimeConfiguration: Equatable, Sendable {
    let recordingMode: OnboardingRecordingMode
    let cadenceSeconds: UInt32
    let screenshotRetentionSeconds: UInt32
    let studyStart: Date
    let studyEnd: Date
}

struct OnboardingPersistedState: Codable, Equatable, Sendable {
    static let schemaVersion = 1

    var schemaVersion: Int
    var currentStep: OnboardingStep
    var furthestStepIndex: Int
    var draft: OnboardingDraft
    var proofState: OnboardingCaptureProofState
    var proofFailure: OnboardingCaptureProofFailure?
    var isComplete: Bool

    static func fresh(now: Date) -> OnboardingPersistedState {
        OnboardingPersistedState(
            schemaVersion: schemaVersion,
            currentStep: .welcome,
            furthestStepIndex: 0,
            draft: .fresh(now: now),
            proofState: .notRun,
            proofFailure: nil,
            isComplete: false
        )
    }

    func integrityIssue() -> String? {
        guard schemaVersion == Self.schemaVersion else {
            return "Saved setup progress uses an unsupported schema version."
        }
        let stepIndex = OnboardingStep.allCases.firstIndex(of: currentStep) ?? 0
        guard furthestStepIndex >= 0,
              furthestStepIndex < OnboardingStep.allCases.count,
              stepIndex <= furthestStepIndex,
              draft.isStructurallyValid,
              proofState != .passed || proofFailure == nil
        else {
            return "Saved setup progress contains invalid or out-of-order state."
        }
        let reachedCompletion = currentStep == .completion || isComplete
        if reachedCompletion,
           (proofState != .passed ||
               proofFailure != nil ||
               !draft.privacyAcknowledged ||
               !draft.deferAgentSetup ||
               furthestStepIndex != OnboardingStep.allCases.count - 1)
        {
            return "Saved setup completion is missing required privacy or capture proof."
        }
        if isComplete, currentStep != .completion {
            return "Saved setup completion contains an inconsistent terminal step."
        }
        return nil
    }
}

@MainActor
protocol OnboardingStateStoring: AnyObject {
    func load(now: Date) -> OnboardingLoadResult
    func save(_ state: OnboardingPersistedState)
}

enum OnboardingLoadResult: Equatable, Sendable {
    case restored(OnboardingPersistedState)
    case repairRequired(String)
}

@MainActor
final class UserDefaultsOnboardingStateStore: OnboardingStateStoring {
    static let stateKey = "onboarding.state.v1"
    static let completionKey = "onboarding.completed"

    private let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    func load(now: Date) -> OnboardingLoadResult {
        guard let data = defaults.data(forKey: Self.stateKey) else {
            if defaults.bool(forKey: Self.completionKey) {
                return .repairRequired(
                    "A legacy completion marker exists without the capture-proof record required by this version."
                )
            }
            return .restored(.fresh(now: now))
        }
        guard let decoded = try? JSONDecoder().decode(OnboardingPersistedState.self, from: data)
        else {
            return .repairRequired("Saved setup progress could not be decoded safely.")
        }
        if let issue = decoded.integrityIssue() {
            return .repairRequired(issue)
        }

        var restored = decoded
        if restored.proofState == .running {
            restored.proofState = .notRun
            restored.proofFailure = nil
        }
        return .restored(restored)
    }

    func save(_ state: OnboardingPersistedState) {
        guard let data = try? JSONEncoder().encode(state) else {
            defaults.set(false, forKey: Self.completionKey)
            return
        }
        defaults.set(data, forKey: Self.stateKey)
        defaults.set(state.isComplete, forKey: Self.completionKey)
    }
}

@MainActor
final class OnboardingModel: ObservableObject {
    typealias FinishHandler = @MainActor (OnboardingRuntimeConfiguration) async throws -> Void
    typealias LaunchPreferenceHandler = @MainActor (Bool) async -> String?

    @Published private(set) var currentStep: OnboardingStep
    @Published private(set) var furthestStepIndex: Int
    @Published var draft: OnboardingDraft {
        didSet { persist() }
    }
    @Published private(set) var proofState: OnboardingCaptureProofState
    @Published private(set) var proofFailure: OnboardingCaptureProofFailure?
    @Published private(set) var permissionGranted: Bool
    @Published private(set) var isComplete: Bool
    @Published private(set) var isFinishing = false
    @Published private(set) var finishError: String?
    @Published private(set) var nonBlockingWarning: String?
    @Published private(set) var restoreIssue: String?
    let agentSetup: AgentSetupModel

    private let store: any OnboardingStateStoring
    private let permission: any ScreenRecordingPermissionServicing
    private let proofRunner: any OnboardingCaptureProofRunning
    private let now: () -> Date
    private let finishHandler: FinishHandler
    private let launchPreferenceHandler: LaunchPreferenceHandler
    private var isInitializing = true

    init(
        store: (any OnboardingStateStoring)? = nil,
        permission: (any ScreenRecordingPermissionServicing)? = nil,
        proofRunner: (any OnboardingCaptureProofRunning)? = nil,
        agentSetup: AgentSetupModel? = nil,
        now: @escaping () -> Date = Date.init,
        finishHandler: @escaping FinishHandler = { _ in
            throw OnboardingModelError.completionUnavailable
        },
        launchPreferenceHandler: @escaping LaunchPreferenceHandler = { _ in nil }
    ) {
        let resolvedStore = store ?? UserDefaultsOnboardingStateStore()
        let resolvedPermission = permission ?? ScreenRecordingPermissionService()
        let resolvedProofRunner = proofRunner ?? CaptureProofService()
        let loadResult = resolvedStore.load(now: now())
        let restored: OnboardingPersistedState
        let restoreIssue: String?
        switch loadResult {
        case let .restored(value):
            if let issue = value.integrityIssue() {
                restored = .fresh(now: now())
                restoreIssue = issue
            } else {
                restored = value
                restoreIssue = nil
            }
        case let .repairRequired(message):
            restored = .fresh(now: now())
            restoreIssue = message
        }
        self.store = resolvedStore
        self.permission = resolvedPermission
        self.proofRunner = resolvedProofRunner
        self.agentSetup = agentSetup ?? AgentSetupModel()
        self.now = now
        self.finishHandler = finishHandler
        self.launchPreferenceHandler = launchPreferenceHandler
        currentStep = restored.currentStep
        furthestStepIndex = restored.furthestStepIndex
        draft = restored.draft
        proofState = restored.proofState
        proofFailure = restored.proofFailure
        permissionGranted = resolvedPermission.isGranted()
        isComplete = restored.isComplete
        self.restoreIssue = restoreIssue
        isInitializing = false
        if !permissionGranted, !isComplete, proofState == .passed {
            proofState = .notRun
            proofFailure = .permissionDenied
            persist()
        }
    }

    var currentStepIndex: Int {
        Self.index(of: currentStep)
    }

    var canGoBack: Bool {
        currentStepIndex > 0 && !isFinishing
    }

    var canAdvance: Bool {
        guard restoreIssue == nil, !isFinishing, !isComplete else { return false }
        switch currentStep {
        case .welcome:
            return true
        case .mode:
            return draft.isModeValid(at: now())
        case .privacy:
            return draft.privacyAcknowledged
        case .permission:
            return permissionGranted
        case .captureProof:
            return proofState == .passed && proofFailure == nil
        case .login:
            return true
        case .agent:
            return draft.deferAgentSetup
        case .completion:
            return proofState == .passed &&
                proofFailure == nil &&
                permissionGranted &&
                draft.privacyAcknowledged &&
                draft.deferAgentSetup &&
                draft.isModeValid(at: now())
        }
    }

    func canNavigate(to step: OnboardingStep) -> Bool {
        restoreIssue == nil &&
            !isComplete &&
            Self.index(of: step) <= furthestStepIndex &&
            !isFinishing
    }

    func navigate(to step: OnboardingStep) {
        guard canNavigate(to: step) else { return }
        currentStep = step
        refreshPermissionIfNeeded()
        persist()
    }

    @discardableResult
    func advance() -> Bool {
        guard canAdvance,
              currentStep != .completion,
              currentStepIndex + 1 < OnboardingStep.allCases.count
        else { return false }
        let nextIndex = currentStepIndex + 1
        furthestStepIndex = max(furthestStepIndex, nextIndex)
        currentStep = OnboardingStep.allCases[nextIndex]
        refreshPermissionIfNeeded()
        persist()
        return true
    }

    func goBack() {
        guard canGoBack else { return }
        currentStep = OnboardingStep.allCases[currentStepIndex - 1]
        refreshPermissionIfNeeded()
        persist()
    }

    func refreshPermission() {
        permissionGranted = permission.isGranted()
        if !permissionGranted, !isComplete, proofState == .passed {
            proofState = .notRun
            proofFailure = .permissionDenied
        }
        persist()
    }

    func requestPermission() {
        _ = permission.request()
        refreshPermission()
    }

    func openPermissionSettings() {
        permission.openSystemSettings()
    }

    func runCaptureProof() async {
        guard !isFinishing else { return }
        refreshPermission()
        guard permissionGranted else {
            proofState = .failed
            proofFailure = .permissionDenied
            persist()
            return
        }
        proofState = .running
        proofFailure = nil
        persist()
        let result = await proofRunner.run()
        switch result {
        case .passed:
            proofState = .passed
            proofFailure = nil
        case let .failed(failure):
            proofState = .failed
            proofFailure = failure
        }
        persist()
    }

    func finish() async {
        refreshPermission()
        guard currentStep == .completion, canAdvance else { return }
        isFinishing = true
        finishError = nil
        nonBlockingWarning = nil
        defer { isFinishing = false }

        nonBlockingWarning = await launchPreferenceHandler(draft.launchAtLogin)
        do {
            try await finishHandler(runtimeConfiguration)
            isComplete = true
            furthestStepIndex = OnboardingStep.allCases.count - 1
            persist()
        } catch {
            finishError = error.localizedDescription
        }
    }

    func restartSetupProgress() {
        let fresh = OnboardingPersistedState.fresh(now: now())
        currentStep = fresh.currentStep
        furthestStepIndex = fresh.furthestStepIndex
        draft = fresh.draft
        proofState = fresh.proofState
        proofFailure = fresh.proofFailure
        isComplete = false
        finishError = nil
        nonBlockingWarning = nil
        restoreIssue = nil
        permissionGranted = permission.isGranted()
        persist()
    }

    var runtimeConfiguration: OnboardingRuntimeConfiguration {
        OnboardingRuntimeConfiguration(
            recordingMode: draft.recordingMode,
            cadenceSeconds: draft.cadenceSeconds,
            screenshotRetentionSeconds: draft.screenshotRetentionSeconds,
            studyStart: draft.studyStart,
            studyEnd: draft.studyEnd
        )
    }

    private func refreshPermissionIfNeeded() {
        if currentStep == .permission || currentStep == .captureProof || currentStep == .completion {
            refreshPermission()
        }
    }

    private func persist() {
        guard !isInitializing else { return }
        store.save(
            OnboardingPersistedState(
                schemaVersion: OnboardingPersistedState.schemaVersion,
                currentStep: currentStep,
                furthestStepIndex: furthestStepIndex,
                draft: draft,
                proofState: proofState == .running ? .notRun : proofState,
                proofFailure: proofFailure,
                isComplete: isComplete
            )
        )
    }

    private static func index(of step: OnboardingStep) -> Int {
        OnboardingStep.allCases.firstIndex(of: step) ?? 0
    }
}

enum OnboardingModelError: LocalizedError {
    case completionUnavailable

    var errorDescription: String? {
        switch self {
        case .completionUnavailable:
            "Open Chronicle is not ready to finish setup yet."
        }
    }
}
