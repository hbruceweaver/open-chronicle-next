import Foundation

@MainActor
enum AppRuntimeFactory {
    static func make(
        core: any CoreService,
        defaults: UserDefaults = .standard,
        statusSink: @escaping AppCaptureRuntime.StatusSink
    ) async throws -> AppCaptureRuntime {
        let deviceID = stableDeviceID(defaults: defaults)
        let displayTimezone = TimeZone.current.identifier
        let runtimeControl = CoreAppRuntimeController(
            core: core,
            deviceID: deviceID,
            displayTimezone: displayTimezone
        )
        let configuration = try await runtimeControl.runtimeConfiguration(at: Date())
        let environment = SystemCaptureEnvironmentSource()
        await environment.update(paused: !configuration.recordingEnabled)
        let epoch = CaptureExecutionEpoch()
        let captureControl = CoreCaptureControlClient(
            core: core,
            deviceID: deviceID,
            displayTimezone: displayTimezone
        )
        let pipeline = CaptureAttemptPipeline(
            windowProvider: SystemActiveWindowProvider(),
            environment: environment,
            capturer: ScreenCaptureService(),
            deduplicator: ContentDeduplicator(),
            recognizer: VisionOCRService(),
            proofTokens: CaptureProofTokenStore(),
            ingestor: CoreCaptureIngestor(core: core),
            validity: epoch
        )
        let coordinator = CaptureCoordinator(
            cadenceSeconds: configuration.cadenceSeconds,
            contextSource: SystemCaptureAttemptContextSource(
                deviceID: deviceID,
                cadenceSeconds: configuration.cadenceSeconds,
                retentionSeconds: TimeInterval(configuration.screenshotRetentionSeconds)
            ),
            executor: pipeline,
            admission: captureControl,
            preferences: captureControl,
            storage: captureControl,
            gaps: captureControl,
            epoch: epoch
        )
        return AppCaptureRuntime(
            sessionID: "session-\(UUID().uuidString.lowercased())",
            recordingEnabled: configuration.recordingEnabled,
            control: runtimeControl,
            coordinator: coordinator,
            environment: environment,
            statusSink: statusSink
        )
    }

    static func hasCompletedOnboarding(defaults: UserDefaults = .standard) -> Bool {
        let store = UserDefaultsOnboardingStateStore(defaults: defaults)
        guard case let .restored(state) = store.load(now: Date()) else { return false }
        return state.isComplete &&
            state.currentStep == .completion &&
            state.furthestStepIndex == OnboardingStep.allCases.count - 1 &&
            state.proofState == .passed &&
            state.proofFailure == nil &&
            state.draft.privacyAcknowledged &&
            state.draft.deferAgentSetup
    }

    private static func stableDeviceID(defaults: UserDefaults) -> String {
        let key = "capture.device-id"
        if let existing = defaults.string(forKey: key), !existing.isEmpty {
            return existing
        }
        let created = "device-\(UUID().uuidString.lowercased())"
        defaults.set(created, forKey: key)
        return created
    }
}
