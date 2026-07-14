import Foundation

enum SettingsPresentationState: Equatable, Sendable {
    case detached
    case loading
    case loaded
    case failed(String)
}

enum SettingsObservationMode: String, CaseIterable, Identifiable, Sendable {
    case personal
    case study

    var id: String { rawValue }
}

struct SettingsExclusionsSnapshot: Equatable, Sendable {
    let policyVersion: String
    let builtInBundleIdentifiers: [String]
    let builtInTitleFragments: [String]
    let customBundleIdentifiers: [String]
    let customTitleFragments: [String]
    let supportsCustomExclusions: Bool
}

struct SettingsDiagnosticsSnapshot: Equatable, Sendable {
    let projection: DiagnosticProjectionHealth
    let acknowledgement: DiagnosticAcknowledgement
    let managedBytes: UInt64
    let availableBytes: UInt64
    let activeGrantCount: UInt32
    let latestJournalAt: String?
}

struct SettingsRuntimeSnapshot: Equatable, Sendable {
    let recordingEnabled: Bool
    let cadenceSeconds: UInt32
    let screenshotRetentionSeconds: UInt32
    let mode: SettingsObservationMode
    let studyState: DiagnosticStudyState
    let studyStart: Date?
    let studyEnd: Date?
    let launchAtLoginState: LaunchAtLoginState
    let exclusions: SettingsExclusionsSnapshot
    let diagnostics: SettingsDiagnosticsSnapshot
}

enum SettingsServiceError: LocalizedError, Equatable {
    case invalidStudyBoundary
    case unsupportedCadence
    case unsupportedRetention
    case customExclusionsUnavailable
    case launchAtLoginFailed(String)
    case launchAtLoginNotApplied

    var errorDescription: String? {
        switch self {
        case .invalidStudyBoundary:
            "The study end must be later than both its start and the current time."
        case .unsupportedCadence:
            "Choose a supported 30- or 60-second observation cadence."
        case .unsupportedRetention:
            "Choose a supported screenshot retention period."
        case .customExclusionsUnavailable:
            "Custom exclusions are not available in this build. The built-in sensitive-app policy remains active."
        case let .launchAtLoginFailed(message):
            "Launch at login could not be updated. \(message)"
        case .launchAtLoginNotApplied:
            "Launch at login did not reach the requested macOS state."
        }
    }
}

@MainActor
protocol SettingsRuntimeServicing: AnyObject {
    func snapshot(at date: Date) async throws -> SettingsRuntimeSnapshot
    func setRecordingEnabled(_ enabled: Bool) async throws
    func setCadence(seconds: UInt32, at date: Date) async throws
    func setScreenshotRetention(seconds: UInt32, at date: Date) async throws
    func usePersonalMode(at date: Date) async throws
    func configureStudy(start: Date, end: Date, at date: Date) async throws
    func setLaunchAtLogin(_ enabled: Bool) async throws
    func openLaunchAtLoginApproval()
    func updateCustomExclusions(
        bundleIdentifiers: [String],
        titleFragments: [String],
        at date: Date
    ) async throws
}

@MainActor
final class CoreSettingsRuntimeService: SettingsRuntimeServicing {
    typealias RecordingHandler = @MainActor (Bool) async throws -> Void

    private let core: any CoreService
    private let launchAtLogin: LaunchAtLoginService
    private let recordingHandler: RecordingHandler

    init(
        core: any CoreService,
        launchAtLogin: LaunchAtLoginService,
        recordingHandler: @escaping RecordingHandler
    ) {
        self.core = core
        self.launchAtLogin = launchAtLogin
        self.recordingHandler = recordingHandler
    }

    func snapshot(at date: Date = Date()) async throws -> SettingsRuntimeSnapshot {
        let configuration = try await CoreAppRuntimeController(
            core: core,
            deviceID: "settings-read",
            displayTimezone: TimeZone.current.identifier
        ).runtimeConfiguration(at: date)
        let health = try await CoreDiagnosticHealthClient(core: core).fetch(at: date)
        launchAtLogin.refresh()
        return SettingsRuntimeSnapshot(
            recordingEnabled: configuration.recordingEnabled,
            cadenceSeconds: configuration.cadenceSeconds,
            screenshotRetentionSeconds: configuration.screenshotRetentionSeconds,
            mode: health.study.state == .personal ? .personal : .study,
            studyState: health.study.state,
            studyStart: health.study.start.flatMap(ChronicleTimestamp.date),
            studyEnd: health.study.end.flatMap(ChronicleTimestamp.date),
            launchAtLoginState: launchAtLogin.state,
            exclusions: SettingsExclusionsSnapshot(
                policyVersion: CapturePrivacyPolicy.default.policyVersion,
                builtInBundleIdentifiers: CapturePrivacyPolicy.default
                    .excludedBundleIdentifiers.sorted(),
                builtInTitleFragments: CapturePrivacyPolicy.default.excludedTitleFragments,
                customBundleIdentifiers: [],
                customTitleFragments: [],
                supportsCustomExclusions: false
            ),
            diagnostics: SettingsDiagnosticsSnapshot(
                projection: health.projection,
                acknowledgement: health.acknowledgement,
                managedBytes: health.storage.managedBytes,
                availableBytes: health.storage.availableBytes,
                activeGrantCount: health.mcp.activeGrants,
                latestJournalAt: health.latest.lastJournalAt
            )
        )
    }

    func setRecordingEnabled(_ enabled: Bool) async throws {
        try await recordingHandler(enabled)
    }

    func setCadence(seconds: UInt32, at date: Date = Date()) async throws {
        guard seconds == 30 || seconds == 60 else {
            throw SettingsServiceError.unsupportedCadence
        }
        try await control([
            "type": "set-cadence",
            "cadence": seconds == 30 ? "thirty-seconds" : "sixty-seconds",
        ], at: date)
    }

    func setScreenshotRetention(seconds: UInt32, at date: Date = Date()) async throws {
        let value: String
        switch seconds {
        case 3_600: value = "one-hour"
        case 86_400: value = "twenty-four-hours"
        case 604_800: value = "seven-days"
        case 2_592_000: value = "thirty-days"
        default: throw SettingsServiceError.unsupportedRetention
        }
        try await control([
            "type": "set-screenshot-retention",
            "retention": value,
        ], at: date)
    }

    func usePersonalMode(at date: Date = Date()) async throws {
        try await control(["type": "use-personal-mode"], at: date)
    }

    func configureStudy(start: Date, end: Date, at date: Date = Date()) async throws {
        guard start < end, date < end else {
            throw SettingsServiceError.invalidStudyBoundary
        }
        try await control([
            "type": "configure-study",
            "start": ChronicleTimestamp.string(start),
            "end": ChronicleTimestamp.string(end),
        ], at: date)
    }

    func setLaunchAtLogin(_ enabled: Bool) async throws {
        await launchAtLogin.setEnabled(enabled)
        if let message = launchAtLogin.lastError {
            throw SettingsServiceError.launchAtLoginFailed(message)
        }
    }

    func openLaunchAtLoginApproval() {
        launchAtLogin.openApprovalSettings()
    }

    func updateCustomExclusions(
        bundleIdentifiers _: [String],
        titleFragments _: [String],
        at _: Date = Date()
    ) async throws {
        throw SettingsServiceError.customExclusionsUnavailable
    }

    private func control(_ control: [String: Any], at date: Date) async throws {
        let request = try JSONSerialization.data(withJSONObject: [
            "schema_version": "1.0",
            "now": ChronicleTimestamp.string(date),
            "control": control,
        ])
        let response = try await core.call(request)
        let envelope = try JSONDecoder().decode(
            ChronicleEnvelope<TimelineJSONValue>.self,
            from: response
        )
        guard envelope.ok else {
            throw ChronicleBridgeError.bridgeStatus(1, envelope.error)
        }
        _ = try envelope.requireCompatibleMajor()
    }
}

@MainActor
final class SettingsViewModel: ObservableObject {
    @Published private(set) var state: SettingsPresentationState = .detached
    @Published private(set) var snapshot: SettingsRuntimeSnapshot?
    @Published private(set) var isSaving = false
    @Published private(set) var lastError: String?
    @Published private(set) var notice: String?
    @Published private(set) var launchApprovalNotice: String?
    @Published var selectedCadenceSeconds: UInt32 = 60
    @Published var selectedRetentionSeconds: UInt32 = 86_400
    @Published var selectedMode: SettingsObservationMode = .personal
    @Published var studyStart = Date()
    @Published var studyEnd = Date().addingTimeInterval(7 * 24 * 60 * 60)
    let integrations = IntegrationSettingsModel()

    private var runtime: (any SettingsRuntimeServicing)?
    private let now: () -> Date

    init(now: @escaping () -> Date = Date.init) {
        self.now = now
    }

    func attach(
        runtime: any SettingsRuntimeServicing,
        integrations: any SettingsIntegrationManaging
    ) {
        self.runtime = runtime
        self.integrations.attach(service: integrations)
        if state == .detached { state = .loading }
    }

    func detach() {
        runtime = nil
        integrations.detach()
        snapshot = nil
        state = .detached
        lastError = nil
        notice = nil
        launchApprovalNotice = nil
    }

    func load() async {
        guard let runtime else {
            state = .detached
            return
        }
        state = snapshot == nil ? .loading : state
        lastError = nil
        notice = nil
        launchApprovalNotice = nil
        do {
            apply(try await runtime.snapshot(at: now()))
            state = .loaded
        } catch {
            let message = error.localizedDescription
            lastError = message
            state = snapshot == nil ? .failed(message) : .loaded
        }
        await integrations.scan()
    }

    func setRecordingEnabled(_ enabled: Bool) async {
        await mutate(success: enabled ? "Observation resumed." : "Observation paused.") {
            try await self.requireRuntime().setRecordingEnabled(enabled)
        }
    }

    func saveCadence() async {
        await mutate(success: "Cadence saved. It is used after the capture runtime restarts.") {
            try await self.requireRuntime().setCadence(
                seconds: self.selectedCadenceSeconds,
                at: self.now()
            )
        }
    }

    func saveRetention() async {
        await mutate(success: "Screenshot retention saved for new captures after restart.") {
            try await self.requireRuntime().setScreenshotRetention(
                seconds: self.selectedRetentionSeconds,
                at: self.now()
            )
        }
    }

    func saveMode() async {
        await mutate(success: selectedMode == .personal
            ? "Personal mode saved."
            : "Bounded study saved.") {
            let runtime = try self.requireRuntime()
            switch self.selectedMode {
            case .personal:
                try await runtime.usePersonalMode(at: self.now())
            case .study:
                try await runtime.configureStudy(
                    start: self.studyStart,
                    end: self.studyEnd,
                    at: self.now()
                )
            }
        }
    }

    func setLaunchAtLogin(_ enabled: Bool) async {
        guard !isSaving else { return }
        isSaving = true
        lastError = nil
        notice = nil
        launchApprovalNotice = nil
        do {
            let runtime = try requireRuntime()
            try await runtime.setLaunchAtLogin(enabled)
            let refreshed = try await runtime.snapshot(at: now())
            apply(refreshed)
            switch (enabled, refreshed.launchAtLoginState) {
            case (true, .enabled):
                notice = "Launch at login enabled."
            case (true, .requiresApproval):
                launchApprovalNotice =
                    "Approval required. Allow Open Chronicle in System Settings → General → Login Items."
            case (false, .notRegistered):
                notice = "Launch at login disabled."
            default:
                throw SettingsServiceError.launchAtLoginNotApplied
            }
        } catch {
            notice = nil
            lastError = error.localizedDescription
        }
        isSaving = false
    }

    func openLaunchAtLoginApproval() {
        runtime?.openLaunchAtLoginApproval()
    }

    @discardableResult
    func saveCustomExclusions(
        bundleIdentifiers: [String],
        titleFragments: [String]
    ) async -> Bool {
        guard let snapshot, snapshot.exclusions.supportsCustomExclusions else {
            lastError = SettingsServiceError.customExclusionsUnavailable.localizedDescription
            return false
        }
        do {
            try await requireRuntime().updateCustomExclusions(
                bundleIdentifiers: bundleIdentifiers,
                titleFragments: titleFragments,
                at: now()
            )
            await reloadRuntime()
            return true
        } catch {
            lastError = error.localizedDescription
            return false
        }
    }

    private func mutate(
        success: String,
        operation: () async throws -> Void
    ) async {
        guard !isSaving else { return }
        isSaving = true
        lastError = nil
        notice = nil
        launchApprovalNotice = nil
        do {
            try await operation()
            notice = success
            await reloadRuntime()
        } catch {
            lastError = error.localizedDescription
        }
        isSaving = false
    }

    private func reloadRuntime() async {
        guard let runtime else { return }
        do {
            apply(try await runtime.snapshot(at: now()))
            state = .loaded
        } catch {
            notice = nil
            lastError = error.localizedDescription
        }
    }

    private func requireRuntime() throws -> any SettingsRuntimeServicing {
        guard let runtime else { throw AppModelOnboardingError.coreUnavailable }
        return runtime
    }

    private func apply(_ value: SettingsRuntimeSnapshot) {
        snapshot = value
        selectedCadenceSeconds = value.cadenceSeconds
        selectedRetentionSeconds = value.screenshotRetentionSeconds
        selectedMode = value.mode
        studyStart = value.studyStart ?? now()
        studyEnd = value.studyEnd ?? now().addingTimeInterval(7 * 24 * 60 * 60)
        lastError = nil
    }
}
