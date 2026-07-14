import Foundation

enum DisclosureContentClass: String, Codable, Sendable {
    case metadata
    case ocr
    case derived
}

struct DisclosureGrantTimeScope: Codable, Equatable, Sendable {
    let type: String
    let seconds: UInt64

    static func rollingHorizon(seconds: UInt64) -> DisclosureGrantTimeScope {
        DisclosureGrantTimeScope(type: "rolling-horizon", seconds: seconds)
    }
}

struct DisclosureGrantLimits: Codable, Equatable, Sendable {
    let maxPageItems: UInt32
    let maxResponseBytes: UInt64
    let maxCumulativeBytes: UInt64

    enum CodingKeys: String, CodingKey {
        case maxPageItems = "max_page_items"
        case maxResponseBytes = "max_response_bytes"
        case maxCumulativeBytes = "max_cumulative_bytes"
    }
}

struct DisclosureGrantRecord: Codable, Equatable, Sendable {
    let schemaVersion: String
    let grantID: String
    let clientID: String
    let receiptID: String
    let timeScope: DisclosureGrantTimeScope
    let contentClasses: [DisclosureContentClass]
    let createdAt: String
    let expiresAt: String
    let state: String
    let limits: DisclosureGrantLimits
    let disclosedBytes: UInt64
    let storeGeneration: UInt64

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case grantID = "grant_id"
        case clientID = "client_id"
        case receiptID = "receipt_id"
        case timeScope = "time_scope"
        case contentClasses = "content_classes"
        case createdAt = "created_at"
        case expiresAt = "expires_at"
        case state
        case limits
        case disclosedBytes = "disclosed_bytes"
        case storeGeneration = "store_generation"
    }
}

enum DisclosureGrantMutation: String, Codable, Sendable {
    case installed
    case alreadyInstalled = "already-installed"
    case revoked
    case alreadyRevoked = "already-revoked"
}

struct DisclosureGrantMutationResponse: Codable, Equatable, Sendable {
    let mutation: DisclosureGrantMutation
    let grant: DisclosureGrantRecord
}

struct DisclosureGrantPolicy: Equatable, Sendable {
    let rollingHorizonSeconds: UInt64
    let expiresAfter: TimeInterval
    let allowOCR: Bool
    let limits: DisclosureGrantLimits

    static let onboardingDefault = DisclosureGrantPolicy(
        rollingHorizonSeconds: 24 * 60 * 60,
        expiresAfter: 7 * 24 * 60 * 60,
        allowOCR: false,
        limits: DisclosureGrantLimits(
            maxPageItems: 50,
            maxResponseBytes: 256 * 1_024,
            maxCumulativeBytes: 64 * 1_024 * 1_024
        )
    )
}

enum DisclosureGrantServiceError: LocalizedError, Equatable {
    case invalidPolicy

    var errorDescription: String? {
        "The disclosure grant policy is not valid."
    }
}

struct CoreDisclosureGrantService: Sendable {
    private let core: any CoreService

    init(core: any CoreService) {
        self.core = core
    }

    func provision(
        for kind: AgentKind,
        policy: DisclosureGrantPolicy = .onboardingDefault,
        now: Date = Date()
    ) async throws -> DisclosureGrantRecord {
        guard policy.rollingHorizonSeconds > 0,
              policy.expiresAfter > 0,
              policy.limits.maxPageItems > 0,
              policy.limits.maxPageItems <= 100,
              policy.limits.maxResponseBytes > 0,
              policy.limits.maxCumulativeBytes >= policy.limits.maxResponseBytes
        else { throw DisclosureGrantServiceError.invalidPolicy }
        let generation = await core.openedStoreGeneration()
        let suffix = UUID().uuidString.lowercased()
        let clientID = "client-open-chronicle-\(kind.rawValue)"
        var classes: [DisclosureContentClass] = [.metadata, .derived]
        if policy.allowOCR { classes.insert(.ocr, at: 1) }
        return DisclosureGrantRecord(
            schemaVersion: "1.0",
            grantID: "grant-\(kind.rawValue)-\(suffix)",
            clientID: clientID,
            receiptID: "receipt-\(kind.rawValue)-\(suffix)",
            timeScope: .rollingHorizon(seconds: policy.rollingHorizonSeconds),
            contentClasses: classes,
            createdAt: Self.timestamp(now),
            expiresAt: Self.timestamp(now.addingTimeInterval(policy.expiresAfter)),
            state: "active",
            limits: policy.limits,
            disclosedBytes: 0,
            storeGeneration: generation
        )
    }

    func install(
        _ grant: DisclosureGrantRecord,
        now: Date = Date()
    ) async throws -> DisclosureGrantMutationResponse {
        let call = DisclosureGrantControlCall(
            schemaVersion: "1.0",
            now: Self.timestamp(now),
            control: InstallDisclosureGrantControl(
                type: "install-disclosure-grant",
                grant: grant
            )
        )
        let response = try await core.call(JSONEncoder().encode(call))
        return try ChronicleFFI.decodeEnvelope(
            DisclosureGrantMutationResponse.self,
            from: response
        ).requireCompatibleMajor()
    }

    func revoke(
        grantID: String,
        clientID: String,
        receiptID: String,
        now: Date = Date()
    ) async throws -> DisclosureGrantMutationResponse {
        let call = DisclosureGrantControlCall(
            schemaVersion: "1.0",
            now: Self.timestamp(now),
            control: RevokeDisclosureGrantControl(
                type: "revoke-disclosure-grant",
                grantID: grantID,
                clientID: clientID,
                receiptID: receiptID
            )
        )
        let response = try await core.call(JSONEncoder().encode(call))
        return try ChronicleFFI.decodeEnvelope(
            DisclosureGrantMutationResponse.self,
            from: response
        ).requireCompatibleMajor()
    }

    private static func timestamp(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        return formatter.string(from: date)
    }
}

private struct DisclosureGrantControlCall<Control: Encodable>: Encodable {
    let schemaVersion: String
    let now: String
    let control: Control

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case now
        case control
    }
}

private struct InstallDisclosureGrantControl: Encodable {
    let type: String
    let grant: DisclosureGrantRecord
}

private struct RevokeDisclosureGrantControl: Encodable {
    let type: String
    let grantID: String
    let clientID: String
    let receiptID: String

    enum CodingKeys: String, CodingKey {
        case type
        case grantID = "grant_id"
        case clientID = "client_id"
        case receiptID = "receipt_id"
    }
}
