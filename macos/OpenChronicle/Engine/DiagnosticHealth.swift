import Foundation

enum DiagnosticProjectionHealth: String, Codable, Equatable, Sendable {
    case current
    case lagging
    case rebuilding
    case blocked
}

enum DiagnosticAcknowledgement: String, Codable, Equatable, Sendable {
    case durable
    case journalDurableProjectionPending = "journal-durable-projection-pending"
    case notDurable = "not-durable"
}

enum DiagnosticHealthSeverity: String, Codable, Equatable, Sendable {
    case info
    case warning
    case critical
}

enum DiagnosticHealthCode: String, Codable, Equatable, Sendable {
    case healthy
    case projectionLag = "projection-lag"
    case storageUnavailable = "storage-unavailable"
    case corruptCanonicalRecord = "corrupt-canonical-record"
    case permissionDenied = "permission-denied"
    case captureUnavailable = "capture-unavailable"
    case studyExpired = "study-expired"
}

enum DiagnosticStudyState: String, Codable, Equatable, Sendable {
    case personal
    case scheduled
    case active
    case expired
}

struct DiagnosticOperationTimes: Codable, Equatable, Sendable {
    let lastScheduledAttemptAt: String?
    let lastSuccessfulCaptureAt: String?
    let lastSuccessfulOCRAt: String?
    let lastJournalAt: String?
    let lastProjectionAt: String?
    let lastChunkAt: String?

    enum CodingKeys: String, CodingKey {
        case lastScheduledAttemptAt = "last_scheduled_attempt_at"
        case lastSuccessfulCaptureAt = "last_successful_capture_at"
        case lastSuccessfulOCRAt = "last_successful_ocr_at"
        case lastJournalAt = "last_journal_at"
        case lastProjectionAt = "last_projection_at"
        case lastChunkAt = "last_chunk_at"
    }
}

struct DiagnosticStorageSummary: Codable, Equatable, Sendable {
    let managedBytes: UInt64
    let availableBytes: UInt64

    enum CodingKeys: String, CodingKey {
        case managedBytes = "managed_bytes"
        case availableBytes = "available_bytes"
    }
}

enum DiagnosticScreenshotStorageState: String, Codable, Equatable, Sendable {
    case healthy
    case warning
    case blockedFreeSpace = "blocked-free-space"
    case blockedImageQuota = "blocked-image-quota"
}

struct DiagnosticScreenshotStorageSummary: Codable, Equatable, Sendable {
    let managedImageBytes: UInt64
    let availableBytes: UInt64
    let warningFreeBytes: UInt64
    let minimumFreeBytes: UInt64
    let managedImageQuotaBytes: UInt64
    let journalReserveBytes: UInt64
    let state: DiagnosticScreenshotStorageState

    enum CodingKeys: String, CodingKey {
        case managedImageBytes = "managed_image_bytes"
        case availableBytes = "available_bytes"
        case warningFreeBytes = "warning_free_bytes"
        case minimumFreeBytes = "minimum_free_bytes"
        case managedImageQuotaBytes = "managed_image_quota_bytes"
        case journalReserveBytes = "journal_reserve_bytes"
        case state
    }
}

struct DiagnosticStudySummary: Codable, Equatable, Sendable {
    let state: DiagnosticStudyState
    let start: String?
    let end: String?
    let expiredAt: String?

    enum CodingKeys: String, CodingKey {
        case state
        case start
        case end
        case expiredAt = "expired_at"
    }

    var endDate: Date? {
        end.flatMap(ChronicleTimestamp.date)
    }
}

struct DiagnosticScreenshotRetentionSummary: Codable, Equatable, Sendable {
    let writePending: UInt64
    let retained: UInt64
    let deletePending: UInt64
    let expired: UInt64
    let userDeleted: UInt64
    let missing: UInt64
    let writeFailed: UInt64
    let nextExpiryAt: String?

    enum CodingKeys: String, CodingKey {
        case writePending = "write_pending"
        case retained
        case deletePending = "delete_pending"
        case expired
        case userDeleted = "user_deleted"
        case missing
        case writeFailed = "write_failed"
        case nextExpiryAt = "next_expiry_at"
    }
}

struct DiagnosticMCPHealthSummary: Codable, Equatable, Sendable {
    let activeGrants: UInt32
    let revokedGrants: UInt32
    let expiredGrants: UInt32
    let exhaustedGrants: UInt32
    let staleGenerationGrants: UInt32

    enum CodingKeys: String, CodingKey {
        case activeGrants = "active_grants"
        case revokedGrants = "revoked_grants"
        case expiredGrants = "expired_grants"
        case exhaustedGrants = "exhausted_grants"
        case staleGenerationGrants = "stale_generation_grants"
    }
}

struct DiagnosticHealthIssue: Codable, Equatable, Sendable {
    let severity: DiagnosticHealthSeverity
    let code: DiagnosticHealthCode
}

struct DiagnosticHealthSnapshot: Codable, Equatable, Sendable {
    let schemaVersion: String
    let observedAt: String
    let storeGeneration: UInt64
    let projection: DiagnosticProjectionHealth
    let acknowledgement: DiagnosticAcknowledgement
    let latest: DiagnosticOperationTimes
    let aggregationWatermark: String?
    let aggregationPendingBuckets: UInt64
    let projectionLagSeconds: UInt64
    let projectionPendingRecords: UInt64
    let storage: DiagnosticStorageSummary
    let study: DiagnosticStudySummary
    let screenshotRetention: DiagnosticScreenshotRetentionSummary
    let mcp: DiagnosticMCPHealthSummary
    let issues: [DiagnosticHealthIssue]
    var screenshotStorage: DiagnosticScreenshotStorageSummary? = nil

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case observedAt = "observed_at"
        case storeGeneration = "store_generation"
        case projection
        case acknowledgement
        case latest
        case aggregationWatermark = "aggregation_watermark"
        case aggregationPendingBuckets = "aggregation_pending_buckets"
        case projectionLagSeconds = "projection_lag_seconds"
        case projectionPendingRecords = "projection_pending_records"
        case storage
        case study
        case screenshotRetention = "screenshot_retention"
        case mcp
        case issues
    }
}

private struct SharedHealthResponse: Codable, Sendable {
    let schemaVersion: String
    let requestID: String
    let generatedAt: String
    let storeGeneration: UInt64
    let result: SharedHealthResult

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case requestID = "request_id"
        case generatedAt = "generated_at"
        case storeGeneration = "store_generation"
        case result
    }
}

private struct SharedHealthResult: Codable, Sendable {
    let type: String
    let data: DiagnosticHealthSnapshot
}

protocol DiagnosticHealthFetching: Sendable {
    func fetch(at date: Date) async throws -> DiagnosticHealthSnapshot
}

actor CoreDiagnosticHealthClient: DiagnosticHealthFetching {
    private let core: any CoreService

    init(core: any CoreService) {
        self.core = core
    }

    func fetch(at date: Date = Date()) async throws -> DiagnosticHealthSnapshot {
        let generation = await core.openedStoreGeneration()
        let requestID = "app-health-\(UUID().uuidString.lowercased())"
        let request = try JSONSerialization.data(withJSONObject: [
            "schema_version": "1.0",
            "now": ChronicleTimestamp.string(date),
            "request": [
                "schema_version": "1.0",
                "request_id": requestID,
                "store_generation": NSNumber(value: generation),
                "operation": ["type": "health"],
            ],
        ])
        let response = try await core.call(request)
        let envelope = try JSONDecoder().decode(
            ChronicleEnvelope<SharedHealthResponse>.self,
            from: response
        )
        let shared = try envelope.requireCompatibleMajor()
        guard shared.schemaVersion.hasPrefix("1."),
              shared.requestID == requestID,
              shared.storeGeneration == generation,
              shared.result.type == "health",
              shared.result.data.schemaVersion.hasPrefix("1."),
              shared.result.data.storeGeneration == generation,
              shared.result.data.observedAt == shared.generatedAt
        else {
            throw ChronicleBridgeError.malformedResponse
        }
        let storageRequest = try JSONSerialization.data(withJSONObject: [
            "schema_version": "1.0",
            "now": ChronicleTimestamp.string(date),
            "control": ["type": "storage-health"],
        ])
        let storageResponse = try await core.call(storageRequest)
        let storageEnvelope = try JSONDecoder().decode(
            ChronicleEnvelope<DiagnosticScreenshotStorageSummary>.self,
            from: storageResponse
        )
        var snapshot = shared.result.data
        snapshot.screenshotStorage = try storageEnvelope.requireCompatibleMajor()
        return snapshot
    }
}

enum ChronicleTimestamp {
    static func string(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.string(from: date)
    }

    static func date(_ value: String) -> Date? {
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let date = fractional.date(from: value) {
            return date
        }
        return ISO8601DateFormatter().date(from: value)
    }
}
