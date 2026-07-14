import Foundation

actor CoreOnboardingConfigurationService {
    private let core: any CoreService

    init(core: any CoreService) {
        self.core = core
    }

    func apply(
        _ configuration: OnboardingRuntimeConfiguration,
        at now: Date = Date()
    ) async throws {
        guard configuration.cadenceSeconds == 30 || configuration.cadenceSeconds == 60,
              [3_600, 86_400, 604_800, 2_592_000]
                .contains(configuration.screenshotRetentionSeconds)
        else {
            throw OnboardingConfigurationError.invalidConfiguration
        }
        if configuration.recordingMode == .study {
            guard configuration.studyStart < configuration.studyEnd,
                  now < configuration.studyEnd
            else {
                throw OnboardingConfigurationError.expiredStudy
            }
        }

        try await call(
            control: [
                "type": "set-cadence",
                "cadence": configuration.cadenceSeconds == 30
                    ? "thirty-seconds"
                    : "sixty-seconds",
            ],
            at: now
        )
        try await call(
            control: [
                "type": "set-screenshot-retention",
                "retention": retentionName(configuration.screenshotRetentionSeconds),
            ],
            at: now
        )
        switch configuration.recordingMode {
        case .personal:
            try await call(control: ["type": "use-personal-mode"], at: now)
        case .study:
            try await call(
                control: [
                    "type": "configure-study",
                    "start": Self.timestamp(configuration.studyStart),
                    "end": Self.timestamp(configuration.studyEnd),
                ],
                at: now
            )
        }
        // Recording becomes eligible only after every other durable choice has
        // been accepted by the Rust-owned configuration.
        try await call(
            control: ["type": "set-recording-preference", "enabled": true],
            at: now
        )
    }

    private func call(control: [String: Any], at date: Date) async throws {
        let request = try JSONSerialization.data(withJSONObject: [
            "schema_version": "1.0",
            "now": Self.timestamp(date),
            "control": control,
        ])
        let response = try await core.call(request)
        let envelope = try JSONDecoder().decode(
            ChronicleEnvelope<OnboardingControlResult>.self,
            from: response
        )
        guard Int(envelope.schemaVersion.split(separator: ".").first ?? "") == 1 else {
            throw ChronicleBridgeError.schemaMismatch(
                expectedMajor: 1,
                actual: envelope.schemaVersion
            )
        }
        guard envelope.ok else {
            throw ChronicleBridgeError.bridgeStatus(1, envelope.error)
        }
    }

    private func retentionName(_ seconds: UInt32) -> String {
        switch seconds {
        case 3_600: "one-hour"
        case 86_400: "twenty-four-hours"
        case 604_800: "seven-days"
        case 2_592_000: "thirty-days"
        default: "twenty-four-hours"
        }
    }

    private static func timestamp(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.string(from: date)
    }
}

private struct OnboardingControlResult: Codable, Sendable {
    let cadence: String?
    let screenshotRetention: String?
    let recordingPreference: Bool?
    let mode: String?

    enum CodingKeys: String, CodingKey {
        case cadence
        case screenshotRetention = "screenshot_retention"
        case recordingPreference = "recording_preference"
        case mode
    }
}

enum OnboardingConfigurationError: LocalizedError {
    case invalidConfiguration
    case expiredStudy

    var errorDescription: String? {
        switch self {
        case .invalidConfiguration:
            "The selected cadence or screenshot retention is unsupported."
        case .expiredStudy:
            "The study end must still be in the future."
        }
    }
}
